//! AC-1, AC-2, AC-11: what `add` accepts, what it refuses, and when it does
//! nothing at all because the book is already there.

mod common;

use std::path::Path;

use common::Sandbox;

fn library_json(sandbox: &Sandbox) -> serde_json::Value {
    let text = std::fs::read_to_string(sandbox.library_path()).expect("library file exists");
    serde_json::from_str(&text).expect("library is valid json")
}

#[test]
fn add_creates_one_entry() {
    let sandbox = Sandbox::new();
    let book = sandbox.book(
        "grokking.epub",
        Some("Grokking Algorithms"),
        &[
            ("Chapter One", "An algorithm is a procedure."),
            ("Chapter Two", "Binary search halves the space."),
        ],
    );

    let stdout = sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    assert!(stdout.contains("added"), "got: {stdout}");
    assert!(stdout.contains("Grokking Algorithms"), "got: {stdout}");
    assert!(stdout.contains("2 chapters"), "got: {stdout}");

    let library = library_json(&sandbox);
    let books = library["books"].as_array().expect("books array");
    assert_eq!(books.len(), 1);

    let entry = &books[0];
    let id = entry["id"].as_str().expect("id");
    assert!(!id.is_empty());
    assert!(id.starts_with("grokking-algorithms-"), "id was {id}");
    assert_eq!(entry["title"], "Grokking Algorithms");
    assert_eq!(entry["author"], "Ursula K. Le Guin");
    assert_eq!(entry["chapter_count"], 2);
    assert!(
        entry["source_path"]
            .as_str()
            .expect("path")
            .ends_with("grokking.epub")
    );
}

#[test]
fn add_is_idempotent_by_content_hash() {
    let sandbox = Sandbox::new();
    let book = sandbox.book("book.epub", Some("Same Book"), &[("One", "Text.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    // a copy under a different name is still the same content
    let copy = sandbox.root().join("renamed-copy.epub");
    std::fs::copy(&book, &copy).expect("copy");

    let second = sandbox.stdout_of(&["add", copy.to_str().expect("path")]);
    assert!(second.contains("already in library"), "got: {second}");

    let library = library_json(&sandbox);
    assert_eq!(library["books"].as_array().expect("books").len(), 1);
}

#[test]
fn add_falls_back_to_the_file_name_when_the_epub_has_no_title() {
    let sandbox = Sandbox::new();
    let book = sandbox.book("Grokking Algorithms.epub", None, &[("One", "Text.")]);

    let stdout = sandbox.stdout_of(&["add", book.to_str().expect("path")]);
    assert!(stdout.contains("Grokking Algorithms"), "got: {stdout}");
    let library = library_json(&sandbox);
    assert_eq!(library["books"][0]["title"], "Grokking Algorithms");
}

#[test]
fn add_refuses_a_drm_protected_book() {
    let sandbox = Sandbox::new();
    let path = sandbox.root().join("locked.epub");
    let encryption = r#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData>
    <enc:CipherData><enc:CipherReference URI="OEBPS/ch0.xhtml"/></enc:CipherData>
  </enc:EncryptedData>
</encryption>"#;
    let package = common::opf(
        Some("Locked"),
        &["c0"],
        &[("c0", "text/ch0.xhtml", "application/xhtml+xml")],
    );
    common::write_epub(
        &path,
        &[
            ("META-INF/container.xml", common::CONTAINER),
            ("META-INF/encryption.xml", encryption),
            ("OEBPS/content.opf", &package),
            (
                "OEBPS/text/ch0.xhtml",
                &common::chapter("Locked", "Hidden."),
            ),
        ],
    );

    let output = sandbox.run(&["add", path.to_str().expect("path")]);
    assert_eq!(
        output.status.code(),
        Some(4),
        "DRM is a bad book, not a crash"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("encrypted"),
        "the message must say why"
    );
    assert!(
        !sandbox.library_path().exists(),
        "nothing is written on refusal"
    );
}

#[test]
fn add_reports_a_corrupt_library_instead_of_overwriting_it() {
    let sandbox = Sandbox::new();
    let book = sandbox.book("book.epub", Some("Book"), &[("One", "Text.")]);
    std::fs::create_dir_all(sandbox.library_path().parent().expect("parent")).expect("dir");
    std::fs::write(sandbox.library_path(), b"{ not json").expect("write garbage");

    let output = sandbox.run(&["add", book.to_str().expect("path")]);
    assert_eq!(output.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a valid library"));

    let untouched = std::fs::read_to_string(sandbox.library_path()).expect("still there");
    assert_eq!(
        untouched, "{ not json",
        "the file is never silently replaced"
    );
}

#[test]
fn add_reports_a_missing_file_without_creating_a_library() {
    let sandbox = Sandbox::new();
    let missing = sandbox.root().join("nope.epub");
    let output = sandbox.run(&["add", missing.to_str().expect("path")]);
    assert_eq!(output.status.code(), Some(6));
    assert!(!Path::new(&sandbox.library_path()).exists());
}
