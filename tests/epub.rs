//! EPUB-level behaviour: what `load` accepts, what it refuses, and how it
//! resolves the messy parts of the spec (relative hrefs, encryption, no title).
//!
//! Every fixture is built in a temp dir, so no book is ever committed.

mod common;

use std::fs::File;
use std::io::Write;

use common::{CONTAINER, chapter, opf, write_epub};
use libretto::epub::{self, EpubError};

#[test]
fn loads_metadata_and_skips_image_only_chapters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("book.epub");
    let package = opf(
        Some("The Dispossessed"),
        &["c1", "plate", "c2"],
        &[
            ("c1", "text/ch1.xhtml", "application/xhtml+xml"),
            ("plate", "text/plate.xhtml", "application/xhtml+xml"),
            ("c2", "text/ch2.xhtml", "application/xhtml+xml"),
        ],
    );
    write_epub(
        &path,
        &[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", &package),
            (
                "OEBPS/text/ch1.xhtml",
                &chapter("Chapter One", "She went. He stayed."),
            ),
            (
                "OEBPS/text/plate.xhtml",
                r#"<html><body><img src="map.png"/></body></html>"#,
            ),
            (
                "OEBPS/text/ch2.xhtml",
                &chapter("Chapter Two", "They left."),
            ),
        ],
    );

    let book = epub::load(&path).expect("loads");
    assert_eq!(book.title, "The Dispossessed");
    assert_eq!(book.author.as_deref(), Some("Ursula K. Le Guin"));
    assert_eq!(
        book.chapters.len(),
        2,
        "the plate page has no prose to speak"
    );
    assert_eq!(book.chapters[0].title.as_deref(), Some("Chapter One"));
    assert_eq!(book.chapters[0].text, "Chapter One\nShe went. He stayed.");
    assert_eq!(book.chapters[1].title.as_deref(), Some("Chapter Two"));
}

#[test]
fn falls_back_to_the_file_name_without_a_title() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("grokking algorithms.epub");
    let package = opf(
        None,
        &["c1"],
        &[("c1", "ch1.xhtml", "application/xhtml+xml")],
    );
    write_epub(
        &path,
        &[
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", &package),
            ("OEBPS/ch1.xhtml", &chapter("One", "Text.")),
        ],
    );

    let book = epub::load(&path).expect("loads");
    assert_eq!(book.title, "grokking algorithms");
}

#[test]
fn resolves_hrefs_relative_to_the_package_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested.epub");
    let package = opf(
        Some("Nested"),
        &["c1"],
        &[("c1", "../text/ch1.xhtml", "application/xhtml+xml")],
    );
    write_epub(
        &path,
        &[
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", &package),
            ("text/ch1.xhtml", &chapter("Moved", "Prose here.")),
        ],
    );

    let book = epub::load(&path).expect("loads");
    assert_eq!(book.chapters.len(), 1);
    assert!(book.chapters[0].text.contains("Prose here."));
}

#[test]
fn decodes_percent_encoded_hrefs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("encoded.epub");
    let package = opf(
        Some("Encoded"),
        &["c1"],
        &[("c1", "text/chapter%20one.xhtml", "application/xhtml+xml")],
    );
    write_epub(
        &path,
        &[
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", &package),
            ("OEBPS/text/chapter one.xhtml", &chapter("Spaced", "Words.")),
        ],
    );

    let book = epub::load(&path).expect("loads");
    assert_eq!(book.chapters[0].title.as_deref(), Some("Spaced"));
}

#[test]
fn refuses_drm_protected_documents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("locked.epub");
    let package = opf(
        Some("Locked"),
        &["c1"],
        &[("c1", "ch1.xhtml", "application/xhtml+xml")],
    );
    let encryption = r#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData>
    <enc:CipherData><enc:CipherReference URI="OEBPS/ch1.xhtml"/></enc:CipherData>
  </enc:EncryptedData>
