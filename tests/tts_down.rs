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
    // 33 MiB for one sentence is not audio, it is an endpoint deciding how much
    // memory this process gets to use.
    let huge = vec![b'x'; 33 * 1024 * 1024];
    let fake = FakeTts::start(Reply::Audio(huge));
    let engine = Kokoro::new(fake.endpoint()).expect("engine");
    let error = engine
        .synthesize(&SpeechRequest::new("Hello."))
        .expect_err("must fail");

    assert!(matches!(error, TtsError::TooLarge { .. }), "got {error}");
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
