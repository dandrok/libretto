//! `libretto` — the binary. Argument parsing, dispatch, and the exit codes the
//! spec promises. Everything of substance lives in the library crate so it can
//! be tested without a terminal.

#![forbid(unsafe_code)]

mod cli;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use libretto::cache::{Cache, CacheError};
use libretto::config::{self, Overrides, Settings};
use libretto::doctor;
use libretto::epub::{self, EpubError};
use libretto::pdf::{self, PdfError};
use libretto::play::{self, Mpv, PlayError};
use libretto::store::{self, BookRecord, Library, Progress, Store, StoreError};
use libretto::text;
use libretto::tts::{Kokoro, SpeechRequest, TtsError};

use crate::cli::{Cli, Command};

/// Exit codes are part of the interface; scripts branch on them.
const EXIT_OK: u8 = 0;
const EXIT_UNKNOWN_BOOK: u8 = 1;
const EXIT_AMBIGUOUS: u8 = 2;
const EXIT_TTS_DOWN: u8 = 3;
const EXIT_BAD_BOOK: u8 = 4;
const EXIT_ALREADY_READING: u8 = 5;
const EXIT_STATE: u8 = 6;

/// One sentence, one request: the same ceiling the splitter uses.
const SENTENCE_LIMIT: usize = 400;

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
        Some(Command::Read {
            id,
            chapter,
            skip,
            rewind,
            sentences,
            voice,
            speed,
            endpoint,
            model,
            player,
            prefetch,
        }) => read(
            &store,
            ReadRequest {
                id,
                chapter,
                skip,
                rewind,
                sentences,
            },
            Overrides {
                voice,
                speed,
                endpoint,
                model,
                player,
                prefetch,
            },
        ),
        Some(Command::Doctor { endpoint, player }) => check(Overrides {
            endpoint,
            player,
            ..Overrides::default()
        }),
        // Bare `libretto` resumes the last book: the common case, one word.
        None => resume(&store),
    }
}

/// `libretto doctor`: every silent-reader cause in one report.
fn check(overrides: Overrides) -> Result<(), AppError> {
    let settings = Settings::resolve(&config::load()?, overrides);
    let report = doctor::run(&settings);
    for line in report.lines() {
        println!("{line}");
    }
    if report.all_ok() {
        Ok(())
    } else {
        Err(AppError::Unhealthy)
    }
}

/// `libretto` with no arguments. Resumes the last book; on a fresh library it
/// explains itself instead of failing, because that is not an error.
fn resume(store: &Store) -> Result<(), AppError> {
    let library = store.load()?;
    if library.books.is_empty() {
        println!("no books\nadd one: libretto add <book.epub>");
        return Ok(());
    }
    let id = match library.last_read.clone() {
        Some(id) => id,
        None if library.books.len() == 1 => library.books[0].id.clone(),
        None => {
            println!("pick a book: libretto read <id>  (libretto ls to see them)");
            return Ok(());
        }
    };
    read(
        store,
        ReadRequest {
            id: Some(id),
            chapter: None,
            skip: 0,
            rewind: 0,
            sentences: None,
        },
        Overrides::default(),
    )
}

/// Both formats end up as the same `Book`; only the reader of the container differs.
fn load_book(path: &Path) -> Result<epub::Book, AppError> {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Ok(match extension.as_str() {
        "pdf" => pdf::load(path).map_err(AppError::Pdf)?,
        _ => epub::load(path)?,
    })
}

fn add(store: &Store, path: &Path) -> Result<(), AppError> {
    let hash = store::hash_file(path)?;
    let mut library = store.load()?;

    if let Some(existing) = library.find_by_hash(&hash) {
        println!(
            "already in library: {} — {}",
            existing.id,
            printable(&existing.title)
        );
        return Ok(());
    }

    let book = load_book(path)?;
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
        printable(&book.title),
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
            printable(&book.title)
        );
    }
    Ok(())
}

