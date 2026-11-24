//! Playing sentences: a `Player` trait, an mpv implementation, and the loop that
//! keeps one sentence ahead of the speaker.
//!
//! The loop is library code with the player injected, so playback is testable
//! without an audio device — the binary's only job is to hand it mpv.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::cache::{Cache, CacheError};
use crate::captions::Word;
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

/// Where a player is inside the file it is playing.
///
/// The read-along draws against this rather than a wall clock, because a paused player
/// keeps its process alive: only the player knows the audio stopped, and `seconds` then
/// stands still on its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub seconds: f64,
    pub paused: bool,
}

/// How often the player is asked where it is. A word lasts 150–400 ms; sampling four times
/// inside the shortest one keeps the quantisation well under a syllable, and two local reads
/// cost nothing worth counting.
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(20);

/// Anything that can play one file to completion.
pub trait Player {
    fn play(&self, file: &Path) -> Result<(), PlayError>;
}

/// The system player. argv only — never a shell string, and the program name is
/// resolved from `PATH` so `--player ffplay` works.
///
/// mpv is also given a private IPC socket, which is the only way to learn where playback
/// is and whether it is paused. A player that ignores the flag simply never answers, and
/// the presentation falls back to its wall clock.
pub struct Mpv {
    program: String,
    position: Arc<Mutex<Option<Position>>>,
}

impl Mpv {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            position: Arc::new(Mutex::new(None)),
        }
    }

    /// The latest position, shared with whoever is drawing. `None` while nothing plays.
    pub fn clock(&self) -> Arc<Mutex<Option<Position>>> {
        Arc::clone(&self.position)
    }
}

