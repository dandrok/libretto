//! `libretto` — the binary. Argument parsing, dispatch, and the exit codes the
//! spec promises. Everything of substance lives in the library crate so it can
//! be tested without a terminal.

#![forbid(unsafe_code)]

mod cli;

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use libretto::epub::{self, EpubError};
use libretto::store::{self, BookRecord, Library, Store, StoreError};

use crate::cli::{Cli, Command};

/// Exit codes are part of the interface; scripts branch on them.
const EXIT_OK: u8 = 0;
const EXIT_UNKNOWN_BOOK: u8 = 1;
const EXIT_AMBIGUOUS: u8 = 2;
#[allow(dead_code)]
const EXIT_TTS_DOWN: u8 = 3;
const EXIT_BAD_BOOK: u8 = 4;
#[allow(dead_code)]
const EXIT_ALREADY_READING: u8 = 5;
const EXIT_STATE: u8 = 6;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(error) => {
            eprintln!("libretto: {error}");
            ExitCode::from(error.code())
        }
    }
}

fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    let store = Store::open().map_err(AppError::Store)?;
    match cli.command {
        Some(Command::Add { path }) => add(&store, &path),
        Some(Command::Ls) => list(&store),
        Some(Command::Rm { id }) => remove(&store, &id),
        None => no_command(&store),
    }
}

fn add(store: &Store, path: &Path) -> Result<(), AppError> {
    let hash = store::hash_file(path)?;
    let mut library = store.load()?;

    if let Some(existing) = library.find_by_hash(&hash) {
        println!("already in library: {} — {}", existing.id, existing.title);
        return Ok(());
    }

    let book = epub::load(path)?;
    let id = store::make_id(&book.title, &hash);
    let source_path = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    let record = BookRecord {
        id: id.clone(),
        title: book.title.clone(),
        author: book.author.clone(),
        source_path,
        content_hash: hash,
        chapter_count: book.chapters.len(),
        added_at: store::now_seconds(),
    };
    library.insert(record);
    store.save(&library)?;
    println!(
        "added {id} — {} ({} chapters)",
        book.title,
        book.chapters.len()
    );
    Ok(())
}

fn list(store: &Store) -> Result<(), AppError> {
    let library = store.load()?;
    if library.books.is_empty() {
        println!("no books");
        return Ok(());
    }
    for book in &library.books {
        let progress = library.progress_of(&book.id);
        println!(
            "{:<40} {:>3}%  ch {}/{}  {}",
            book.id,
            library.percent_of(book),
            progress.chapter_index + 1,
            book.chapter_count,
            book.title
        );
    }
    Ok(())
}

fn remove(store: &Store, id: &str) -> Result<(), AppError> {
    let mut library = store.load()?;
    let removed = library.remove(id)?;
    store.save(&library)?;
    println!("removed {} — {}", removed.id, removed.title);
    Ok(())
}

/// Bare `libretto` will resume the last book once playback exists (T5); until
/// then it answers honestly instead of pretending.
fn no_command(store: &Store) -> Result<(), AppError> {
    let library: Library = store.load()?;
    match library.last_read.as_deref() {
        Some(id) => {
            let book = library.resolve(id)?;
            println!("last read: {} — {}", book.id, book.title);
            println!("playback lands in a later task; for now: libretto rm <id>");
        }
        None if library.books.is_empty() => println!("no books\nadd one: libretto add <book.epub>"),
        None => println!("pick a book: libretto ls"),
    }
    Ok(())
}

#[derive(Debug)]
enum AppError {
    Store(StoreError),
    Epub(EpubError),
}

impl AppError {
    fn code(&self) -> u8 {
        match self {
            AppError::Store(error) => match error {
                StoreError::UnknownBook => EXIT_UNKNOWN_BOOK,
                StoreError::Ambiguous { .. } => EXIT_AMBIGUOUS,
                StoreError::AlreadyLocked => EXIT_ALREADY_READING,
                StoreError::Corrupt { .. }
                | StoreError::Encode(_)
                | StoreError::Io { .. }
                | StoreError::Path(_) => EXIT_STATE,
            },
            AppError::Epub(_) => EXIT_BAD_BOOK,
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Store(error) => write!(f, "{error}"),
            AppError::Epub(error) => write!(f, "{error}"),
        }
    }
}

impl From<StoreError> for AppError {
    fn from(error: StoreError) -> Self {
        AppError::Store(error)
    }
}

impl From<EpubError> for AppError {
    fn from(error: EpubError) -> Self {
        AppError::Epub(error)
    }
}
