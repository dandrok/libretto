//! AC-4, AC-6, AC-7, AC-8: the `read` command's contract.
//!
//! Playback runs with `--player true` — resolved from PATH, never an absolute
//! path, because macOS keeps it in /usr/bin while Linux has it in /bin.
//! immediately: the loop and the position bookkeeping are exercised, no audio
//! device is needed, and no test depends on how fast mpv starts.

mod common;

use common::Sandbox;
use common::fake_tts::{FakeTts, Reply};
use libretto::text::sentences;

const AUDIO: &[u8] = b"ID3\x04\x00\x00fake-mp3";

fn sentence_count(path: &std::path::Path) -> usize {
    let book = libretto::epub::load(path).expect("fixture loads");
    book.chapters
        .iter()
        .map(|chapter| sentences(&chapter.text, 400).len())
        .sum::<usize>()
        .min(
            book.chapters
                .first()
                .map_or(0, |c| sentences(&c.text, 400).len()),
        )
}

fn position(sandbox: &Sandbox, id_fragment: &str) -> (usize, usize) {
    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    let progress = &json["progress"];
    let Some(key) = progress
        .as_object()
        .expect("progress map")
        .keys()
        .find(|key| key.contains(id_fragment))
        .cloned()
    else {
        // No entry at all: nothing was played. That is a legitimate state and
        // exactly what a failed run should leave behind.
        return (0, 0);
    };
    (
        progress[&key]["chapter_index"].as_u64().unwrap_or(0) as usize,
        progress[&key]["sentence_index"].as_u64().unwrap_or(0) as usize,
    )
}

fn read_args<'a>(book: &'a str, fake: &'a FakeTts, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["read", book, "--player", "true"];
    args.extend_from_slice(extra);
    let _ = fake;
    args
}

#[test]
fn read_plays_a_chapter_and_remembers_where_it_stopped() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Two Chapters"),
        &[
            ("One", "First sentence here. Second one follows."),
            ("Two", "Third sentence."),
        ],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let total = sentence_count(&book);

    let endpoint = fake.endpoint();
    let args = read_args("two-chapters", &fake, &["--endpoint", &endpoint]);
    let stdout = sandbox.stdout_of(&args);

    assert!(
        stdout.contains("Two Chapters"),
        "prints what it is reading: {stdout}"
    );
    let (chapter, sentence) = position(&sandbox, "two-chapters");
    assert_eq!(chapter, 0, "chapter one");
    assert_eq!(sentence, total, "the whole chapter was played");
    assert!(fake.hits() >= total, "each sentence was synthesized once");
}

#[test]
fn resume_continues_from_the_stored_sentence() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Resumable"),
        &[(
            "One",
            "One sentence. Two sentence. Three sentence. Four sentence. Five sentence.",
        )],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();

    let first = read_args(
        "resumable",
        &fake,
        &["--endpoint", &endpoint, "--sentences", "2"],
    );
    sandbox.stdout_of(&first);
    assert_eq!(position(&sandbox, "resumable").1, 2, "two sentences played");

    let second = read_args(
        "resumable",
        &fake,
        &["--endpoint", &endpoint, "--sentences", "1"],
    );
    let stdout = sandbox.stdout_of(&second);
    assert!(
        stdout.contains("starting at sentence 3/"),
        "resumes at 3: {stdout}"
    );
    assert_eq!(
        position(&sandbox, "resumable").1,
        3,
        "one more sentence played"
    );
}

