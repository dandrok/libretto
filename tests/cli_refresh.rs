//! AC-25: a changed book is recoverable in place, and the position survives it.
//!
//! The point of the command is that the two refusals around it are both right — exit 4
//! because the index no longer describes the file, and a second `add` because the bytes are
//! no longer the ones that were imported — so the only way out used to be `rm` + `add`,
//! which throws the position away with it.

mod common;

use common::Sandbox;
use common::fake_tts::{FakeTts, Reply};

const AUDIO: &[u8] = b"ID3\x04\x00\x00fake-mp3";

fn position(sandbox: &Sandbox) -> (usize, usize) {
    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    let progress = json["progress"].as_object().expect("progress map");
    let (_, entry) = progress.iter().next().expect("one entry of progress");
    (
        entry["chapter_index"].as_u64().unwrap_or(0) as usize,
        entry["sentence_index"].as_u64().unwrap_or(0) as usize,
    )
}

/// The one id in the library, so a refresh can be shown to have kept it.
fn only_id(sandbox: &Sandbox) -> String {
    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    let books = json["books"].as_array().expect("books");
    assert_eq!(books.len(), 1, "refresh never forks the entry: {books:?}");
    books[0]["id"].as_str().expect("id").to_string()
}

#[test]
fn refreshing_a_rewritten_book_keeps_the_position_and_reads_it() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Rewritten"),
        &[(
            "One",
            "One sentence. Two sentence. Three sentence. Four sentence.",
        )],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();
    sandbox.stdout_of(&[
        "read",
        "rewritten",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        "--sentences",
        "2",
    ]);
    assert_eq!(position(&sandbox), (0, 2), "two sentences played");
    let id = only_id(&sandbox);

    // The same path, a new edition: a second chapter, and the first one longer.
    sandbox.book(
        "book.epub",
        Some("Rewritten"),
        &[
            (
                "One",
                "One sentence. Two sentence. Three sentence. Four sentence. Five sentence. Six sentence.",
            ),
            ("Two", "A chapter the first import never had."),
        ],
    );

    // The wall this command exists for, asserted rather than assumed.
    let refused = sandbox.run(&["read", "rewritten", "--player", "true"]);
    assert_eq!(
        refused.status.code(),
        Some(4),
        "a changed book is a bad book"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("libretto refresh"),
        "and it names the remedy: {}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let refreshed = sandbox.run(&["refresh", "rewritten"]);
    assert_eq!(
        refreshed.status.code(),
        Some(0),
        "refresh is not a failure: {}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    assert_eq!(
        only_id(&sandbox),
        id,
        "the id the position is keyed on is kept"
    );
    assert_eq!(position(&sandbox), (0, 2), "and so is the position");

    let stdout = sandbox.stdout_of(&[
        "read",
        "rewritten",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        "--sentences",
        "1",
    ]);
    assert!(
        stdout.contains("starting at sentence 3/"),
        "reads on from where it stopped: {stdout}"
    );
}

#[test]
fn a_position_past_the_end_of_the_new_index_is_clamped() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Shrinking"),
        &[(
            "One",
            "One sentence. Two sentence. Three sentence. Four sentence. Five sentence. Six sentence.",
        )],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();
    sandbox.stdout_of(&[
        "read",
        "shrinking",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        "--sentences",
        "5",
    ]);
    assert_eq!(position(&sandbox), (0, 5), "five sentences in");

    // Shorter than the reader got to: the stored sentence no longer exists.
    sandbox.book(
        "book.epub",
        Some("Shrinking"),
        &[("One", "One sentence. Two sentence.")],
    );
    sandbox.stdout_of(&["refresh", "shrinking"]);

    let stdout = sandbox.stdout_of(&["ls"]);
    assert!(
        stdout.contains("done"),
        "a clamped position at the end is finished, not an error: {stdout}"
    );
}

#[test]
fn refreshing_an_unchanged_book_changes_nothing() {
    let sandbox = Sandbox::new();
    let book = sandbox.book("book.epub", Some("Same Book"), &[("One", "Some text.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let before = std::fs::read(sandbox.library_path()).expect("library");

    let output = sandbox.run(&["refresh", "same-book"]);
    assert!(output.status.success(), "a no-op is not a failure");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("up to date"), "says so: {stdout}");
    assert_eq!(
        std::fs::read(sandbox.library_path()).expect("library"),
        before,
        "nothing was rewritten"
    );
}

#[test]
fn refresh_on_an_unknown_id_exits_1() {
    let sandbox = Sandbox::new();
    let book = sandbox.book("book.epub", Some("Some Book"), &[("One", "Text.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let output = sandbox.run(&["refresh", "does-not-exist"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown book"));
}
