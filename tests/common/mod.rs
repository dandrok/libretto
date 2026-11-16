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

    /// The directory the binary treats as `$XDG_DATA_HOME/libretto`.
    pub fn data_dir(&self) -> PathBuf {
        self.root.path().join("data/libretto")
    }

    /// Pre-fill the library without importing 50 EPUBs: `ls` only reads the
    /// index, so the index is what a performance test should build.
    pub fn seed_library(&self, count: usize) {
        use libretto::store::{BookRecord, Library, Progress, Store, now_seconds};

        let mut library = Library::default();
        for index in 0..count {
            let id = format!("book-{index:02}-aaaa");
            library.insert(BookRecord {
                id: id.clone(),
                title: format!("Book number {index} with a title of realistic length"),
                author: Some("An Author".to_string()),
                source_path: format!("/books/{index}.epub"),
                content_hash: format!("hash{index:04}"),
                chapter_count: 12,
                sentence_counts: vec![40; 12],
                chapter_chars: vec![4000; 12],
                added_at: now_seconds(),
            });
            if index % 2 == 0 {
                library.progress.insert(
                    id,
                    Progress {
                        chapter_index: index % 12,
                        sentence_index: index * 3,
                        updated_at: now_seconds(),
                    },
                );
            }
        }
        library.last_read = Some("book-00-aaaa".to_string());
        Store::at(self.data_dir())
            .save(&library)
            .expect("seed library");
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

/// Run the binary under a real pty, which is the only way `stderr` counts as a terminal.
///
/// `script` is util-linux's (`-qec`), so this helper is Linux-only rather than pretending
/// about macOS. A missing `script` is a panic that names the reason: skipping the proof
/// silently would be indistinguishable from one that passed.
///
/// The player is a script that sleeps, because `--player true` exits before the first frame
/// is ever drawn — a one-sentence run under it renders nothing at all.
///
/// `$PATH` starts with `<root>/bin`, so a test that needs a slow or blocked external tool puts
/// a stub there instead of reaching into the program.
#[cfg(target_os = "linux")]
pub fn under_pty(sandbox: &Sandbox, args: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    if !Path::new("/usr/bin/script").exists() && !Path::new("/bin/script").exists() {
        panic!(
            "no `script` binary: this proof cannot run, and skipping it silently would hide that"
        );
    }

    let root = sandbox.root().display();
    let player = sandbox.root().join("slow-player");
    if !player.exists() {
        std::fs::write(&player, "#!/bin/sh\nsleep 1\n").expect("player script");
        std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let inner = format!(
        "env XDG_DATA_HOME='{root}/data' XDG_CACHE_HOME='{root}/cache' \
             XDG_CONFIG_HOME='{root}/config' HOME='{root}' COLUMNS=60 \
             PATH='{root}/bin':$PATH \
             '{}' {args} --player '{}' --width 60",
        env!("CARGO_BIN_EXE_libretto"),
        player.display(),
    );
    let output = Command::new("script")
        .args(["-qec", &inner, "/dev/null"])
        .output()
        .expect("run under a pty");
    String::from_utf8_lossy(&output.stdout).to_string()
}
