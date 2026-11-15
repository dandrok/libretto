//! AC-5: the cache is the reason a chapter can be replayed for free.

mod common;

use common::fake_tts::{FakeTts, Reply};
use libretto::cache::Cache;
use libretto::tts::{Kokoro, SpeechRequest};

const AUDIO: &[u8] = b"ID3\x04\x00\x00fake-mp3-payload";

fn sentence(index: usize) -> String {
    format!("Sentence number {index}, which is long enough to be worth caching.")
}

#[test]
fn second_pass_over_a_chapter_issues_no_requests() {
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path());

    let sentences: Vec<SpeechRequest> = (0..5).map(|i| SpeechRequest::new(sentence(i))).collect();

    let first: Vec<_> = sentences
        .iter()
        .map(|request| cache.audio_for(&engine, request).expect("first pass"))
        .collect();
    assert_eq!(
        fake.hits(),
        5,
        "a cold cache synthesizes each sentence once"
    );

    let second: Vec<_> = sentences
        .iter()
        .map(|request| cache.audio_for(&engine, request).expect("second pass"))
        .collect();
    assert_eq!(fake.hits(), 5, "a warm cache must issue no requests at all");
    assert_eq!(first, second, "the same sentences map to the same files");
    for path in &second {
        assert_eq!(
            std::fs::read(path).expect("audio on disk"),
            AUDIO,
            "the cached bytes are the ones the engine returned"
        );
    }
}

#[test]
fn a_slow_client_keeps_its_connection_open() {
    // The client fsyncs each mp3 before asking for the next sentence, and on a
    // loaded runner that gap is longer than the 400 ms idle timeout the fake
    // server used to have — which closed a live pooled socket and surfaced as
    // "io: Peer disconnected". Asserting request count alone does not catch it,
    // because ureq silently reconnects; asserting the connection count does.
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path());

    cache
        .audio_for(&engine, &SpeechRequest::new("First sentence."))
        .expect("first request");
    std::thread::sleep(std::time::Duration::from_millis(1200));
    cache
        .audio_for(&engine, &SpeechRequest::new("Second sentence."))
        .expect("a pooled connection must survive a slow client");

    assert_eq!(fake.hits(), 2, "both requests reached the server");
    assert_eq!(
        fake.connections(),
        1,
        "one socket served both: a second connection means the server hung up on a slow client"
    );
}

#[test]
fn a_different_voice_is_a_different_entry() {
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path());

    let heart = SpeechRequest::new("Same words.").with_voice("af_heart");
    let george = SpeechRequest::new("Same words.").with_voice("bm_george");

    let a = cache.audio_for(&engine, &heart).expect("heart");
    let b = cache.audio_for(&engine, &george).expect("george");

    assert_ne!(a, b, "a different narrator is a different file");
    assert_eq!(fake.hits(), 2);
}

#[test]
fn the_request_asks_for_mp3_with_the_right_voice_and_text() {
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path());

    let request = SpeechRequest::new("Hello there.")
        .with_voice("bf_emma")
        .with_speed(1.25);
    cache.audio_for(&engine, &request).expect("audio");

    // Parse, do not compare formatting: the wire shape is the contract, not the
    // whitespace serde happens to emit.
    let body: serde_json::Value =
        serde_json::from_str(&fake.last_body()).expect("the request body is json");
    assert_eq!(body["input"], "Hello there.");
    assert_eq!(body["voice"], "bf_emma");
    assert_eq!(body["model"], "kokoro");
    assert_eq!(body["response_format"], "mp3");
    assert_eq!(body["speed"], 1.25);
}

#[test]
fn cached_files_are_private_to_their_owner() {
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path().join("audio"));

    let path = cache
        .audio_for(&engine, &SpeechRequest::new("A sentence."))
        .expect("audio");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "audio is derived from a book");
        let parent = path.parent().expect("parent");
        let dir_mode = std::fs::metadata(parent)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }
    assert!(path.exists());
    assert!(
        !path.with_extension("mp3.tmp").exists(),
        "no temp file survives a successful write"
    );
}

#[test]
fn deleting_the_cache_only_costs_a_resynthesis() {
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path());
    let request = SpeechRequest::new("Anything.");

    cache.audio_for(&engine, &request).expect("first");
    std::fs::remove_dir_all(cache.root()).expect("wipe cache");
    cache.audio_for(&engine, &request).expect("after wipe");

    assert_eq!(fake.hits(), 2, "the cache is disposable, not authoritative");
}

#[test]
fn timings_are_stored_beside_the_audio_and_cost_nothing_to_replay() {
    let fake = FakeTts::start(Reply::Captioned {
        audio: AUDIO.to_vec(),
        words: vec![
            ("Hello".to_string(), 0.0, 0.4),
            ("world".to_string(), 0.4, 0.9),
        ],
    });
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path());
    let request = SpeechRequest::new("Hello world");

    let (path, words) = cache
        .audio_with_timings(&engine, &request)
        .expect("first pass");
    assert_eq!(std::fs::read(&path).expect("audio on disk"), AUDIO);
    let words = words.expect("the engine had timings");
    assert_eq!(words.len(), 2);
    assert_eq!(words[1].text, "world");
    assert_eq!(fake.hits(), 1, "the timings ride along with the audio");

    let (again, words) = cache
        .audio_with_timings(&engine, &request)
        .expect("second pass");
    assert_eq!(again, path);
    assert_eq!(
        words.expect("timings from the sidecar").len(),
        2,
        "a warm cache still knows the words"
    );
    assert_eq!(fake.hits(), 1, "and it cost no request at all");
}

#[test]
fn a_cached_sentence_without_timings_is_not_re_synthesized_to_get_them() {
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::at(dir.path());
    let request = SpeechRequest::new("A sentence from an engine without timings.");

    cache.audio_for(&engine, &request).expect("first pass");
    assert_eq!(fake.hits(), 1);

    let (_, words) = cache
        .audio_with_timings(&engine, &request)
        .expect("second pass");
    assert_eq!(words, None, "no sidecar means interpolate");
    assert_eq!(fake.hits(), 1, "never a second request for a warm file");
}
