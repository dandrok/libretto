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

/// One row of the wordmark, exactly as it is drawn. A fragment of the mark rather than a
/// shape-less word, so the terminal test proves *this* mark arrives and the pipe test proves
/// it does not — an absence assertion on a string the art no longer contains passes while
/// asserting nothing.
const WORDMARK_ROW: &str = "╚══════╝╚═╝╚═════╝ ╚═╝  ╚═╝╚══════╝";

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
fn a_timer_that_has_already_expired_plays_nothing() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Timed"),
        &[("One", "One sentence. Two sentence. Three sentence.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();

    let args = read_args("timed", &fake, &["--endpoint", &endpoint, "--for", "0s"]);
    let stdout = sandbox.stdout_of(&args);

    assert_eq!(
        position(&sandbox, "timed"),
        (0, 0),
        "a window that closed before the run began starts no sentence"
    );
    assert_eq!(
        fake.hits(),
        0,
        "nor does it pay for audio it will not speak"
    );
    assert!(
        stdout.contains("0 played"),
        "and it says so rather than pretending the chapter ended: {stdout}"
    );
}

#[test]
fn a_timer_that_cannot_be_parsed_exits_2_without_playing() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book("book.epub", Some("Timed"), &[("One", "Some text.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();

    let output = sandbox.run(&[
        "read",
        "timed",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        "--for",
        "half an hour",
    ]);

    assert_eq!(output.status.code(), Some(2), "bad input is not a state");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--for") && stderr.contains("invalid value"),
        "the flag, not an unknown argument: {stderr}"
    );
    assert_eq!(
        fake.hits(),
        0,
        "nothing is played for a duration it cannot read"
    );
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
        stderr.contains("libretto refresh"),
        "says how to fix it without losing the position: {stderr}"
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

/// AC-16, AC-17: the flourish belongs to terminals. A pipe gets the plain status
/// line and no escapes, and `--quiet` is accepted and equally plain.
#[test]
fn a_piped_read_stays_plain_and_quiet_is_accepted() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "book.epub",
        Some("Plain Output"),
        &[("One", "First sentence here. Second one follows.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    let endpoint = fake.endpoint();

    let piped = sandbox.run(&read_args(
        "plain",
        &fake,
        &["--endpoint", &endpoint, "--sentences", "1"],
    ));
    assert!(
        piped.status.success(),
        "{:?}",
        String::from_utf8_lossy(&piped.stderr)
    );
    let status = String::from_utf8_lossy(&piped.stderr);
    assert!(
        !status.contains('\x1b'),
        "a pipe must not receive escapes: {status:?}"
    );
    assert!(
        !status.contains(WORDMARK_ROW),
        "no wordmark away from a terminal: {status:?}"
    );
    assert!(
        status.contains("af_heart ×"),
        "the plain status line still reports: {status:?}"
    );

    let quiet = sandbox.run(&read_args(
        "plain",
        &fake,
        &["--endpoint", &endpoint, "--sentences", "1", "--quiet"],
    ));
    assert!(
        quiet.status.success(),
        "--quiet must not change the exit code: {:?}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains('\x1b'),
        "--quiet stays plain"
    );
}

/// AC-16: the wordmark is the terminal's, it is drawn exactly once per run, and it is the
/// mark the README shows. A pipe never sees it (the test above proves that side), so this is
/// the positive control — without it a mark that stopped being drawn would still pass.
#[cfg(target_os = "linux")]
#[test]
fn on_a_terminal_the_wordmark_is_drawn_once() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "marked.epub",
        Some("Marked Book"),
        &[("One", "First sentence here.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let raw = common::under_pty(
        &sandbox,
        &format!("read marked --endpoint '{endpoint}' --sentences 1"),
    );

    assert_eq!(
        raw.matches(WORDMARK_ROW).count(),
        1,
        "the wordmark reaches the terminal once: {raw:?}"
    );
    assert!(
        raw.contains("██╗     ██╗██████╗"),
        "its opening row is drawn too, not just one line of it: {raw:?}"
    );
}

/// Put a `pdftotext` on the sandbox's `PATH` that waits before doing the real thing. The
/// parse is real — only slow — which is exactly the machine the line is for.
#[cfg(target_os = "linux")]
fn a_slow_pdftotext(sandbox: &Sandbox, milliseconds: u32) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let real = std::process::Command::new("sh")
        .args(["-c", "command -v pdftotext"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    if real.is_empty() {
        eprintln!("skipping: pdftotext not installed");
        return false;
    }
    let bin = sandbox.root().join("bin");
    std::fs::create_dir_all(&bin).expect("stub dir");
    let stub = bin.join("pdftotext");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nsleep {}\nexec '{real}' \"$@\"\n",
            milliseconds as f64 / 1000.0
        ),
    )
    .expect("stub script");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    true
}

/// AC-24: a wait long enough to notice is named, and the line is given back before the header
/// prints. Without it the reader stares at a blank terminal for the seconds `pdftotext` takes.
#[cfg(target_os = "linux")]
#[test]
fn on_a_terminal_a_slow_open_is_named_and_erased() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let pdf = format!(
        "{}/tests/fixtures/two-chapters.pdf",
        env!("CARGO_MANIFEST_DIR")
    );
    sandbox.stdout_of(&["add", &pdf]);
    if !a_slow_pdftotext(&sandbox, 400) {
        return;
    }

    let endpoint = fake.endpoint();
    let raw = common::under_pty(
        &sandbox,
        &format!("read two-chapters --endpoint '{endpoint}' --sentences 1"),
    );

    let header = raw
        .find("— chapter")
        .unwrap_or_else(|| panic!("the opening header is printed: {raw:?}"));
    // The mark is drawn before the label, so the line is found from the mark backwards.
    let drawn = raw[..header]
        .rfind('⠋')
        .unwrap_or_else(|| panic!("the waiting mark is drawn on a terminal: {raw:?}"));
    let line = &raw[drawn..header];
    assert!(
        line.contains("reading a ") && line.contains("PDF"),
        "the line names what is being read: {line:?}"
    );
    // The erase has to land before the header row is written: the property is that the row the
    // header occupies carries none of the waiting line. Checking that the slice merely
    // *contains* a clear would pass on the frames' own row resets, which every redraw carries.
    let erased_at = raw[..header].rfind("\x1b[2K").unwrap_or_else(|| {
        panic!(
            "the header row starts with a row reset: {:?}",
            &raw[..header.min(400)]
        )
    });
    let row = &raw[erased_at + 4..header];
    assert!(
        !row.contains("reading a ") && !row.contains('⠇'),
        "the waiting line is gone before the header row is written: {row:?}"
    );
}

/// AC-24: a quick open draws no line at all. A row that appears and vanishes inside one frame
/// reads as a rendering fault, which is what the delay in `WAITING_DELAY` prevents.
#[cfg(target_os = "linux")]
#[test]
fn on_a_terminal_a_quick_open_draws_no_waiting_line() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "quick.epub",
        Some("Quick Book"),
        &[("One", "First sentence here.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let raw = common::under_pty(
        &sandbox,
        &format!("read quick --endpoint '{endpoint}' --sentences 1"),
    );

    assert!(
        raw.contains("██╗"),
        "the wordmark still goes out at once: {raw:?}"
    );
    assert!(
        !raw.contains("reading a "),
        "an open this fast gets no line, so nothing flashes: {raw:?}"
    );
}

#[test]
fn read_fills_in_sentence_counts_for_a_library_added_before_them() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "sized.epub",
        Some("Sized Book"),
        &[
            ("One", "First sentence. Second sentence."),
            ("Two", "Third sentence."),
        ],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    // Rewrite the index without the counts, exactly as a library written
    // before 1.3.0 would look.
    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let mut json: serde_json::Value = serde_json::from_str(&text).expect("json");
    json["books"][0]
        .as_object_mut()
        .expect("book object")
        .remove("sentence_counts");
    std::fs::write(sandbox.library_path(), json.to_string()).expect("rewrite library");

    let endpoint = fake.endpoint();
    sandbox.stdout_of(&read_args(
        "sized",
        &fake,
        &["--endpoint", &endpoint, "--sentences", "1"],
    ));

    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    let counts = json["books"][0]["sentence_counts"]
        .as_array()
        .expect("read filled the counts in");
    assert_eq!(counts.len(), 2, "one count per chapter: {counts:?}");
    assert!(
        counts.iter().all(|count| count.as_u64().unwrap_or(0) > 0),
        "both chapters were counted: {counts:?}"
    );
}

#[test]
fn read_adds_the_character_counts_a_1_3_0_index_lacks() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "chars.epub",
        Some("Charred Book"),
        &[
            ("One", "First sentence here. Second one."),
            ("Two", "Third."),
        ],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    // What 1.3.0 wrote: the sentence counts, no character counts.
    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let mut json: serde_json::Value = serde_json::from_str(&text).expect("json");
    json["books"][0]
        .as_object_mut()
        .expect("book object")
        .remove("chapter_chars");
    std::fs::write(sandbox.library_path(), json.to_string()).expect("rewrite library");

    let endpoint = fake.endpoint();
    let stdout = sandbox.stdout_of(&read_args(
        "charred",
        &fake,
        &["--endpoint", &endpoint, "--sentences", "1"],
    ));
    assert!(
        stdout.contains("~<1m left"),
        "three short sentences are under a minute: {stdout}"
    );

    let text = std::fs::read_to_string(sandbox.library_path()).expect("library");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    let chars = json["books"][0]["chapter_chars"]
        .as_array()
        .expect("read filled the character counts in");
    assert_eq!(chars.len(), 2, "one per chapter: {chars:?}");
    assert!(
        chars.iter().all(|count| count.as_u64().unwrap_or(0) > 0),
        "the estimate can be computed now: {chars:?}"
    );
    assert!(
        json["books"][0]["sentence_counts"].is_array(),
        "the counts 1.3.0 wrote are left alone"
    );
}
