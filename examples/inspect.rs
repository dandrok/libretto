//! Debugging aid: `cargo run --example inspect -- <book.epub> [more.epub]`.
//!
//! Prints what the parser saw, so a suspicious book can be checked without a
//! player, a network or a TTS engine.

use std::path::PathBuf;

use libretto::{epub, text};

const SENTENCE_LIMIT: usize = 400;

fn main() -> anyhow::Result<()> {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: inspect <book.epub> [more.epub ...]");
        std::process::exit(2);
    }

    for path in paths {
        match epub::load(&path) {
            Ok(book) => {
                let total_sentences: usize = book
                    .chapters
                    .iter()
                    .map(|chapter| text::sentences(&chapter.text, SENTENCE_LIMIT).len())
                    .sum();
                let chars: usize = book.chapters.iter().map(|c| c.text.chars().count()).sum();
                println!(
                    "{} — {}\n  by {}\n  {} chapters · {} sentences · {} chars (~{} min of speech)",
                    path.display(),
                    book.title,
                    book.author.as_deref().unwrap_or("unknown"),
                    book.chapters.len(),
                    total_sentences,
                    chars,
                    chars / 15 / 60,
                );
                for (index, chapter) in book.chapters.iter().enumerate() {
                    let first = text::sentences(&chapter.text, SENTENCE_LIMIT);
                    println!(
                        "  {index:>3}. {:<40} {:>5} sentences | {}",
                        chapter
                            .title
                            .as_deref()
                            .unwrap_or("(untitled)")
                            .chars()
                            .take(40)
                            .collect::<String>(),
                        first.len(),
                        first
                            .first()
                            .map(|s| s.chars().take(60).collect::<String>())
                            .unwrap_or_default()
                    );
                }
            }
            Err(error) => println!("{} — FAILED: {error}", path.display()),
        }
    }
    Ok(())
}
