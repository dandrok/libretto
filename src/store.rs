//! The library: which books exist, where the reader stopped, and when.
//!
//! One JSON file, written atomically, guarded by a lock file. It is deliberately
//! boring: a library a human can open, diff and repair by hand beats a database
//! for twenty books, and a corrupt file is reported rather than overwritten.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths;

pub const LIBRARY_FILE: &str = "library.json";
const LOCK_FILE: &str = "library.lock";
const VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cannot reach the library: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error(
        "{path} is not a valid library: {reason}; fix or move the file, it will not be overwritten"
    )]
    Corrupt { path: String, reason: String },
    #[error("cannot write the library: {0}")]
    Encode(String),
    #[error("unknown book")]
    UnknownBook,
    #[error("ambiguous id '{prefix}', matches: {matches}")]
    Ambiguous { prefix: String, matches: String },
    #[error("already reading another book")]
    AlreadyLocked,
    #[error(transparent)]
    Path(#[from] paths::PathError),
}

/// One book, as the library remembers it. `chapter_count` is denormalised so
/// `ls` never has to open an EPUB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookRecord {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    pub source_path: String,
    pub content_hash: String,
    pub chapter_count: usize,
    pub added_at: u64,
}

/// Where the reader stopped. Sentence granularity, because a chapter is 20 minutes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub chapter_index: usize,
    pub sentence_index: usize,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default)]
    pub books: Vec<BookRecord>,
    #[serde(default)]
    pub progress: BTreeMap<String, Progress>,
    #[serde(default)]
    pub last_read: Option<String>,
}

fn current_version() -> u32 {
    VERSION
}

impl Default for Library {
    fn default() -> Self {
        Self {
            version: VERSION,
            books: Vec::new(),
            progress: BTreeMap::new(),
            last_read: None,
        }
    }
}

impl Library {
    pub fn find_by_hash(&self, hash: &str) -> Option<&BookRecord> {
        self.books.iter().find(|book| book.content_hash == hash)
    }