impl Player for Mpv {
    fn play(&self, file: &Path) -> Result<(), PlayError> {
        let socket = Socket::open();
        let mut command = Command::new(&self.program);
        command.arg("--no-video").arg("--really-quiet");
        if let Some(socket) = &socket {
            command.arg(format!("--input-ipc-server={}", socket.path().display()));
        }
        let mut child = command
            .arg(file)
            .spawn()
            .map_err(|error| PlayError::Spawn {
                program: self.program.clone(),
                reason: error.to_string(),
            })?;

        let stop = Arc::new(AtomicBool::new(false));
        let watcher = socket.map(|socket| {
            let path = socket.path();
            let position = Arc::clone(&self.position);
            let stop = Arc::clone(&stop);
            // The socket has to outlive the thread, not the spawn call.
            std::thread::spawn(move || {
                let _keep = socket;
                while !stop.load(Ordering::Relaxed) {
                    let latest = query(&path);
                    if let Ok(mut slot) = position.lock() {
                        *slot = latest;
                    }
                    std::thread::sleep(POLL);
                }
            })
        });

        let waited = child.wait();
        stop.store(true, Ordering::Relaxed);
        if let Some(watcher) = watcher {
            let _ = watcher.join();
        }
        if let Ok(mut slot) = self.position.lock() {
            *slot = None;
        }

        let status = waited.map_err(|error| PlayError::Exit {
            program: self.program.clone(),
            status: error.to_string(),
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

/// A private directory for the IPC socket, removed when playback ends.
///
/// mpv creates the socket itself, so the directory has to exist first and be ours: a
/// predictable path in a world-writable temp directory is something another local user
/// could pre-create, and then playback either fails or reports positions that never
/// happened. The name is unique per run and the directory is 0700.
#[cfg(unix)]
struct Socket {
    dir: PathBuf,
}

#[cfg(unix)]
impl Socket {
    fn open() -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let dir = base.join(format!("libretto-ipc-{}-{stamp}", std::process::id()));
        // `create_dir` fails if the path exists, which is exactly what keeps a planted
        // directory or symlink out of this.
        std::fs::create_dir(&dir).ok()?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
        Some(Self { dir })
    }

    fn path(&self) -> PathBuf {
        self.dir.join("mpv.sock")
    }
}

#[cfg(unix)]
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(not(unix))]
struct Socket;

#[cfg(not(unix))]
impl Socket {
    fn open() -> Option<Self> {
        None
    }

    fn path(&self) -> PathBuf {
        PathBuf::new()
    }
}

/// Ask the player where it is: one short connection, two commands, two replies.
#[cfg(unix)]
fn query(path: &Path) -> Option<Position> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(path).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    stream
        .write_all(b"{\"command\":[\"get_property\",\"time-pos\"]}\n{\"command\":[\"get_property\",\"pause\"]}\n")
        .ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let seconds = reply_data(&line, serde_json::Value::as_f64)?;
    line.clear();
    reader.read_line(&mut line).ok()?;
    let paused = reply_data(&line, serde_json::Value::as_bool)?;
    Some(Position { seconds, paused })
}

#[cfg(not(unix))]
fn query(_path: &Path) -> Option<Position> {
    None
}

/// mpv answers `{"data": null, "error": "success"}` before playback starts. That is not
/// a position and not an error, so it reads as `None` and the caller keeps waiting.
fn reply_data<T>(line: &str, take: impl Fn(&serde_json::Value) -> Option<T>) -> Option<T> {
    let reply: serde_json::Value = serde_json::from_str(line).ok()?;
    if reply.get("error")?.as_str()? != "success" {
        return None;
    }
    take(reply.get("data")?)
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
    /// Ask the engine for word timings as well. Only a read-along needs them, and asking
    /// costs a refused request on an endpoint that has no such thing — so `read`, which
    /// draws nothing, does not ask.
    pub captions: bool,
}

/// Play `sentences[start..]`, stopping after `limit` sentences if given and between
/// sentences once `stop_after` has passed.
///
/// `on_played` is called with the index of the next sentence after each one
/// finishes, which is where the caller persists progress: a killed reader loses
/// at most one sentence.
pub fn play_sentences(
    session: &Session<'_>,
    sentences: &[String],
    start: usize,
    limit: Option<usize>,
    stop_after: Option<Instant>,
    mut on_speaking: impl FnMut(usize, Option<&[Word]>),
    mut on_played: impl FnMut(usize),
) -> Result<Played, PlayError> {
    let engine = session.engine;
    let player = session.player;
    let cache = session.cache;
    let template = session.template;
    // Read out before the worker is spawned: the closure must not capture the session,
    // which holds a `&dyn Player` that is not `Sync`.
    let captions = session.captions;
    let total = sentences.len();
    let begin = start.min(total);
    let end = match limit {
        Some(limit) => begin.saturating_add(limit).min(total),
        None => total,
    };
    // A window that has already closed starts nothing, and this check is up here rather
    // than only in the loop so it also pays for no synthesis: an expired `--for` costs a
    // hash and a parse, nothing more.
    let closed = || stop_after.is_some_and(|deadline| Instant::now() >= deadline);
    if begin >= end || closed() {
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
        let (tx, rx) = mpsc::sync_channel::<(
            usize,
            bool,
            Result<(PathBuf, Option<Vec<Word>>), CacheError>,
        )>(DEFAULT_LEAD);
        let worker = scope.spawn(move || {
            for (index, text) in sentences.iter().enumerate().take(end).skip(begin) {
                let mut request = template.clone();
                request.text = text.clone();
                let key = Cache::key(&request);
                let hit = cache.get(&key).is_some();
                let result = if captions {
                    cache.audio_with_timings(engine, &request)
                } else {
                    cache.audio_for(engine, &request).map(|path| (path, None))
                };
                // The receiver is gone only when playback already failed.
                if tx.send((index, hit, result)).is_err() {
                    break;
                }
            }
        });

        for (index, hit, result) in rx {
            // Between sentences, never inside one: the sentence being spoken is finished,
            // so the position saved below is a sentence boundary and nobody hears half a
            // word chopped off by a timer.
            if closed() {
                break;
            }
            let (path, words) = match result {
                Ok(audio) => {
                    silent_run = 0;
                    audio
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
            // Whoever draws gets the words before the audio starts, so the first frame of
            // a sentence already knows its timings.
            on_speaking(index, words.as_deref());
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

    use crate::tts::{Speech, TtsError};

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

    /// Answers with two word timings per sentence, so the wiring to the view is visible.
    struct CaptionedEngine;

    impl TtsEngine for CaptionedEngine {
        fn synthesize(&self, _request: &SpeechRequest) -> Result<Vec<u8>, TtsError> {
            Ok(b"ID3".to_vec())
        }

        fn synthesize_captioned(&self, request: &SpeechRequest) -> Result<Speech, TtsError> {
            Ok(Speech {
                audio: b"ID3".to_vec(),
                words: Some(crate::captions::interpolate(&request.text, 1.0)),
            })
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

    /// A player that takes its time. The only way to watch a listening window close in the
    /// middle of a run without waiting for a real book to be read aloud.
    struct SlowPlayer {
        played: RefCell<Vec<PathBuf>>,
        per_sentence: Duration,
    }

    impl Player for SlowPlayer {
        fn play(&self, file: &Path) -> Result<(), PlayError> {
            self.played.borrow_mut().push(file.to_path_buf());
            std::thread::sleep(self.per_sentence);
            Ok(())
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
            captions: false,
        };
        let mut positions = Vec::new();

        let result = play_sentences(
            &session,
            &sentences(4),
            1,
            Some(2),
            None,
            |_, _| {},
            |next| positions.push(next),
        )
        .expect("plays");

        assert_eq!(result.count, 2);
        assert_eq!(result.next, 3);
        assert_eq!(positions, vec![2, 3], "progress after each sentence");
        assert_eq!(engine.calls(), 2);
        assert_eq!(player.played.borrow().len(), 2);
    }

    #[test]
    fn the_run_stops_at_a_sentence_boundary_once_the_time_is_up() {
        let engine = CountingEngine::new();
        let player = SlowPlayer {
            played: RefCell::new(Vec::new()),
            per_sentence: Duration::from_millis(50),
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
            captions: false,
        };
        let mut positions = Vec::new();

        // Fifty milliseconds a sentence against a window of four hundred: the path from the
        // first sentence to the player — cache lookup, synthesis, the cache's fsync — has
        // 400 ms of room, so at least one sentence always starts; eight sentences cost
        // 8 × 50 = 400 ms, so the ninth cannot finish inside the window and ten of them
        // certainly do not.
        let window = Instant::now() + Duration::from_millis(400);
        let result = play_sentences(
            &session,
            &sentences(10),
            0,
            None,
            Some(window),
            |_, _| {},
            |next| positions.push(next),
        )
        .expect("plays");

        assert!(
            (1..=8).contains(&result.count),
            "the window was open when the run began and closed before the ninth sentence, not {}",
            result.count
        );
        assert_eq!(
            positions,
            (1..=result.count).collect::<Vec<_>>(),
            "progress is reported after each sentence that played, and it starts at one"
        );
        assert_eq!(
            player.played.borrow().len(),
            result.count,
            "the sentence being spoken is finished, not cut"
        );
    }

    #[test]
    fn an_expired_window_synthesizes_nothing() {
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
            captions: false,
        };

        let past = Instant::now() - Duration::from_secs(1);
        let result = play_sentences(
            &session,
            &sentences(4),
            0,
            None,
            Some(past),
            |_, _| {},
            |_| {},
        )
        .expect("no work to do");

        assert_eq!(result.count, 0);
        assert_eq!(result.next, 0, "nothing moved, so the next run starts here");
        assert_eq!(
            engine.calls(),
            0,
            "a window that never opened pays for no audio"
        );
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
            captions: false,
        };

        play_sentences(&session, &sentences(3), 0, None, None, |_, _| {}, |_| {}).expect("first");
        let second = play_sentences(&session, &sentences(3), 0, None, None, |_, _| {}, |_| {})
            .expect("second");

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
            captions: false,
        };

        let result = play_sentences(&session, &sentences(2), 9, None, None, |_, _| {}, |_| {})
            .expect("no work");

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
            captions: false,
        };
        let mut positions = Vec::new();

        let error = play_sentences(
            &session,
            &sentences(5),
            0,
            None,
            None,
            |_, _| {},
            |next| positions.push(next),
        )
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
            captions: false,
        };
        let lines = vec![
            "Sentence 0.".to_string(),
            "noise only".to_string(),
            "Sentence 2.".to_string(),
        ];
        let mut positions = Vec::new();

        let result = play_sentences(
            &session,
            &lines,
            0,
            None,
            None,
            |_, _| {},
            |next| positions.push(next),
        )
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
            captions: false,
        };

        let error = play_sentences(&session, &sentences(20), 0, None, None, |_, _| {}, |_| {})
            .expect_err("silence everywhere is a broken endpoint");

        assert!(
            matches!(
                error,
                PlayError::Cache(CacheError::Tts(TtsError::Empty { .. }))
            ),
            "got {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_player_is_told_where_the_socket_is() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let record = dir.path().join("argv");
        let stub = dir.path().join("player");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                record.display()
            ),
        )
        .expect("stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let file = dir.path().join("sentence.mp3");
        std::fs::write(&file, b"ID3").expect("file");

        Mpv::new(stub.to_str().expect("path"))
            .play(&file)
            .expect("the stub exits 0");

        let argv = std::fs::read_to_string(&record).expect("argv");
        assert!(
            argv.lines()
                .any(|line| line.starts_with("--input-ipc-server=")),
            "mpv is given a socket to answer on: {argv:?}"
        );
        assert!(
            argv.lines().any(|line| line.ends_with("sentence.mp3")),
            "and the file to play: {argv:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_position_is_read_from_the_socket_including_a_pause() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mpv.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("first command");
            assert!(
                line.contains("\"time-pos\""),
                "asked for the position: {line}"
            );
            stream
                .write_all(b"{\"data\":12.5,\"error\":\"success\"}\n")
                .expect("reply");
            line.clear();
            reader.read_line(&mut line).expect("second command");
            assert!(
                line.contains("\"pause\""),
                "asked whether it is paused: {line}"
            );
            stream
                .write_all(b"{\"data\":true,\"error\":\"success\"}\n")
                .expect("reply");
        });

        let position = query(&path).expect("a position");
        server.join().expect("server");

        assert!((position.seconds - 12.5).abs() < 1e-6, "{position:?}");
        assert!(position.paused, "a paused player says so: {position:?}");
    }

