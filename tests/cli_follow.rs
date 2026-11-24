//! AC-20: `follow` is a presentation of `read`, and a pipe cannot tell them apart.

mod common;

use common::Sandbox;
use common::fake_tts::{FakeTts, Reply};

const AUDIO: &[u8] = b"ID3\x04\x00\x00fake-mp3";

#[test]
fn follow_on_a_pipe_is_read_byte_for_byte() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "followed.epub",
        Some("Followed Book"),
        &[
            ("One", "First sentence. Second sentence."),
            ("Two", "Third."),
        ],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let shared = [
        "followed",
        "--player",
        "true",
        "--endpoint",
        endpoint.as_str(),
        "--sentences",
        "1",
        "--chapter",
        "1",
    ];
    let mut read_args = vec!["read"];
    read_args.extend_from_slice(&shared);
    let mut follow_args = vec!["follow"];
    follow_args.extend_from_slice(&shared);

    let read = sandbox.stdout_of(&read_args);
    // Same starting point, and no warm cache to report a hit the other run did not have:
    // the comparison is about presentation, not about the cache.
    std::fs::remove_dir_all(sandbox.root().join("cache")).expect("clear cache");
    let follow = sandbox.stdout_of(&follow_args);
    assert_eq!(
        read, follow,
        "no terminal means no block, no escapes and no different words"
    );
    assert!(
        !follow.contains('\x1b'),
        "a pipe must never receive escape sequences: {follow:?}"
    );
}

#[test]
fn follow_quiet_is_plain_and_says_where_it_stopped() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "quiet.epub",
        Some("Quiet Book"),
        &[("One", "Only sentence.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let output = sandbox.run(&[
        "follow",
        "quiet",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        "--quiet",
    ]);
    assert!(output.status.success(), "quiet follow still reads");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("stopped at chapter 1"), "{stdout}");
    assert!(
        !stdout.contains('\x1b'),
        "no escapes with --quiet: {stdout:?}"
    );
}

#[test]
fn follow_accepts_the_same_arguments_as_read() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "args.epub",
        Some("Arguable Book"),
        &[("One", "A. B. C. D."), ("Two", "E.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let stdout = sandbox.stdout_of(&[
        "follow",
        "arguable",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        // No --chapter: that flag means "start this chapter", which resets the position
        // and would swallow --skip.
        "--skip",
        "1",
        "--sentences",
        "1",
        // An hour is longer than this book, so the budget never ends the run early: the
        // point is that `follow` takes the flag `read` takes.
        "--for",
        "1h",
        "--speed",
        "1.25",
        "--width",
        "50",
    ]);
    assert!(stdout.contains("starting at sentence 2/"), "{stdout}");
}

/// The one thing a pipe cannot show: that a terminal gets the block at all, and that a
/// block taller than one row is replaced rather than printed again underneath. The pty
/// comes from `common::under_pty`.
#[cfg(target_os = "linux")]
#[test]
fn on_a_terminal_follow_draws_the_sentence_and_lights_a_word() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "screen.epub",
        Some("Screen Book"),
        &[("One", "Alpha beta gamma delta epsilon zeta eta theta.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let raw = common::under_pty(
        &sandbox,
        &format!("follow screen --endpoint '{endpoint}' --sentences 1"),
    );

    assert!(
        raw.contains("Alpha beta gamma"),
        "the sentence reaches the terminal: {raw:?}"
    );
    assert!(raw.contains("\x1b[4m"), "a word is underlined: {raw:?}");
}

#[test]
fn a_negative_sync_offset_is_a_value_not_a_flag() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book("sync.epub", Some("Sync Book"), &[("One", "One. Two.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    for offset in ["-80", "80", "0"] {
        let output = sandbox.run(&[
            "follow",
            "sync",
            "--player",
            "true",
            "--endpoint",
            &endpoint,
            "--chapter",
            "1",
            "--sentences",
            "1",
            "--sync-offset",
            offset,
        ]);
        assert!(
            output.status.success(),
            "{offset} was refused as a flag: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The width the block wraps to has to be the terminal's own. Guessing eighty columns on a
/// sixty-column terminal makes every painted row soft-wrap into two, and the painter, which
/// counts the logical rows it wrote, then moves up one row too few — so the next frame lands
/// inside the previous block and the screen fills with duplicated, glued sentences.
#[cfg(target_os = "linux")]
#[test]
fn a_narrow_terminal_gets_rows_that_fit_it() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "narrow.epub",
        Some("Narrow Book"),
        &[(
            "One",
            "The systems of record are the primary databases to which data is first written, \
             and every other system in the organisation derives from them in some way.",
        )],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let raw = common::under_pty_sized(
        &sandbox,
        &format!("follow narrow --endpoint '{endpoint}' --sentences 1"),
        60,
    );

    let rows = common::painted_rows(&raw);
    assert!(
        !rows.is_empty(),
        "the block was drawn at all, or nothing below means anything: {raw:?}"
    );
    for row in &rows {
        assert!(
            row.chars().count() <= 60,
            "a row must fit the 60-column terminal it is drawn on, this one is {} \
             characters and will wrap: {row:?}",
            row.chars().count()
        );
    }
}