    /// Resolve a full id or a unique prefix, so `libretto read grok` works.
    pub fn resolve(&self, prefix: &str) -> Result<&BookRecord, StoreError> {
        let needle = prefix.trim().to_lowercase();
        if needle.is_empty() {
            return Err(StoreError::UnknownBook);
        }
        let matches: Vec<&BookRecord> = self
            .books
            .iter()
            .filter(|book| book.id.to_lowercase().starts_with(&needle))
            .collect();
        match matches.as_slice() {
            [] => Err(StoreError::UnknownBook),
            [only] => Ok(only),
            many => Err(StoreError::Ambiguous {
                prefix: prefix.to_string(),
                matches: many
                    .iter()
                    .map(|book| book.id.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }

    pub fn remove(&mut self, id: &str) -> Result<BookRecord, StoreError> {
        let book = self.resolve(id)?.clone();
        self.books.retain(|candidate| candidate.id != book.id);
        self.progress.remove(&book.id);
        if self.last_read.as_deref() == Some(book.id.as_str()) {
            self.last_read = None;
        }
        Ok(book)
    }

    pub fn insert(&mut self, record: BookRecord) {
        self.books.push(record);
        // cached key: lowercase mapping costs one allocation per book, not per comparison
        self.books
            .sort_by_cached_key(|book| book.title.to_lowercase());
    }

    pub fn progress_of(&self, id: &str) -> Progress {
        self.progress.get(id).copied().unwrap_or_default()
    }

    /// Fraction of the book behind the reader, by chapter. Sentence-level
    /// precision would need the chapter's length, which is not in the record.
    pub fn percent_of(&self, record: &BookRecord) -> u8 {
        if record.chapter_count == 0 {
            return 0;
        }
        let chapter = self.progress_of(&record.id).chapter_index;
        (chapter.saturating_mul(100) / record.chapter_count).min(100) as u8
    }
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The real library, under `$XDG_DATA_HOME/libretto`.
    pub fn open() -> Result<Self, StoreError> {
        Ok(Self {
            dir: paths::data_dir()?,
        })
    }

    /// A library at a chosen directory — what tests use.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn library_path(&self) -> PathBuf {
        self.dir.join(LIBRARY_FILE)
    }

    pub fn load(&self) -> Result<Library, StoreError> {
        let path = self.library_path();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| StoreError::Corrupt {
                path: path.display().to_string(),
                reason: error.to_string(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Library::default()),
            Err(source) => Err(StoreError::Io { source }),
        }
    }

    /// Write to a sibling temp file, then rename: a crash mid-write leaves the
    /// previous library intact rather than a truncated one.
    pub fn save(&self, library: &Library) -> Result<(), StoreError> {
        self.ensure_dir()?;
        let target = self.library_path();
        let temp = self.dir.join(format!("{LIBRARY_FILE}.tmp"));
        let body = serde_json::to_vec_pretty(library)
            .map_err(|error| StoreError::Encode(error.to_string()))?;

        {
            let mut file = self.create_private(&temp)?;
            restrict_permissions(&file)?;
            file.write_all(&body)
                .and_then(|()| file.sync_all())
                .map_err(|source| StoreError::Io { source })?;
        }
        fs::rename(&temp, &target).map_err(|source| StoreError::Io { source })
    }

    /// The data directory holds a list of what someone reads: owner only.
    fn ensure_dir(&self) -> Result<(), StoreError> {
        fs::create_dir_all(&self.dir).map_err(|source| StoreError::Io { source })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
                .map_err(|source| StoreError::Io { source })?;
        }
        Ok(())
    }

    /// `O_EXCL`, never "open whatever is at this path": a symlink planted where
    /// the temp file goes must fail instead of being followed onto another file.
    fn create_private(&self, path: &Path) -> Result<fs::File, StoreError> {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(path); // left behind by a crash
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|source| StoreError::Io { source })
            }
            Err(source) => Err(StoreError::Io { source }),
        }
    }

    /// Take the "one reader at a time" lock. Dropping it releases the lock.
    pub fn lock(&self) -> Result<StoreLock, StoreError> {
        self.ensure_dir()?;
        let path = self.dir.join(LOCK_FILE);
        for attempt in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(StoreLock { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // A killed process leaves the file behind; a stale lock must
                    // not make the tool permanently unusable.
                    if attempt == 0 && !lock_is_live(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Err(StoreError::AlreadyLocked);
                }
                Err(source) => return Err(StoreError::Io { source }),
            }
        }
        Err(StoreError::AlreadyLocked)
    }
}

/// Is the process that wrote this lock still running?
fn lock_is_live(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false; // unreadable lock: treat as stale rather than wedging
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    if pid == std::process::id() {
        return true;
    }
    if !Path::new("/proc").is_dir() {
        return true; // no way to tell: assume live, refuse rather than double-play
    }
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The library lists what someone reads, so keep it private to them.
fn restrict_permissions(file: &fs::File) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| StoreError::Io { source })?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