</encryption>"#;
    write_epub(
        &path,
        &[
            ("META-INF/container.xml", CONTAINER),
            ("META-INF/encryption.xml", encryption),
            ("OEBPS/content.opf", &package),
            ("OEBPS/ch1.xhtml", &chapter("Locked", "Hidden.")),
        ],
    );

    assert!(matches!(epub::load(&path), Err(EpubError::Encrypted)));
}

#[test]
fn allows_font_obfuscation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fonts.epub");
    let package = opf(
        Some("Fonts"),
        &["c1"],
        &[("c1", "ch1.xhtml", "application/xhtml+xml")],
    );
    let encryption = r#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData>
    <enc:CipherData><enc:CipherReference URI="OEBPS/fonts/serif.otf"/></enc:CipherData>
  </enc:EncryptedData>
</encryption>"#;
    write_epub(
        &path,
        &[
            ("META-INF/container.xml", CONTAINER),
            ("META-INF/encryption.xml", encryption),
            ("OEBPS/content.opf", &package),
            ("OEBPS/ch1.xhtml", &chapter("Readable", "Fine.")),
        ],
    );

    let book = epub::load(&path).expect("font obfuscation is not DRM");
    assert_eq!(book.chapters.len(), 1);
}

#[test]
fn reports_a_book_with_no_readable_chapters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("plates.epub");
    let package = opf(
        Some("Plates"),
        &["plate"],
        &[("plate", "plate.xhtml", "application/xhtml+xml")],
    );
    write_epub(
        &path,
        &[
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", &package),
            (
                "OEBPS/plate.xhtml",
                r#"<html><body><img src="p.png"/></body></html>"#,
            ),
        ],
    );

    assert!(matches!(epub::load(&path), Err(EpubError::NoChapters)));
}

#[test]
fn refuses_an_entry_that_decompresses_past_the_ceiling() {
    // 20 MiB of zeros is a few KB in the container but 20 MiB in memory: the
    // declared size is attacker-controlled, so the read has to be bounded.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bomb.epub");
    let package = opf(
        Some("Bomb"),
        &["c1"],
        &[("c1", "ch1.xhtml", "application/xhtml+xml")],
    );
    let payload = "a".repeat(20 * 1024 * 1024);

    let file = File::create(&path).expect("create");
    let mut zip = zip::ZipWriter::new(file);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("META-INF/container.xml", stored)
        .expect("start");
    zip.write_all(CONTAINER.as_bytes()).expect("write");
    zip.start_file("OEBPS/content.opf", stored).expect("start");
    zip.write_all(package.as_bytes()).expect("write");
    zip.start_file("OEBPS/ch1.xhtml", deflated).expect("start");
    zip.write_all(payload.as_bytes()).expect("write");
    zip.finish().expect("finish");

    assert!(
        matches!(epub::load(&path), Err(EpubError::TooLarge { .. })),
        "an entry that inflates to 20 MiB must be refused, not allocated"
    );
}

#[test]
fn accepts_a_chapter_far_below_the_ceiling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big-but-fine.epub");
    let package = opf(
        Some("Big"),
        &["c1"],
        &[("c1", "ch1.xhtml", "application/xhtml+xml")],
    );
    let prose = "word ".repeat(200_000); // ~1 MB, a long chapter, not an attack

    let file = File::create(&path).expect("create");
    let mut zip = zip::ZipWriter::new(file);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("META-INF/container.xml", deflated)
        .expect("start");
    zip.write_all(CONTAINER.as_bytes()).expect("write");
    zip.start_file("OEBPS/content.opf", deflated)
        .expect("start");
    zip.write_all(package.as_bytes()).expect("write");
    zip.start_file("OEBPS/ch1.xhtml", deflated).expect("start");
    zip.write_all(chapter("Long", &prose).as_bytes())
        .expect("write");
    zip.finish().expect("finish");

    let book = epub::load(&path).expect("a 1 MB chapter is a normal book");
    assert!(book.chapters[0].text.len() > 100_000);
}

#[test]
fn reports_a_file_that_is_not_an_epub() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("notes.epub");
    std::fs::write(&path, b"just text, not a zip").expect("write");

    assert!(matches!(epub::load(&path), Err(EpubError::Archive { .. })));
}
