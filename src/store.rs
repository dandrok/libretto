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
/// A library this large is not a library. Parsing it would be an allocation an
/// unreadable file chose for us.
const MAX_LIBRARY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cannot reach the library: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    /// The book file itself could not be read. Separate from `Io` because "cannot
    /// reach the library" is the wrong sentence for a path the shell never resolved.
    #[error("cannot read {path}: {source}")]
    Unreadable {
        path: String,
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
    /// Sentences per chapter, in reading order, counted with the same splitter
    /// `read` uses, so `ls` can report a position inside a chapter. Records
    /// written before 1.3.0 have no counts and fall back to chapter boundaries.
    #[serde(default)]
    pub sentence_counts: Vec<usize>,
    /// Characters per chapter, summed over the sentences `read` will speak. Together
    /// with `sentence_counts` this is what a time estimate needs: how many characters
    /// are still ahead. Empty in records written before 1.4.0.
    #[serde(default)]
    pub chapter_chars: Vec<usize>,
    pub added_at: u64,
}

impl BookRecord {
    /// How far into the book the stored position is, in whole percent. `None`
    /// when this record cannot answer exactly: no counts at all (a library
    /// written before 1.3.0), or a count list that disagrees with
    /// `chapter_count` (a hand-edited index).
    pub fn sentence_percent(&self, progress: Progress) -> Option<u8> {
        let total: usize = self.sentence_counts.iter().sum();
        if total == 0 || self.sentence_counts.len() != self.chapter_count {
            return None;
        }
        let behind: usize = self
            .sentence_counts
            .iter()
            .take(progress.chapter_index)
            .sum();
        let within = self
            .sentence_counts
            .get(progress.chapter_index)
            .copied()
            .unwrap_or(0)
            .min(progress.sentence_index);
        Some((behind.saturating_add(within).saturating_mul(100) / total).min(100) as u8)
    }

    /// Whether the two count lists describe the book `chapter_count` claims. A stale
    /// or hand-edited index answers `false` instead of producing a confident number.
    fn counts_agree(&self) -> bool {
        self.chapter_count > 0
            && self.chapter_count == self.sentence_counts.len()
            && self.chapter_count == self.chapter_chars.len()
    }

    /// Characters still to speak from the stored position — what `ls` needs, and it
    /// has the index and nothing else. Inside the current chapter it assumes every
    /// sentence is the same length, because the text is not open; `read` counts the
    /// spoken prefix exactly and does not go through here.
    ///
    /// `None` for a record written before the counts existed, and for a book whose
    /// chapters hold no measurable text.
    pub fn chars_left(&self, progress: Progress) -> Option<usize> {
        if !self.counts_agree() {
            return None;
        }
        let total: usize = self.chapter_chars.iter().sum();
        if total == 0 {
            return None;
        }
        let behind: usize = self.chapter_chars.iter().take(progress.chapter_index).sum();
        let chapter_chars = self
            .chapter_chars
            .get(progress.chapter_index)
            .copied()
            .unwrap_or(0);
        let chapter_sentences = self
            .sentence_counts
            .get(progress.chapter_index)
            .copied()
            .unwrap_or(0);
        let within = chapter_chars
            .saturating_mul(progress.sentence_index.min(chapter_sentences))
            .checked_div(chapter_sentences)
            .unwrap_or(0);
        Some(total.saturating_sub(behind.saturating_add(within)))
    }
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

