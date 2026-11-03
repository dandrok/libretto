//! Shared test scaffolding: a sandboxed library and EPUBs built in a temp dir.
//!
//! No book is ever committed, and no test reads the real `~/.local/share`: the
//! CLI is spawned with `XDG_*` pointing into a temp dir, so tests stay parallel.

#![allow(dead_code)]

pub mod fake_tts;

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

pub fn write_epub(path: &Path, entries: &[(&str, &str)]) {
    let file = File::create(path).expect("create epub");
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in entries {
        zip.start_file(*name, options).expect("start entry");
        zip.write_all(body.as_bytes()).expect("write entry");
    }
    zip.finish().expect("finish epub");
}

pub fn opf(title: Option<&str>, spine: &[&str], manifest: &[(&str, &str, &str)]) -> String {
    let title = title
        .map(|t| format!("<dc:title>{t}</dc:title>"))
        .unwrap_or_default();
    let items: String = manifest
        .iter()
        .map(|(id, href, media)| format!(r#"<item id="{id}" href="{href}" media-type="{media}"/>"#))
        .collect();
    let refs: String = spine
        .iter()
        .map(|id| format!(r#"<itemref idref="{id}"/>"#))
        .collect();
    format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    {title}<dc:creator>Ursula K. Le Guin</dc:creator>
  </metadata>
  <manifest>{items}</manifest>
  <spine>{refs}</spine>
</package>"#
    )
}

pub fn chapter(heading: &str, prose: &str) -> String {
    format!(
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1>{heading}</h1><p>{prose}</p></body></html>"#
    )
}

/// A sandboxed `$XDG_*` tree plus the compiled binary.
pub struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("data")).expect("data dir");
        std::fs::create_dir_all(root.path().join("cache")).expect("cache dir");
        std::fs::create_dir_all(root.path().join("config")).expect("config dir");
        Self { root }
    }

    pub fn library_path(&self) -> PathBuf {
        self.root.path().join("data/libretto/library.json")
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// One chapter per entry, all with a title.
    pub fn book(&self, file_name: &str, title: Option<&str>, chapters: &[(&str, &str)]) -> PathBuf {
        let path = self.root.path().join(file_name);
        let manifest: Vec<(String, String, &str)> = chapters
            .iter()
            .enumerate()
            .map(|(i, _)| {
                (
                    format!("c{i}"),
                    format!("text/ch{i}.xhtml"),
                    "application/xhtml+xml",
                )
            })
            .collect();
        let manifest_refs: Vec<(&str, &str, &str)> = manifest
            .iter()
            .map(|(id, href, media)| (id.as_str(), href.as_str(), *media))
            .collect();
        let spine: Vec<String> = manifest.iter().map(|(id, _, _)| id.clone()).collect();
        let spine_refs: Vec<&str> = spine.iter().map(String::as_str).collect();
        let package = opf(title, &spine_refs, &manifest_refs);

        let mut entries: Vec<(String, String)> = vec![
            ("mimetype".to_string(), "application/epub+zip".to_string()),
            ("META-INF/container.xml".to_string(), CONTAINER.to_string()),
            ("OEBPS/content.opf".to_string(), package),
        ];
        for (i, (heading, prose)) in chapters.iter().enumerate() {
            entries.push((format!("OEBPS/text/ch{i}.xhtml"), chapter(heading, prose)));
        }
        let borrowed: Vec<(&str, &str)> = entries
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_str()))
            .collect();
        write_epub(&path, &borrowed);
        path
    }

    /// Run the binary with the sandbox in charge of every path it can touch.
    pub fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_libretto"))
            .args(args)
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("HOME", self.root.path())
            .output()
            .expect("run libretto")
    }

    pub fn stdout_of(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "expected success for {args:?}, got {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}
