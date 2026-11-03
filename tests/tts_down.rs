//! AC-7 (engine half): a dead or dishonest endpoint produces a named error,
//! never a hang and never silence.

mod common;

use common::fake_tts::{FakeTts, Reply};
use libretto::tts::{Kokoro, SpeechRequest, TtsEngine, TtsError};

#[test]
fn an_unreachable_endpoint_names_itself() {
    // port 9 (discard) on loopback: nothing listens, connection is refused fast
    let engine = Kokoro::new("http://127.0.0.1:9/v1").expect("valid url");
    let error = engine
        .synthesize(&SpeechRequest::new("Hello."))
        .expect_err("must fail");
    match error {
        TtsError::Unreachable { endpoint, .. } => {
            assert_eq!(endpoint, "http://127.0.0.1:9/v1");
        }
        other => panic!("expected Unreachable, got {other}"),
    }
    assert!(error_message(&engine).contains("127.0.0.1:9"));
}

fn error_message(engine: &Kokoro) -> String {
    engine
        .synthesize(&SpeechRequest::new("Hello."))
        .expect_err("must fail")
        .to_string()
}

#[test]
fn a_non_http_endpoint_is_refused_at_construction() {
    let error = match Kokoro::new("ftp://example.com/v1") {
        Ok(_) => panic!("a non-http endpoint must be refused"),
        Err(error) => error,
    };
    assert!(matches!(error, TtsError::BadEndpoint { .. }));
    assert!(error.to_string().contains("not an http(s) URL"));
}

#[test]
fn an_error_status_is_reported_with_the_code() {
    let fake = FakeTts::start(Reply::Status(500));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let error = engine
        .synthesize(&SpeechRequest::new("Hello."))
        .expect_err("must fail");

    assert!(
        matches!(error, TtsError::Status { status: 500, .. }),
        "got {error}"
    );
    assert!(error.to_string().contains("500"));
}

#[test]
fn an_oversized_reply_is_refused_instead_of_allocated() {
    // The ceiling is injected: 2 KiB against 1 KiB. Asserting the *limit* with a
    // 33 MiB body made this test depend on socket timing — on CI the client hit
    // its limit, closed, and the error surfaced as "Peer disconnected" instead
    // of BodyExceedsLimit. The property under test is "a reply past the limit is
    // refused", and a small limit tests it deterministically.
    let fake = FakeTts::start(Reply::Audio(vec![b'x'; 2048]));
    let engine = Kokoro::with_max_audio_bytes(fake.endpoint(), 1024).expect("engine");
    let error = engine
        .synthesize(&SpeechRequest::new("Hello."))
        .expect_err("must fail");

    assert!(matches!(error, TtsError::TooLarge { .. }), "got {error}");
}

#[test]
fn a_reply_under_the_ceiling_is_accepted() {
    // Positive control for the limit above: without this, a test asserting
    // "oversized is refused" also passes when *everything* is refused.
    let fake = FakeTts::start(Reply::Audio(vec![b'a'; 1000]));
    let engine = Kokoro::with_max_audio_bytes(fake.endpoint(), 1024).expect("engine");
    let audio = engine
        .synthesize(&SpeechRequest::new("Hello."))
        .expect("1000 bytes is not oversized");
    assert_eq!(audio.len(), 1000);
}

#[test]
fn an_empty_200_is_an_error_not_an_empty_file() {
    let fake = FakeTts::start(Reply::Empty);
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let error = engine
        .synthesize(&SpeechRequest::new("Hello."))
        .expect_err("must fail");

    assert!(matches!(error, TtsError::Empty { .. }), "got {error}");
    assert!(error.to_string().contains("no audio"));
}