    #[test]
    fn no_socket_and_no_position_are_the_same_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            query(&dir.path().join("absent.sock")),
            None,
            "a player that never answers leaves the clock to the caller"
        );
    }

    #[test]
    fn a_reply_before_playback_is_not_a_position() {
        assert_eq!(
            reply_data(
                "{\"data\":null,\"error\":\"success\"}",
                serde_json::Value::as_f64
            ),
            None
        );
        assert_eq!(
            reply_data(
                "{\"data\":1.0,\"error\":\"property unavailable\"}",
                serde_json::Value::as_f64
            ),
            None
        );
        assert_eq!(reply_data("not json", serde_json::Value::as_f64), None);
    }

    #[test]
    fn the_words_of_a_sentence_reach_the_caller_before_it_plays() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = Cache::at(dir.path());
        let engine = CaptionedEngine;
        let player = recording_player(None);
        let template = SpeechRequest::new("");
        let session = Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
            captions: true,
        };
        let lines = vec!["one two".to_string(), "three four five".to_string()];

        let mut seen: Vec<(usize, usize)> = Vec::new();
        let mut played_before = Vec::new();
        play_sentences(
            &session,
            &lines,
            0,
            None,
            None,
            |index, words| {
                played_before.push(player.played.borrow().len());
                seen.push((index, words.map(|words| words.len()).unwrap_or(0)));
            },
            |_| {},
        )
        .expect("plays");

        assert_eq!(
            seen,
            vec![(0, 2), (1, 3)],
            "the timings match each sentence"
        );
        assert_eq!(
            played_before,
            vec![0, 1],
            "and they arrive before that sentence is handed to the player"
        );
    }
}