    /// Fraction of the book behind the reader. Sentence-exact when the record
    /// carries `sentence_counts` (every book added since 1.3.0); a record
    /// without them gets chapter granularity, which is all the index alone can
    /// support.
    pub fn percent_of(&self, record: &BookRecord) -> u8 {
        let progress = self.progress_of(&record.id);
        if let Some(percent) = record.sentence_percent(progress) {
            return percent;
        }
        if record.chapter_count == 0 {
            return 0;
        }
        (progress.chapter_index.saturating_mul(100) / record.chapter_count).min(100) as u8
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
        match fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_LIBRARY_BYTES => {
                return Err(StoreError::Corrupt {
                    path: path.display().to_string(),
                    reason: format!("{} bytes is larger than any real library", meta.len()),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Library::default());
            }
            Err(source) => return Err(StoreError::Io { source }),
        }
        let text = fs::read_to_string(&path).map_err(|source| StoreError::Io { source })?;
        serde_json::from_str(&text).map_err(|error| StoreError::Corrupt {
            path: path.display().to_string(),
            reason: error.to_string(),
        })
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
    lock_is_live_in(path, Path::new("/proc"))
}

/// The `/proc` root is a parameter so the "cannot tell" branch is testable on
/// Linux, where it would otherwise only ever run on someone else's machine.
fn lock_is_live_in(path: &Path, proc_root: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false; // unreadable lock: treat as stale rather than wedging
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    if pid == std::process::id() {
        return true;
    }
    if !proc_root.is_dir() {
        return true; // no way to tell: assume live, refuse rather than double-play
    }
    proc_root.join(pid.to_string()).exists()
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
    let bytes = fs::read(path).map_err(|source| StoreError::Unreadable {
        path: describe_path(path),
        source,
    })?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// The path as the filesystem saw it, not as it was typed.
///
/// A relative path resolves against the real working directory, and `~/git/libretto`
/// is a symlink into a sandbox — so `../../Downloads/book.pdf` can mean something very
/// different from what the prompt suggests. Printing the resolved path is what turns
/// "no such file" into "that is not where you think you are".
fn describe_path(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).ok().or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|directory| lexical(&directory.join(path)))
    });
    match resolved {
        Some(absolute) if absolute != path => {
            format!("{} (resolved to {})", path.display(), absolute.display())
        }
        _ => path.display().to_string(),
    }
}

