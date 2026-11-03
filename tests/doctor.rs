//! `libretto doctor`: a report a user can act on, and an exit code a script can.

mod common;

use common::Sandbox;
use common::fake_tts::{FakeTts, Reply};

#[test]
fn a_healthy_setup_reports_every_check_and_exits_0() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(b"ID3".to_vec()));
    sandbox.seed_library(3);

    let endpoint = fake.endpoint();
    let output = sandbox.run(&["doctor", "--endpoint", &endpoint, "--player", "true"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "a healthy setup is a success: {stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for label in ["library", "config", "player", "tts", "cache"] {
        assert!(stdout.contains(label), "reports {label}: {stdout}");
    }
    assert!(stdout.contains("3 book(s)"), "counts the library: {stdout}");
    assert!(
        stdout.contains("answering"),
        "confirms the endpoint answered: {stdout}"
    );
    assert!(!stdout.contains("FAIL"), "nothing failed: {stdout}");
}

#[test]
fn a_dead_endpoint_is_reported_and_exits_nonzero() {
    let sandbox = Sandbox::new();
    sandbox.seed_library(1);

    let output = sandbox.run(&[
        "doctor",
        "--endpoint",
        "http://127.0.0.1:9/v1",
        "--player",
        "true",
    ]);

    assert_eq!(
        output.status.code(),
        Some(6),
        "a failing check is state (6)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FAIL"), "marks the failure: {stdout}");
    assert!(
        stdout.contains("127.0.0.1:9"),
        "names the endpoint it could not reach: {stdout}"
    );
}

#[test]
fn a_missing_player_is_reported_without_stopping_the_other_checks() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(b"ID3".to_vec()));
    let endpoint = fake.endpoint();

    let output = sandbox.run(&[
        "doctor",
        "--endpoint",
        &endpoint,
        "--player",
        "definitely-not-a-player",
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(6));
    assert!(
        stdout.contains("definitely-not-a-player"),
        "names the program: {stdout}"
    );
    assert!(
        stdout.contains("answering"),
        "the endpoint check still ran: {stdout}"
    );
}
