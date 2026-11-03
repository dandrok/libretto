//! Playing sentences: a `Player` trait, an mpv implementation, and the loop that
//! keeps one sentence ahead of the speaker.
//!
//! The loop is library code with the player injected, so playback is testable
//! without an audio device — the binary's only job is to hand it mpv.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

use thiserror::Error;

use crate::cache::{Cache, CacheError};
use crate::tts::{SpeechRequest, TtsEngine, TtsError};

/// How far ahead the synthesizer may run. Synthesis is 4–5× faster than speech,
/// so a small lead is enough and it bounds memory and cache churn.
const DEFAULT_LEAD: usize = 5;

/// How many unusable sentences in a row before the endpoint, not the text, is to blame.
const MAX_SILENT_SENTENCES: usize = 5;

#[derive(Debug, Error)]
pub enum PlayError {
    #[error("cannot start the player '{program}': {reason}")]
    Spawn { program: String, reason: String },
    #[error("the player '{program}' exited with {status}")]
    Exit { program: String, status: String },
    #[error(transparent)]
    Cache(#[from] CacheError),
}

/// Anything that can play one file to completion.
pub trait Player {
    fn play(&self, file: &Path) -> Result<(), PlayError>;
}

/// The system player. argv only — never a shell string, and the program name is
/// resolved from `PATH` so `--player ffplay` works.
pub struct Mpv {
    program: String,
}

impl Mpv {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Player for Mpv {
    fn play(&self, file: &Path) -> Result<(), PlayError> {
        let status = Command::new(&self.program)
            .arg("--no-video")
            .arg("--really-quiet")
            .arg(file)
            .status()
            .map_err(|error| PlayError::Spawn {
                program: self.program.clone(),
                reason: error.to_string(),
            })?;
        if !status.success() {
            return Err(PlayError::Exit {
                program: self.program.clone(),
                status: status.to_string(),
            });
        }
        Ok(())
    }
}

/// Result of a playback run, so the caller can persist the position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    /// Index of the next sentence to speak.
    pub next: usize,
    /// How many sentences were actually played.
    pub count: usize,
    /// How many came from the cache (the rest were synthesized).
    pub cache_hits: usize,
    /// Sentences the engine could not voice, and which were stepped over.
    pub skipped: usize,
}

/// The collaborators one playback run needs. Grouped so the function below keeps
/// a readable signature.
pub struct Session<'a> {
    pub engine: &'a dyn TtsEngine,
    pub player: &'a dyn Player,
    pub cache: &'a Cache,
    /// Voice, speed and model; `text` is replaced per sentence.
    pub template: &'a SpeechRequest,
}