pub struct StoreLock {
    path: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Content identity for dedupe: the file's bytes, not its name or mtime.
pub fn hash_file(path: &Path) -> Result<String, StoreError> {
    let bytes = fs::read(path).map_err(|source| StoreError::Io { source })?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// A readable, stable id: `grokking-algorithms-2nd-9f3a`. The hash suffix keeps
/// two editions of the same title apart.
pub fn make_id(title: &str, hash: &str) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let stem: String = slug.chars().take(28).collect();
    let stem = stem.trim_matches('-');
    let stem = if stem.is_empty() { "book" } else { stem };
    let suffix: String = hash.chars().take(4).collect();
    format!("{stem}-{suffix}")
}

pub fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, hash: &str) -> BookRecord {
        BookRecord {
            id: id.to_string(),
            title: id.to_string(),
            author: None,
            source_path: "/tmp/x.epub".to_string(),
            content_hash: hash.to_string(),
            chapter_count: 10,
            added_at: 0,
        }
    }

    #[test]
    fn ids_are_readable_and_end_with_the_hash() {
        let id = make_id("Grokking Algorithms, 2nd Edition", "abcd1234");
        assert!(id.starts_with("grokking-algorithms-2nd"), "{id}");
        assert!(
            id.ends_with("-abcd"),
            "the hash suffix keeps editions apart: {id}"
        );
        assert!(id.chars().count() <= 33, "ids stay typeable: {id}");
        assert!(!id.contains("--") && !id.ends_with('-'), "{id}");
        assert_eq!(make_id("  ", "abcd1234"), "book-abcd");
        assert!(make_id("!!!", "ff00aa").starts_with("book-"));
    }

    #[test]
    fn resolve_accepts_a_unique_prefix_only() {
        let mut library = Library::default();
        library.insert(record("grokking-algorithms-ab12", "h1"));
        library.insert(record("groovy-in-action-cd34", "h2"));

        assert!(library.resolve("grok").is_ok());
        assert!(matches!(
            library.resolve("gro"),
            Err(StoreError::Ambiguous { .. })
        ));
        assert!(matches!(
            library.resolve("nope"),
            Err(StoreError::UnknownBook)
        ));
    }

    #[test]
    fn removing_a_book_clears_its_progress_and_last_read() {
        let mut library = Library::default();
        library.insert(record("alpha-1111", "h1"));
        library.progress.insert(
            "alpha-1111".into(),
            Progress {
                chapter_index: 2,
                sentence_index: 5,
                updated_at: 0,
            },
        );
        library.last_read = Some("alpha-1111".into());

        library.remove("alpha").expect("removes");
        assert!(library.books.is_empty());
        assert!(library.progress.is_empty());
        assert_eq!(library.last_read, None);
    }

    #[test]
    fn a_dropped_lock_can_be_taken_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path());
        {
            let _held = store.lock().expect("first lock");
            assert!(matches!(store.lock(), Err(StoreError::AlreadyLocked)));
        }
        assert!(store.lock().is_ok(), "the lock is released on drop");
    }

    #[test]
    fn a_stale_lock_from_a_dead_process_is_recovered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path());
        std::fs::create_dir_all(dir.path()).expect("dir");
        // pid 4194304 is above the usual pid_max: no such process
        std::fs::write(dir.path().join("library.lock"), b"4194304\n").expect("write lock");

        assert!(
            store.lock().is_ok(),
            "a killed reader must not wedge the tool"
        );
    }

    #[test]
    fn a_lock_held_by_this_process_is_respected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path());
        std::fs::create_dir_all(dir.path()).expect("dir");
        std::fs::write(
            dir.path().join("library.lock"),
            format!("{}\n", std::process::id()),
        )
        .expect("write lock");

        assert!(matches!(store.lock(), Err(StoreError::AlreadyLocked)));
    }

    #[test]
    fn the_data_directory_is_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path().join("libretto"));
        store.save(&Library::default()).expect("saves");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.dir())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "the directory name alone leaks reading habits"
            );
        }
    }

    #[test]
    fn a_corrupt_library_is_reported_not_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path());
        fs::create_dir_all(dir.path()).expect("dir");
        fs::write(store.library_path(), b"{ not json").expect("write");

        assert!(matches!(store.load(), Err(StoreError::Corrupt { .. })));
        let on_disk = fs::read_to_string(store.library_path()).expect("still there");
        assert_eq!(on_disk, "{ not json");
    }

    #[test]
    fn saving_is_atomic_and_private() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path());
        let mut library = Library::default();
        library.insert(record("alpha-1111", "h1"));
        store.save(&library).expect("saves");

        assert!(store.library_path().exists());
        assert!(!dir.path().join(format!("{LIBRARY_FILE}.tmp")).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.library_path())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the library lists what you read");
        }
        let reloaded = store.load().expect("reloads");
        assert_eq!(reloaded.books.len(), 1);
    }
}
