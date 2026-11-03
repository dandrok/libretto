//! PDF import, end to end, against a generated fixture.
//!
//! The fixture is committed (`tests/fixtures/two-chapters.pdf`, made with ghostscript)
//! so these tests never depend on a book someone happens to own, and so the awkward
//! parts of a real page — running head, page number, figure caption, a paragraph split
//! by a page break — are present on purpose.

mod common;

use common::Sandbox;
use common::fake_tts::{FakeTts, Reply};

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// `pdftotext` is a system tool; without it PDFs are unsupported and the test says why
/// instead of pretending the code is broken.
fn have_pdftotext() -> bool {
    std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .is_ok()
}

#[test]
fn a_pdf_is_indexed_with_the_chapters_its_running_heads_describe() {
    if !have_pdftotext() {
        eprintln!("skipping: pdftotext not installed");
        return;
    }
    let sandbox = Sandbox::new();
    let path = fixture("two-chapters.pdf");

    let stdout = sandbox.stdout_of(&["add", &path]);
    assert!(
        stdout.contains("(2 chapters)"),
        "two running heads, two chapters: {stdout}"
    );
    assert!(
        stdout.contains("two-chapters"),
        "falls back to the file name when the PDF has no title metadata: {stdout}"
    );

    let listing = sandbox.stdout_of(&["ls"]);
    assert!(
        listing.contains("2 chapters") || listing.contains("ch 1/2"),
        "{listing}"
    );
}

#[test]
fn reading_a_pdf_speaks_the_prose_and_not_the_page_furniture() {
    if !have_pdftotext() {
        eprintln!("skipping: pdftotext not installed");
        return;
    }
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(b"ID3".to_vec()));
    let path = fixture("two-chapters.pdf");
    sandbox.stdout_of(&["add", &path]);

    let endpoint = fake.endpoint();
    let stdout = sandbox.stdout_of(&[
        "read",
        "two-chapters",
        "--sentences",
        "1",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
    ]);

    assert!(stdout.contains("1/"), "played one sentence: {stdout}");
    let body = fake.last_body();
    assert!(
        !body.contains("CHAPTER 1"),
        "the running head must not be synthesized: {body}"
    );
    assert!(
        !body.contains("Figure 1.1"),
        "a figure caption must not be synthesized: {body}"
    );
    assert!(
        body.contains("layout wrapped across lines"),
        "the first sentence is the prose: {body}"
    );
}

#[test]
fn a_pdf_without_a_text_layer_is_refused_with_a_reason() {
    if !have_pdftotext() {
        eprintln!("skipping: pdftotext not installed");
        return;
    }
    let sandbox = Sandbox::new();
    let path = fixture("no-text.pdf");

    let output = sandbox.run(&["add", &path]);
    assert_eq!(
        output.status.code(),
        Some(4),
        "an unreadable book is exit 4; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no text layer") || stderr.contains("no readable text"),
        "says what is wrong instead of failing silently: {stderr}"
    );
    assert!(
        !sandbox.library_path().exists() || sandbox.stdout_of(&["ls"]).contains("no books"),
        "a refused book leaves no entry behind"
    );
}

#[test]
fn an_epub_still_goes_through_the_epub_reader() {
    // The dispatch is by extension: a regression here would send EPUBs to pdftotext.
    let sandbox = Sandbox::new();
    let path = sandbox.book(
        "chapters.epub",
        Some("A Test Book"),
        &[("One", "First sentence here.")],
    );
    let path = path.display().to_string();

    let stdout = sandbox.stdout_of(&["add", &path]);
    assert!(stdout.contains("1 chapters"), "{stdout}");
}
