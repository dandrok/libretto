//! AC-10: `ls` stays fast as the library grows.
//!
//! Two measurements, because they fail for different reasons:
//! - the in-process one (load + summarise) is the budget the spec names and has
//!   no startup noise in it;
//! - the subprocess one is what a user actually runs, and includes process start,
//!   which is why it gets a debug-build allowance.

mod common;

use std::time::{Duration, Instant};

use common::Sandbox;
use libretto::store::Store;

const BOOKS: usize = 50;
/// The spec's number, applied where it is meaningful: our code, not rustc's.
const PROCESS_BUDGET: Duration = Duration::from_millis(30);
/// Measured on a Ryzen 7 8845HS: debug 4.9-5.6 ms, release 1.2-2.2 ms for 50 books.
/// A debug binary pays startup costs rustc owns; the allowance is ~13x the
/// measured worst case so a loaded runner does not fail on noise, while release
/// keeps the spec's 30 ms.
fn subprocess_budget() -> Duration {
    if cfg!(debug_assertions) {
        Duration::from_millis(75)
    } else {
        PROCESS_BUDGET
    }
}

#[test]
fn loading_and_summarising_fifty_books_is_under_the_budget() {
    let sandbox = Sandbox::new();
    sandbox.seed_library(BOOKS);
    let store = Store::at(sandbox.data_dir());

    let started = Instant::now();
    let library = store.load().expect("library loads");
    let rows: Vec<String> = library
        .books
        .iter()
        .map(|book| {
            let progress = library.progress_of(&book.id);
            format!(
                "{:<40} {:>3}%  ch {}/{}  {}",
                book.id,
                library.percent_of(book),
                progress.chapter_index + 1,
                book.chapter_count,
                book.title
            )
        })
        .collect();
    let elapsed = started.elapsed();

    assert_eq!(rows.len(), BOOKS);
    assert!(
        elapsed < PROCESS_BUDGET,
        "summarising {BOOKS} books took {elapsed:?}, budget {PROCESS_BUDGET:?}"
    );
}

#[test]
fn ls_prints_fifty_books_and_stays_within_its_budget() {
    let sandbox = Sandbox::new();
    sandbox.seed_library(BOOKS);

    let started = Instant::now();
    let stdout = sandbox.stdout_of(&["ls"]);
    let elapsed = started.elapsed();

    assert_eq!(stdout.lines().count(), BOOKS, "one line per book");
    assert!(stdout.contains("book-00-aaaa"));
    assert!(stdout.contains("Book number 49"));
    let budget = subprocess_budget();
    assert!(
        elapsed < budget,
        "`libretto ls` with {BOOKS} books took {elapsed:?}, budget {budget:?} \
         (debug build: {}, release target: {PROCESS_BUDGET:?})",
        cfg!(debug_assertions)
    );
}

#[test]
fn ls_does_not_open_the_books_it_lists() {
    // The index carries chapter_count precisely so `ls` never touches an EPUB.
    // Every source path here is fictitious, so a regression that opens them fails
    // loudly instead of being slow.
    let sandbox = Sandbox::new();
    sandbox.seed_library(5);

    let stdout = sandbox.stdout_of(&["ls"]);
    assert_eq!(stdout.lines().count(), 5);
}