/// `a/b/../c` → `a/c`, so the reported path is one a person can read. `..` at the
/// root is dropped, which is what the kernel does too.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
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
            sentence_counts: Vec::new(),
            chapter_chars: Vec::new(),
            added_at: 0,
        }
    }

    /// A record that can answer exactly: one count per chapter, as given.
    fn sized(id: &str, counts: &[usize]) -> BookRecord {
        BookRecord {
            id: id.to_string(),
            chapter_count: counts.len(),
            sentence_counts: counts.to_vec(),
            ..record(id, "hash")
        }
    }

    /// A record carrying both lists: sentences and characters, per chapter.
    fn shaped(id: &str, sentences: &[usize], chars: &[usize]) -> BookRecord {
        BookRecord {
            id: id.to_string(),
            chapter_count: sentences.len(),
            sentence_counts: sentences.to_vec(),
            chapter_chars: chars.to_vec(),
            ..record(id, "hash")
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

        let recovered = store.lock().is_ok();

        if cfg!(target_os = "linux") {
            assert!(recovered, "a killed reader must not wedge the tool");
        } else {
            // Without /proc there is no way to prove a pid is dead, so the
            // documented behaviour is to assume the lock is live. The macOS CI
            // job asserts that refusal instead of demanding recovery.
            assert!(
                !recovered,
                "no /proc means assume live, never steal a possibly-live lock"
            );
            assert!(matches!(store.lock(), Err(StoreError::AlreadyLocked)));
        }
    }

    #[test]
    fn without_a_proc_root_a_foreign_pid_is_assumed_alive() {
        // This is the macOS path, exercised on Linux by pointing the check at a
        // directory that does not exist. Refusing is the safe answer: stealing a
        // live lock means two readers on one position.
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join("library.lock");
        std::fs::write(&lock, b"4194304\n").expect("write lock");
        let absent_proc = dir.path().join("no-proc-here");

        assert!(lock_is_live_in(&lock, &absent_proc));
    }

    #[test]
    fn our_own_pid_is_always_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join("library.lock");
        std::fs::write(&lock, format!("{}\n", std::process::id())).expect("write lock");
        assert!(lock_is_live_in(&lock, &dir.path().join("no-proc-here")));
        assert!(lock_is_live(&lock));
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
    fn a_symlink_planted_at_the_temp_path_is_not_followed() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let dir = tempfile::tempdir().expect("tempdir");
            let store = Store::at(dir.path());
            let victim = dir.path().join("victim.txt");
            fs::write(&victim, b"do not touch").expect("victim");
            symlink(&victim, dir.path().join("library.json.tmp")).expect("plant link");

            store
                .save(&Library::default())
                .expect("save replaces the link");

            assert_eq!(
                fs::read_to_string(&victim).expect("victim readable"),
                "do not touch",
                "a pre-planted symlink must not redirect the write"
            );
        }
    }

    #[test]
    fn an_absurdly_large_library_is_refused_before_parsing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::at(dir.path());
        fs::create_dir_all(dir.path()).expect("dir");
        let body = "x".repeat((MAX_LIBRARY_BYTES + 1) as usize);
        fs::write(store.library_path(), body).expect("write");

        assert!(matches!(store.load(), Err(StoreError::Corrupt { .. })));
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

    #[test]
    fn a_percentage_lands_inside_the_chapter_the_reader_is_in() {
        let mut library = Library::default();
        let book = sized("sized-1111", &[10, 10, 10]);
        library.progress.insert(
            book.id.clone(),
            Progress {
                chapter_index: 1,
                sentence_index: 5,
                updated_at: 0,
            },
        );
        assert_eq!(
            library.percent_of(&book),
            50,
            "15 of 30 sentences — not the 33% a chapter count alone gives"
        );
    }

    #[test]
    fn the_last_sentence_of_a_book_is_one_hundred_percent() {
        let mut library = Library::default();
        let book = sized("sized-2222", &[10, 10, 10]);
        library.progress.insert(
            book.id.clone(),
            Progress {
                chapter_index: 2,
                sentence_index: 10,
                updated_at: 0,
            },
        );
        assert_eq!(library.percent_of(&book), 100);
    }

    #[test]
    fn a_position_past_the_end_does_not_overflow_the_percentage() {
        let mut library = Library::default();
        let book = sized("sized-3333", &[4, 4]);
        library.progress.insert(
            book.id.clone(),
            Progress {
                chapter_index: 9,
                sentence_index: 9999,
                updated_at: 0,
            },
        );
        assert_eq!(library.percent_of(&book), 100, "clamped, never 250%");
    }

    #[test]
    fn a_record_without_counts_still_reports_by_chapter() {
        let mut library = Library::default();
        let book = record("old-4444", "h2");
        library.progress.insert(
            book.id.clone(),
            Progress {
                chapter_index: 2,
                sentence_index: 40,
                updated_at: 0,
            },
        );
        assert_eq!(library.percent_of(&book), 20, "2 of 10 chapters");
    }

    #[test]
    fn a_count_list_that_disagrees_with_the_chapter_count_is_ignored() {
        let mut library = Library::default();
        let mut book = record("odd-5555", "h3");
        book.sentence_counts = vec![10, 10, 10];
        library.progress.insert(
            book.id.clone(),
            Progress {
                chapter_index: 1,
                sentence_index: 0,
                updated_at: 0,
            },
        );
        assert_eq!(
            library.percent_of(&book),
            10,
            "three counts for ten chapters is a stale index, not a position"
        );
    }

    #[test]
    fn a_library_written_before_sentence_counts_still_loads() {
        let json = r#"{"version":1,"books":[{"id":"a-1111","title":"A","author":null,"source_path":"/tmp/a.epub","content_hash":"h","chapter_count":2,"added_at":0}],"progress":{},"last_read":null}"#;
        let library: Library = serde_json::from_str(json).expect("an older index loads");
        assert!(
            library.books[0].sentence_counts.is_empty(),
            "absent, not invented"
        );
    }

    #[test]
    fn time_left_shrinks_as_the_reader_moves() {
        let book = shaped("timed-1111", &[10, 10], &[1000, 1000]);
        let start = Progress::default();
        assert_eq!(book.chars_left(start), Some(2000), "whole book ahead");
        let halfway_in = Progress {
            chapter_index: 1,
            sentence_index: 5,
            updated_at: 0,
        };
        assert_eq!(
            book.chars_left(halfway_in),
            Some(500),
            "a whole chapter plus half of this one is behind"
        );
    }

    #[test]
    fn a_finished_book_has_nothing_left() {
        let book = shaped("timed-2222", &[4, 4], &[400, 400]);
        let finished = Progress {
            chapter_index: 1,
            sentence_index: 4,
            updated_at: 0,
        };
        assert_eq!(book.chars_left(finished), Some(0));
    }

    #[test]
    fn a_position_past_the_end_leaves_nothing_rather_than_a_debt() {
        let book = shaped("timed-3333", &[4], &[400]);
        let past = Progress {
            chapter_index: 9,
            sentence_index: 99,
            updated_at: 0,
        };
        assert_eq!(book.chars_left(past), Some(0), "saturating, never negative");
    }

    #[test]
    fn a_record_without_characters_has_no_estimate() {
        let book = sized("counts-only-4444", &[10, 10]);
        assert_eq!(
            book.chars_left(Progress::default()),
            None,
            "1.3.0's index knows sentences, not length"
        );
    }

    #[test]
    fn a_count_list_that_disagrees_with_the_book_has_no_estimate() {
        let book = shaped("odd-5555", &[10, 10], &[1000]);
        assert_eq!(book.chars_left(Progress::default()), None);
    }

    #[test]
    fn a_book_with_no_text_has_no_estimate() {
        let book = shaped("empty-6666", &[0, 0], &[0, 0]);
        assert_eq!(book.chars_left(Progress::default()), None);
    }
}
