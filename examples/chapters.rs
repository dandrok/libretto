//! Print the chapters a book yields, with word counts.
//!
//! `cargo run --example chapters -- book.epub` or `book.pdf`. This is the tool for
//! judging an import before spending eight hours listening to it: wrong chapters, prose
//! glued to a page number, or one 300-page chapter all show up here in seconds.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: chapters <book.epub|book.pdf>");
        return ExitCode::FAILURE;
    };
    let is_pdf = path
        .extension()
        .map(|extension| extension.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false);

    let book = if is_pdf {
        match libretto::pdf::load(&path) {
            Ok(book) => book,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match libretto::epub::load(&path) {
            Ok(book) => book,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    };

    println!("title:    {}", book.title);
    println!("author:   {}", book.author.as_deref().unwrap_or("-"));
    println!("chapters: {}", book.chapters.len());
    println!();
    for (index, chapter) in book.chapters.iter().enumerate() {
        let words = chapter.text.split_whitespace().count();
        let sentences = libretto::text::sentences(&chapter.text, 400).len();
        println!(
            "{:>3}. {:<52} {:>6} words {:>5} sentences",
            index + 1,
            chapter.title.as_deref().unwrap_or("(untitled)"),
            words,
            sentences
        );
    }
    ExitCode::SUCCESS
}