/// Play `sentences[start..]`, stopping after `limit` sentences if given.
///
/// `on_played` is called with the index of the next sentence after each one
/// finishes, which is where the caller persists progress: a killed reader loses
/// at most one sentence.
pub fn play_sentences(
    session: &Session<'_>,
    sentences: &[String],
    start: usize,
    limit: Option<usize>,
    mut on_played: impl FnMut(usize),
) -> Result<Played, PlayError> {
    let engine = session.engine;
    let player = session.player;
    let cache = session.cache;
    let template = session.template;
    let total = sentences.len();
    let begin = start.min(total);
    let end = match limit {
        Some(limit) => begin.saturating_add(limit).min(total),
        None => total,
    };
    if begin >= end {
        return Ok(Played {
            next: begin,
            count: 0,
            cache_hits: 0,
            skipped: 0,
        });
    }

    let mut played = 0usize;
    let mut cache_hits = 0usize;
    let mut skipped = 0usize;
    let mut silent_run = 0usize;
    let mut next = begin;

    std::thread::scope(|scope| -> Result<(), PlayError> {
        // Bounded so a fast synthesizer cannot run away with memory.
        let (tx, rx) =
            mpsc::sync_channel::<(usize, bool, Result<PathBuf, CacheError>)>(DEFAULT_LEAD);
        let worker = scope.spawn(move || {
            for (index, text) in sentences.iter().enumerate().take(end).skip(begin) {
                let mut request = template.clone();
                request.text = text.clone();
                let key = Cache::key(&request);
                let hit = cache.get(&key).is_some();
                let result = cache.audio_for(engine, &request);
                // The receiver is gone only when playback already failed.
                if tx.send((index, hit, result)).is_err() {
                    break;
                }
            }
        });

        for (index, hit, result) in rx {
            let path = match result {
                Ok(path) => {
                    silent_run = 0;
                    path
                }
                // One fragment the engine cannot voice is not a broken book: skip it and
                // keep reading. A run of them means the endpoint is the problem, so the
                // error still surfaces instead of an hour of silence.
                Err(CacheError::Tts(TtsError::Empty { .. }))
                    if silent_run < MAX_SILENT_SENTENCES =>
                {
                    silent_run += 1;
                    skipped += 1;
                    next = index + 1;
                    on_played(next);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            player.play(&path)?;
            if hit {
                cache_hits += 1;
            }
            played += 1;
            next = index + 1;
            on_played(next);
        }
        let _ = worker.join();
        Ok(())
    })?;

    Ok(Played {
        next,
        count: played,
        cache_hits,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::tts::TtsError;

    struct RecordingPlayer {
        played: RefCell<Vec<PathBuf>>,
        fail_after: Option<usize>,
    }

    impl Player for RecordingPlayer {
        fn play(&self, file: &Path) -> Result<(), PlayError> {
            let mut played = self.played.borrow_mut();
            played.push(file.to_path_buf());
            match self.fail_after {
                Some(limit) if played.len() > limit => Err(PlayError::Exit {
                    program: "recording".into(),
                    status: "failed on purpose".into(),
                }),
                _ => Ok(()),
            }
        }
    }

    /// One byte per sentence and a call counter, so cache behaviour is visible
    /// without a network.
    struct CountingEngine {
        calls: AtomicUsize,
    }

    impl CountingEngine {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl TtsEngine for CountingEngine {
        fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>, TtsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(request.text.clone().into_bytes())
        }
    }

    /// Answers with an empty body for any sentence containing `needle`, the way a real
    /// engine answers a fragment with nothing to pronounce.
    struct SilentFor {
        needle: String,
    }

    impl TtsEngine for SilentFor {
        fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>, TtsError> {
            if request.text.contains(&self.needle) {
                return Err(TtsError::Empty {
                    endpoint: "fake".into(),
                });
            }
            Ok(request.text.clone().into_bytes())
        }
    }

    fn recording_player(fail_after: Option<usize>) -> RecordingPlayer {
        RecordingPlayer {
            played: RefCell::new(Vec::new()),
            fail_after,
        }
    }

    fn sentences(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("Sentence {i}.")).collect()
    }

    #[test]
    fn plays_the_requested_range_and_reports_the_next_index() {
        let engine = CountingEngine::new();
        let player = recording_player(None);
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
        };
        let mut positions = Vec::new();

        let result = play_sentences(&session, &sentences(4), 1, Some(2), |next| {
            positions.push(next)
        })
        .expect("plays");

        assert_eq!(result.count, 2);
        assert_eq!(result.next, 3);
        assert_eq!(positions, vec![2, 3], "progress after each sentence");
        assert_eq!(engine.calls(), 2);
        assert_eq!(player.played.borrow().len(), 2);
    }

    #[test]
    fn a_second_run_is_all_cache_hits() {
        let engine = CountingEngine::new();
        let player = recording_player(None);
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
        };

        play_sentences(&session, &sentences(3), 0, None, |_| {}).expect("first");
        let second = play_sentences(&session, &sentences(3), 0, None, |_| {}).expect("second");

        assert_eq!(engine.calls(), 3, "synthesized once per sentence");
        assert_eq!(second.cache_hits, 3, "the second pass is all hits");
    }

    #[test]
    fn a_start_past_the_end_is_not_an_error() {
        let engine = CountingEngine::new();
        let player = recording_player(None);
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
        };

        let result = play_sentences(&session, &sentences(2), 9, None, |_| {}).expect("no work");

        assert_eq!(result.count, 0);
        assert_eq!(result.next, 2, "clamped to the end, not past it");
        assert_eq!(engine.calls(), 0, "nothing to synthesize");
    }

    #[test]
    fn a_failing_player_stops_the_run_and_reports_the_error() {
        let engine = CountingEngine::new();
        let player = recording_player(Some(1));
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
        };
        let mut positions = Vec::new();

        let error = play_sentences(&session, &sentences(5), 0, None, |next| {
            positions.push(next)
        })
        .expect_err("the player failed");

        assert!(matches!(error, PlayError::Exit { .. }), "got {error}");
        assert_eq!(
            positions,
            vec![1],
            "only the finished sentence was reported"
        );
    }

    #[test]
    fn one_unvoiceable_fragment_is_stepped_over_instead_of_ending_the_book() {
        let engine = SilentFor {
            needle: "noise".into(),
        };
        let player = recording_player(None);
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
        };
        let lines = vec![
            "Sentence 0.".to_string(),
            "noise only".to_string(),
            "Sentence 2.".to_string(),
        ];
        let mut positions = Vec::new();

        let result = play_sentences(&session, &lines, 0, None, |next| positions.push(next))
            .expect("the book continues");

        assert_eq!(result.count, 2, "two sentences were played");
        assert_eq!(result.skipped, 1, "the fragment was stepped over");
        assert_eq!(result.next, 3, "and the position moved past it");
        assert_eq!(positions, vec![1, 2, 3], "{positions:?}");
        assert_eq!(player.played.borrow().len(), 2);
    }

    #[test]
    fn a_run_of_silent_replies_still_reports_the_endpoint_as_broken() {
        let engine = SilentFor {
            needle: "Sentence".into(),
        };
        let player = recording_player(None);
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
        };

        let error = play_sentences(&session, &sentences(20), 0, None, |_| {})
            .expect_err("silence everywhere is a broken endpoint");

        assert!(
            matches!(
                error,
                PlayError::Cache(CacheError::Tts(TtsError::Empty { .. }))
            ),
            "got {error}"
        );
    }
}
