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
use libretto::captions;
use libretto::config::{self, Overrides, Settings};
use libretto::doctor;
use libretto::epub::{self, EpubError};
use libretto::estimate;
use libretto::follow;
use libretto::pdf::{self, PdfError};
use libretto::play::{self, Mpv, PlayError};
use libretto::store::{self, BookRecord, Library, Progress, Store, StoreError};
use libretto::text;
use libretto::tts::{Kokoro, SpeechRequest, TtsError};
use libretto::ui::{self, Ui};

use crate::cli::{BookArgs, Cli, Command};

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
        Some(Command::Add { path }) => add(&store, &path, cli.quiet),
        Some(Command::Ls) => {
            // The estimate follows the configured speed; a broken config must not turn
            // listing a library into a failure, so it falls back to the default.
            let speed = config::load()
                .map(|config| Settings::resolve(&config, Overrides::default()).speed)
                .unwrap_or(config::DEFAULT_SPEED);
            list(&store, speed)
        }
        Some(Command::Rm { id }) => remove(&store, &id),
        Some(Command::Read(args)) => {
            let (request, overrides) = split(args, cli.sync_offset);
            read(&store, request, overrides, cli.quiet, false, cli.width)
        }
        Some(Command::Follow(args)) => {
            let (request, overrides) = split(args, cli.sync_offset);
            read(&store, request, overrides, cli.quiet, true, cli.width)
        }
        Some(Command::Doctor { endpoint, player }) => check(Overrides {
            endpoint,
            player,
            ..Overrides::default()
        }),
        // Bare `libretto` resumes the last book: the common case, one word.
        None => resume(&store, cli.quiet),
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
fn resume(store: &Store, quiet: bool) -> Result<(), AppError> {
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
        quiet,
        false,
        None,
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

/// What the waiting line says while a book is being read: its kind, and its size.
///
/// The size is the honest predictor of the wait — an EPUB is unzipped in milliseconds, while
/// a PDF is converted whole by `pdftotext` on every run — so the number the reader waits on is
/// the number they are shown.
fn describe_book(path: &Path) -> String {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let kind = if extension == "pdf" { "PDF" } else { "EPUB" };
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() >= 1_000_000 => {
            format!("reading a {:.1} MB {kind}", meta.len() as f64 / 1_000_000.0)
        }
        Ok(meta) => format!("reading a {} KB {kind}", (meta.len() / 1000).max(1)),
        // The file may be gone: the check that says so comes next, and names the reason.
        Err(_) => format!("reading the {kind}"),
    }
}

/// Sentences per chapter, and the characters they hold, counted with the same
/// splitter `read` uses — so the percentage lands on the sentence that will be
/// spoken and the time estimate describes the text that will be read out.
fn chapter_shape(book: &epub::Book) -> (Vec<usize>, Vec<usize>) {
    let mut sentences = Vec::with_capacity(book.chapters.len());
    let mut chars = Vec::with_capacity(book.chapters.len());
    for chapter in &book.chapters {
        let split = text::sentences(&chapter.text, SENTENCE_LIMIT);
        sentences.push(split.len());
        chars.push(split.iter().map(|sentence| sentence.chars().count()).sum());
    }
    (sentences, chars)
}

/// Characters still ahead, counted against the chapter that is actually open rather
/// than assuming its sentences are all the same length like `chars_left` must.
/// `None` when the record has no per-chapter totals yet.
fn chars_left_in_book(
    record: &BookRecord,
    chapter_index: usize,
    sentence_index: usize,
    chapter_sentences: &[String],
) -> Option<usize> {
    if record.chapter_chars.len() != record.chapter_count || record.chapter_chars.is_empty() {
        return None;
    }
    let total: usize = record.chapter_chars.iter().sum();
    if total == 0 {
        return None;
    }
    let behind: usize = record.chapter_chars.iter().take(chapter_index).sum();
    let spoken: usize = chapter_sentences
        .iter()
        .take(sentence_index)
        .map(|sentence| sentence.chars().count())
        .sum();
    Some(total.saturating_sub(behind.saturating_add(spoken)))
}