fn remove(store: &Store, id: &str) -> Result<(), AppError> {
    let mut library = store.load()?;
    let removed = library.remove(id)?;
    store.save(&library)?;
    println!("removed {} — {}", removed.id, printable(&removed.title));
    Ok(())
}

struct ReadRequest {
    id: Option<String>,
    chapter: Option<usize>,
    skip: usize,
    rewind: usize,
    sentences: Option<usize>,
}

fn read(store: &Store, request: ReadRequest, overrides: Overrides) -> Result<(), AppError> {
    let mut library = store.load()?;
    // Cloned: the progress closure below mutates the library while this is read.
    let record = pick_book(&library, request.id.as_deref())?.clone();

    let settings = Settings::resolve(&config::load()?, overrides);
    let engine = Kokoro::new(settings.endpoint.clone())?;
    let cache = Cache::open()?;
    let player = Mpv::new(settings.player.clone());

    let book_path = PathBuf::from(&record.source_path);
    // The path came out of the library file, so check it still is the book that
    // was indexed: a tampered library must not turn into "read me that file".
    verify_unchanged(&book_path, &record)?;
    let book = load_book(&book_path)?;
    let progress = library.progress_of(&record.id);

    let chapter_index = request
        .chapter
        .map(|chapter| chapter.saturating_sub(1))
        .unwrap_or(progress.chapter_index)
        .min(book.chapters.len().saturating_sub(1));
    let chapter = &book.chapters[chapter_index];
    let sentences = text::sentences(&chapter.text, SENTENCE_LIMIT);

    let start = if request.chapter.is_some() {
        0
    } else {
        progress
            .sentence_index
            .saturating_sub(request.rewind)
            .saturating_add(request.skip)
    }
    .min(sentences.len());

    // One reader at a time: two processes on one position would fight over it.
    let _lock = store.lock()?;

    let template = SpeechRequest {
        text: String::new(),
        voice: settings.voice.clone(),
        speed: settings.speed,
        model: settings.model.clone(),
    };

    println!(
        "{} — chapter {}/{} · starting at sentence {}/{}",
        printable(&record.title),
        chapter_index + 1,
        book.chapters.len(),
        start + 1,
        sentences.len()
    );

    let mut position = Progress {
        chapter_index,
        sentence_index: start,
        updated_at: store::now_seconds(),
    };
    let mut save_failure: Option<StoreError> = None;

    let played = play::play_sentences(
        &play::Session {
            engine: &engine,
            player: &player,
            cache: &cache,
            template: &template,
        },
        &sentences,
        start,
        request.sentences,
        |next| {
            position.sentence_index = next;
            position.updated_at = store::now_seconds();
            print_progress(
                &record,
                chapter_index,
                book.chapters.len(),
                next,
                sentences.len(),
                &settings,
            );
            library.progress.insert(record.id.clone(), position);
            library.last_read = Some(record.id.clone());
            if let Err(error) = store.save(&library) {
                save_failure = Some(error);
            }
        },
    );
    eprintln!();
    let played = played?;
    if let Some(error) = save_failure {
        return Err(AppError::Store(error));
    }

    library.last_read = Some(record.id.clone());
    store.save(&library)?;

    println!(
        "stopped at chapter {}, sentence {}/{} ({} played, {} from cache)",
        chapter_index + 1,
        played.next,
        sentences.len(),
        played.count,
        played.cache_hits
    );
    if played.next >= sentences.len() && chapter_index + 1 < book.chapters.len() {
        println!(
            "next: libretto read {} --chapter {}",
            record.id,
            chapter_index + 2
        );
    }
    Ok(())
}

/// The library stores a path and a content hash. Trust the hash, not the path:
/// if the file behind it changed, the index is stale and the position meaningless.
fn verify_unchanged(path: &Path, record: &BookRecord) -> Result<(), AppError> {
    let current = match store::hash_file(path) {
        Ok(hash) => hash,
        Err(_) => {
            return Err(AppError::BookChanged {
                path: path.display().to_string(),
                reason: "it is no longer readable".to_string(),
            });
        }
    };
    if current == record.content_hash {
        return Ok(());
    }
    Err(AppError::BookChanged {
        path: path.display().to_string(),
        reason: "its contents changed since it was added".to_string(),
    })
}