#[test]
fn skip_advances_the_position_without_playing_what_it_skips() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Skippable"),
        &[("One", "Alpha. Beta. Gamma. Delta. Epsilon. Zeta.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();

    let args = read_args(
        "skippable",
        &fake,
        &["--endpoint", &endpoint, "--skip", "3", "--sentences", "1"],
    );
    sandbox.stdout_of(&args);

    assert_eq!(
        position(&sandbox, "skippable").1,
        4,
        "three skipped, one played, position four"
    );
    assert_eq!(fake.hits(), 1, "skipped sentences are never synthesized");
}

#[test]
fn an_ambiguous_prefix_lists_the_candidates_and_exits_2() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let first = sandbox.book("a.epub", Some("Grokking Algorithms"), &[("One", "Text.")]);
    let second = sandbox.book(
        "b.epub",
        Some("Groovy in Action"),
        &[("One", "Other text.")],
    );
    sandbox.stdout_of(&["add", first.to_str().expect("path")]);
    sandbox.stdout_of(&["add", second.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let output = sandbox.run(&["read", "gro", "--player", "true", "--endpoint", &endpoint]);

    assert_eq!(output.status.code(), Some(2), "ambiguous is its own code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ambiguous"), "says why: {stderr}");
    assert!(
        stderr.contains("grokking") && stderr.contains("groovy"),
        "lists both: {stderr}"
    );
    assert_eq!(fake.hits(), 0, "nothing is synthesized for an ambiguous id");
}

#[test]
fn an_unknown_book_exits_1() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let endpoint = fake.endpoint();
    let output = sandbox.run(&[
        "read",
        "nothing-here",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown book"));
}

#[test]
fn a_dead_tts_endpoint_exits_3_and_leaves_the_position_alone() {
    let sandbox = Sandbox::new();
    let book = sandbox.book(
        "book.epub",
        Some("Unreachable"),
        &[("One", "Some text. More text.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let output = sandbox.run(&[
        "read",
        "unreachable",
        "--player",
        "true",
        "--endpoint",
        "http://127.0.0.1:9/v1",
    ]);

    assert_eq!(output.status.code(), Some(3), "TTS down is its own code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("127.0.0.1:9"),
        "names the endpoint: {stderr}"
    );
    assert_eq!(
        position(&sandbox, "unreachable"),
        (0, 0),
        "a failed run must not claim progress — no entry is written at all"
    );
}

#[test]
fn a_title_with_control_characters_cannot_drive_the_terminal() {
    // The library file is editable by hand and its titles are printed; an ESC
    // sequence there could move the cursor, rewrite the prompt or set the
    // clipboard (OSC 52). Every printed title goes through a control-character
    // filter, so nothing reaches the terminal as an instruction.
    let sandbox = Sandbox::new();
    let book = sandbox.book("book.epub", Some("Innocent"), &[("One", "Text.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let hostile = text.replace(
        "\"title\": \"Innocent\"",
        "\"title\": \"Innocent\\u001b]52;c;ZXZpbA==\\u0007 redacted\"",
    );
    assert_ne!(hostile, text, "the fixture title must have been rewritten");
    std::fs::write(sandbox.library_path(), hostile).expect("write hostile library");

    let stdout = sandbox.stdout_of(&["ls"]);

    assert!(
        stdout.contains("Innocent"),
        "the readable part survives: {stdout}"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "no ESC byte reaches the terminal: {stdout:?}"
    );
    assert!(
        !stdout.contains('\u{7}'),
        "no BEL byte reaches the terminal: {stdout:?}"
    );
}

#[test]
fn a_changed_book_file_is_refused_instead_of_read() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book("book.epub", Some("Original"), &[("One", "Some text.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    // Replace the file behind the library entry: a tampered library must not
    // become "play whatever this path points at now".
    let other = sandbox.book(
        "other.epub",
        Some("Impostor"),
        &[("One", "Different text.")],
    );
    std::fs::copy(&other, &book).expect("swap the book");

    let endpoint = fake.endpoint();
    let output = sandbox.run(&[
        "read",
        "original",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
    ]);

    assert_eq!(
        output.status.code(),
        Some(4),
        "a changed book is a bad book"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("changed"), "says what happened: {stderr}");
    assert!(
        stderr.contains("libretto add"),
        "says how to fix it: {stderr}"
    );
    assert_eq!(fake.hits(), 0, "nothing is synthesized from the impostor");
}

#[test]
fn the_chapter_flag_starts_at_the_first_sentence_of_that_chapter() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Chapters"),
        &[
            ("One", "First chapter text. More first chapter."),
            ("Two", "Second chapter text. More second."),
        ],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();

    let args = read_args(
        "chapters",
        &fake,
        &[
            "--endpoint",
            &endpoint,
            "--chapter",
            "2",
            "--sentences",
            "1",
        ],
    );
    sandbox.stdout_of(&args);

    let (chapter, sentence) = position(&sandbox, "chapters");
    assert_eq!(chapter, 1, "chapter two, zero-based");
    assert_eq!(sentence, 1, "one sentence into it");
}