fn add(store: &Store, path: &Path, quiet: bool) -> Result<(), AppError> {
    // Parsing a PDF is seconds of work, and it happens before anything about the book is
    // known: the line under the prompt is the only thing that can say so. No wordmark here —
    // an import does not need four rows of banner over it.
    let working = Ui::new(quiet).waiting(&describe_book(path));
    let hash = store::hash_file(path)?;
    let mut library = store.load()?;

    if let Some(existing) = library.find_by_hash(&hash) {
        drop(working);
        println!(
            "already in library: {} — {}",
            existing.id,
            printable(&existing.title)
        );
        return Ok(());
    }

    let book = load_book(path)?;
    drop(working);
    let id = store::make_id(&book.title, &hash);
    let source_path = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    let (sentence_counts, chapter_chars) = chapter_shape(&book);
    let record = BookRecord {
        id: id.clone(),
        title: book.title.clone(),
        author: book.author.clone(),
        source_path,
        content_hash: hash,
        chapter_count: book.chapters.len(),
        sentence_counts,
        chapter_chars,
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

fn list(store: &Store, speed: f32) -> Result<(), AppError> {
    let library = store.load()?;
    if library.books.is_empty() {
        println!("no books");
        return Ok(());
    }
    for book in &library.books {
        let progress = library.progress_of(&book.id);
        let mut row = vec![
            format!("{:<40}", book.id),
            format!("{:>3}%", library.percent_of(book)),
            format!("ch {}/{}", progress.chapter_index + 1, book.chapter_count),
        ];
        // No estimate for an index written before the counts existed: a blank beats a
        // number invented from a chapter count.
        match book.chars_left(progress) {
            Some(0) => row.push("done".to_string()),
            Some(chars) => row.push(format!(
                "~{} left",
                estimate::human(estimate::seconds(chars, speed))
            )),
            None => {}
        }
        row.push(printable(&book.title));
        println!("{}", row.join("  "));
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

/// One argument block, two verbs: `read` and `follow` share everything except how the
/// sentence reaches the screen.
fn split(args: BookArgs, sync_offset: Option<i64>) -> (ReadRequest, Overrides) {
    (
        ReadRequest {
            id: args.id,
            chapter: args.chapter,
            skip: args.skip,
            rewind: args.rewind,
            sentences: args.sentences,
        },
        Overrides {
            voice: args.voice,
            speed: args.speed,
            endpoint: args.endpoint,
            model: args.model,
            player: args.player,
            prefetch: args.prefetch,
            sync_offset,
        },
    )
}

struct ReadRequest {
    id: Option<String>,
    chapter: Option<usize>,
    skip: usize,
    rewind: usize,
    sentences: Option<usize>,
}

fn read(
    store: &Store,
    request: ReadRequest,
    overrides: Overrides,
    quiet: bool,
    following: bool,
    width: Option<usize>,
) -> Result<(), AppError> {
    let mut library = store.load()?;
    // Cloned: the progress closure below mutates the library while this is read.
    let mut record = pick_book(&library, request.id.as_deref())?.clone();

    let ui = Ui::with_width(quiet, if following { width } else { None });
    let settings = Settings::resolve(&config::load()?, overrides);
    ui.set_sync_offset(settings.sync_offset);
    let engine = Kokoro::new(settings.endpoint.clone())?;
    let cache = Cache::open()?;
    let player = Mpv::new(settings.player.clone());
    {
        // The view asks the player where it is instead of counting seconds: mpv answers
        // over IPC, a player without a socket answers nothing, and the wall clock stays
        // the fallback for both.
        let position = player.clock();
        ui.set_clock(std::sync::Arc::new(move || {
            position.lock().ok().and_then(|slot| *slot)
        }));
    }

    let book_path = PathBuf::from(&record.source_path);
    // The wordmark, then the wait by name: hashing and parsing this file happen before the
    // opening line can say which book it is, and on a PDF they are seconds.
    let working = ui.opening(&describe_book(&book_path));
    // The path came out of the library file, so check it still is the book that
    // was indexed: a tampered library must not turn into "read me that file".
    verify_unchanged(&book_path, &record)?;
    let book = load_book(&book_path)?;
    drop(working);
    let progress = library.progress_of(&record.id);

    // A library written before 1.3.0 carries no per-chapter sentence counts, so
    // `ls` could only report chapter boundaries. The book is open right now:
    // count it and put the result in the in-memory index, so the position saves
    // below carry it and the next `ls` is exact.
    let shape_is_stale = record.sentence_percent(progress).is_none()
        || record.chapter_chars.len() != record.chapter_count;
    if shape_is_stale && record.chapter_count == book.chapters.len() {
        let (sentence_counts, chapter_chars) = chapter_shape(&book);
        record.sentence_counts = sentence_counts;
        record.chapter_chars = chapter_chars;
        if let Some(slot) = library.books.iter_mut().find(|book| book.id == record.id) {
            *slot = record.clone();
        }
    }

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

    // On a terminal the live line carries this and keeps it current; repeating it in the
    // opening line is the same number twice within one screen. A pipe never sees the live
    // line, so there it stays.
    let left = chars_left_in_book(&record, chapter_index, start, &sentences)
        .filter(|_| !ui.interactive())
        .map(|chars| {
            format!(
                " · ~{} left",
                estimate::human(estimate::seconds(chars, settings.speed))
            )
        })
        .unwrap_or_default();
    // What the read-along draws for one sentence: its neighbours for context, and its
    // words spread over the sentence's estimated length. The engine's own word timings
    // replace the estimate when it has them.
    let follow_sentence = |index: usize, spoken: Option<&[captions::Word]>| -> follow::Sentence {
        // The view prints book text, which is not ours: control characters are flattened
        // before anything reaches the terminal, and the word list is built from the
        // flattened text so the highlight still addresses the words on screen.
        let current = text::visible(sentences.get(index).map(String::as_str).unwrap_or_default());
        let neighbour = |offset: isize| -> Option<String> {
            let at = index.checked_add_signed(offset)?;
            sentences.get(at).map(|sentence| text::visible(sentence))
        };
        let duration = estimate::duration(current.chars().count(), settings.speed);
        // The engine's own timings are used only when they cover the sentence word for
        // word: it normalises text before speaking, and a list that slipped by one word
        // would light the wrong word for the rest of the sentence.
        let words = spoken
            .filter(|words| captions::aligns(words, &current))
            .map(|words| words.to_vec())
            .unwrap_or_else(|| captions::interpolate(&current, duration));
        follow::Sentence {
            previous: neighbour(-1),
            words,
            current,
            next: neighbour(1),
        }
    };
    if following {
        // Painted before the first sentence is synthesized, so the text appears as soon as
        // the book opens; the engine's timings replace the estimate when it starts.
        ui.set_follow(Some(follow_sentence(start, None)));
    }

    println!(
        "{} — chapter {}/{} · starting at sentence {}/{}{}",
        printable(&record.title),
        chapter_index + 1,
        book.chapters.len(),
        start + 1,
        sentences.len(),
        left
    );

    let mut position = Progress {
        chapter_index,
        sentence_index: start,
        updated_at: store::now_seconds(),
    };
    let mut save_failure: Option<StoreError> = None;

    let speaking = ui::Speaking::new(&player, &ui);
    // The live line speaks the book's language: the same percentage `ls` prints, the
    // chapters as a shape, and how much listening is left.
    let book_percent = |chapter: usize, sentence: usize| -> u8 {
        let progress = Progress {
            chapter_index: chapter,
            sentence_index: sentence,
            updated_at: 0,
        };
        record.sentence_percent(progress).unwrap_or_else(|| {
            let chapters = record.chapter_count.max(1);
            ((chapter.saturating_mul(100) / chapters).min(100)) as u8
        })
    };
    let left = |chapter: usize, sentence: usize| -> Option<u64> {
        chars_left_in_book(&record, chapter, sentence, &sentences)
            .map(|chars| estimate::seconds(chars, settings.speed))
    };
    let base = ui::Status {
        title: short(&record.title, 28),
        chapter: chapter_index,
        chapters: book.chapters.len(),
        sentence: start,
        total: sentences.len(),
        percent: book_percent(chapter_index, start),
        left: left(chapter_index, start),
        voice: settings.voice.clone(),
        speed: settings.speed,
    };
    ui.open(base.clone());

    let played = play::play_sentences(
        &play::Session {
            engine: &engine,
            player: &speaking,
            cache: &cache,
            template: &template,
            captions: following,
        },
        &sentences,
        start,
        request.sentences,
        |index, words| {
            if following {
                ui.set_follow(Some(follow_sentence(index, words)));
            }
        },
        |next| {
            position.sentence_index = next;
            position.updated_at = store::now_seconds();
            let mut snapshot = base.clone();
            snapshot.sentence = next;
            snapshot.percent = book_percent(chapter_index, next);
            snapshot.left = left(chapter_index, next);
            ui.note(snapshot);
            library.progress.insert(record.id.clone(), position);
            library.last_read = Some(record.id.clone());
            if let Err(error) = store.save(&library) {
                save_failure = Some(error);
            }
        },
    );
    ui.finish();
    let played = played?;
    if let Some(error) = save_failure {
        return Err(AppError::Store(error));
    }

    library.last_read = Some(record.id.clone());
    store.save(&library)?;

    println!(
        "stopped at chapter {}, sentence {}/{} ({} played, {} from cache{})",
        chapter_index + 1,
        played.next,
        sentences.len(),
        played.count,
        played.cache_hits,
        if played.skipped > 0 {
            format!(", {} skipped", played.skipped)
        } else {
            String::new()
        }
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
                // A path that was never resolved is bad input, not a broken state.
                StoreError::Unreadable { .. } => EXIT_BAD_BOOK,
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