fn pick_book<'a>(library: &'a Library, id: Option<&str>) -> Result<&'a BookRecord, AppError> {
    match id {
        Some(prefix) => Ok(library.resolve(prefix)?),
        None => {
            let last = library
                .last_read
                .clone()
                .ok_or(AppError::Store(StoreError::UnknownBook))?;
            Ok(library.resolve(&last)?)
        }
    }
}

fn print_progress(
    record: &BookRecord,
    chapter_index: usize,
    chapters: usize,
    sentence: usize,
    total: usize,
    settings: &Settings,
) {
    eprint!(
        "\r  {} · ch {}/{} · {}/{} · {} ×{:.2}   ",
        short(&record.title, 28),
        chapter_index + 1,
        chapters,
        sentence,
        total,
        settings.voice,
        settings.speed
    );
}

/// Book titles come from a file, and a terminal interprets control characters:
/// an ESC sequence can move the cursor, rewrite the prompt or set the clipboard
/// (OSC 52). Nothing book-derived reaches stdout or stderr unsanitised.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn short(text: &str, width: usize) -> String {
    let clean = printable(text);
    let mut out: String = clean.chars().take(width).collect();
    if clean.chars().count() > width {
        out.push('…');
    }
    out
}

#[derive(Debug)]
enum AppError {
    Store(StoreError),
    Epub(EpubError),
    Tts(TtsError),
    Play(PlayError),
    Cache(CacheError),
    Config(config::ConfigError),
    /// The file behind a library entry is gone or is a different book.
    BookChanged {
        path: String,
        reason: String,
    },
    Pdf(PdfError),
    /// `doctor` found a failing check; the report was already printed.
    Unhealthy,
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
            AppError::Tts(_) => EXIT_TTS_DOWN,
            AppError::Cache(error) => match error {
                CacheError::Tts(_) => EXIT_TTS_DOWN,
                _ => EXIT_STATE,
            },
            AppError::Play(error) => match error {
                // A dead endpoint reaches us through the cache inside playback.
                PlayError::Cache(CacheError::Tts(_)) => EXIT_TTS_DOWN,
                _ => EXIT_STATE,
            },
            AppError::Config(_) => EXIT_STATE,
            AppError::BookChanged { .. } => EXIT_BAD_BOOK,
            AppError::Pdf(PdfError::NoTool) => EXIT_STATE,
            AppError::Pdf(_) => EXIT_BAD_BOOK,
            AppError::Unhealthy => EXIT_STATE,
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Store(error) => write!(f, "{error}"),
            AppError::Epub(error) => write!(f, "{error}"),
            AppError::Tts(error) => write!(f, "{error}"),
            AppError::Play(error) => write!(f, "{error}"),
            AppError::Cache(error) => write!(f, "{error}"),
            AppError::Config(error) => write!(f, "{error}"),
            AppError::BookChanged { path, reason } => write!(
                f,
                "{path}: {reason}; re-index it with `libretto add <path>`"
            ),
            AppError::Pdf(error) => write!(f, "{error}"),
            AppError::Unhealthy => write!(f, "one or more checks failed"),
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

impl From<TtsError> for AppError {
    fn from(error: TtsError) -> Self {
        AppError::Tts(error)
    }
}

impl From<CacheError> for AppError {
    fn from(error: CacheError) -> Self {
        AppError::Cache(error)
    }
}

impl From<PlayError> for AppError {
    fn from(error: PlayError) -> Self {
        AppError::Play(error)
    }
}

impl From<config::ConfigError> for AppError {
    fn from(error: config::ConfigError) -> Self {
        AppError::Config(error)
    }
}
