//! Running heads: retail EPUBs repeat the book title at the top of every
//! chapter file, and spoken aloud that is the same sentence dozens of times.

use libretto::text::drop_running_heads;

#[test]
fn drops_whole_lines_that_repeat_title_or_author() {
    let chapter = "Software Architecture: The Hard Parts\n\
                   Chapter 6. Pulling apart operational data\n\
                   Prose that matters.\n\
                   Neal Ford";
    let cleaned = drop_running_heads(
        chapter,
        "Software Architecture: The Hard Parts",
        Some("Neal Ford"),
    );
    assert_eq!(
        cleaned,
        "Chapter 6. Pulling apart operational data\nProse that matters."
    );
}

#[test]
fn ignores_case_and_surrounding_punctuation() {
    let cleaned = drop_running_heads(
        "  grokking algorithms  \nBody text.",
        "Grokking Algorithms",
        None,
    );
    assert_eq!(cleaned, "Body text.");
}

#[test]
fn drops_a_running_head_that_adds_an_edition_suffix() {
    let chapter = "Designing Data-Intensive Applications, 2nd Edition\nProse that matters.";
    let cleaned = drop_running_heads(chapter, "Designing Data-Intensive Applications", None);
    assert_eq!(cleaned, "Prose that matters.");
}

#[test]
fn keeps_prose_that_merely_mentions_the_title() {
    let line = "Grokking Algorithms is a book about algorithms.";
    let cleaned = drop_running_heads(line, "Grokking Algorithms", None);
    assert_eq!(cleaned, line);
}

#[test]
fn leaves_text_alone_when_there_is_nothing_to_drop() {
    let text = "First paragraph.\n\nSecond paragraph.";
    assert_eq!(
        drop_running_heads(text, "Some Title", Some("Some Author")),
        text
    );
}
