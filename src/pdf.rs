//! PDF → the same `Book` the EPUB path produces, best effort and honest about it.
//!
//! A PDF is a page description, not text: it has no paragraphs, no reading order and
//! no chapters. What a text layer gives you is lines positioned on a page, with the
//! book's furniture — the running head at the top, the page number, figure captions —
//! mixed into the prose. This module removes that furniture, rebuilds paragraphs, and
//! takes the chapters from the running head, which changes exactly at a chapter
//! boundary.
//!
//! Two shapes are refused rather than guessed at: a PDF with no text layer (a scan),
//! and a PDF whose layout the plain extraction scrambles (two columns). Both produce
//! word salad, and word salad read aloud for eight hours is worse than an error.

use std::path::Path;
use std::process::Command;

use crate::epub::{Book, Chapter};

/// A book's prose is bounded here for the same reason the EPUB path bounds it: a
/// 400 MB "book" is not a book, and we do not want to find that out by allocating.
pub const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
/// Below this many characters per page, on average, there is no text layer worth
/// reading: a scanned page yields a blank or a page number.
const MIN_CHARS_PER_PAGE: usize = 200;
/// Token-order agreement between the plain and the layout-preserving extraction.
/// Measured: single-column books score 0.89-0.99 (the low end is layout's own
/// hyphenation splitting words the plain extraction keeps whole), a two-column page
/// scores 0.29 because the plain extraction interleaves the columns. 0.70 sits in the
/// gap with room on both sides.
const MIN_LAYOUT_AGREEMENT: f32 = 0.70;
/// A line that appears at the top of this share of pages is furniture, not a sentence.
const FURNITURE_PAGE_RATIO: f32 = 0.30;
/// Groups below this many words are a chapter's opening page or a part divider, and
/// belong to the chapter that follows rather than to a chapter of their own.
const SHORT_GROUP_WORDS: usize = 250;

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("cannot read {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("PDF support needs `pdftotext` (poppler-utils), which is not installed")]
    NoTool,
    #[error("{path} has no text layer — it looks like a scan, so there is nothing to read")]
    NoText { path: String },
    #[error("{path} has a layout libretto cannot follow ({reason}); convert it to EPUB first")]
    Layout { path: String, reason: String },
    #[error("`{path}` produced no readable text")]
    Empty { path: String },
}

/// Read a PDF into the same shape the EPUB path produces.
pub fn load(path: &Path) -> Result<Book, PdfError> {
    let raw = extract(path, false)?;
    let laid_out = extract(path, true)?;

    let pages = split_pages(&raw);
    if pages.len() > 1 {
        let agreement = layout_agreement(&raw, &laid_out);
        if agreement < MIN_LAYOUT_AGREEMENT {
            return Err(PdfError::Layout {
                path: path.display().to_string(),
                reason: format!(
                    "columns are interleaved (extraction agreement {:.0}%); \
                     two-column or complex page layouts are not supported",
                    agreement * 100.0
                ),
            });
        }
    }

    let chapters = chapters_from_pages(&pages);
    if chapters.is_empty() {
        let chars: usize = raw.chars().filter(|c| !c.is_whitespace()).count();
        return Err(if chars < pages.len().max(1) * MIN_CHARS_PER_PAGE / 4 {
            PdfError::NoText {
                path: path.display().to_string(),
            }
        } else {
            PdfError::Empty {
                path: path.display().to_string(),
            }
        });
    }

    Ok(Book {
        title: title_from(path).unwrap_or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".to_string())
        }),
        author: None,
        chapters,
    })
}

fn extract(path: &Path, layout: bool) -> Result<String, PdfError> {
    let mut command = Command::new("pdftotext");
    command.arg("-q");
    if layout {
        command.arg("-layout");
    }
    command.arg(path).arg("-");

    let output = command.output().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => PdfError::NoTool,
        _ => PdfError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        },
    })?;
    if !output.status.success() {
        return Err(PdfError::Io {
            path: path.display().to_string(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    if output.stdout.len() > MAX_TEXT_BYTES {
        return Err(PdfError::Io {
            path: path.display().to_string(),
            reason: format!("text layer is larger than {MAX_TEXT_BYTES} bytes"),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// The PDF's own title metadata, when the producer wrote a usable one.
fn title_from(path: &Path) -> Option<String> {
    let output = Command::new("pdfinfo").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let title = text
        .lines()
        .find_map(|line| line.strip_prefix("Title:"))?
        .trim()
        .to_string();
    let usable = !title.is_empty()
        && !title.eq_ignore_ascii_case("untitled")
        && title.chars().any(|c| c.is_alphabetic());
    usable.then_some(title)
}

/// Form feed separates pages in pdftotext's output.
pub fn split_pages(text: &str) -> Vec<String> {
    text.split('\u{c}')
        .map(|page| page.to_string())
        .filter(|page| !page.trim().is_empty())
        .collect()
}

/// How much the two extractions agree on word order, as a 0..1 score.
///
/// Word *sets* are identical either way — interleaving only reorders them — so the
/// comparison has to be on sequences. Five-word shingles keep it cheap and make
/// reordering expensive, which is exactly the signal wanted here.
pub fn layout_agreement(a: &str, b: &str) -> f32 {
    let left = shingles(a);
    let right = shingles(b);
    if left.is_empty() || right.is_empty() {
        return 1.0;
    }
    let shared = left.intersection(&right).count() as f32;
    shared / (left.len().max(right.len()) as f32)
}

fn shingles(text: &str) -> std::collections::HashSet<[u8; 16]> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(normalize_word)
        .filter(|word| !word.is_empty())
        .collect();
    let mut set = std::collections::HashSet::new();
    if words.len() < 5 {
        for word in &words {
            let mut hash = [0u8; 16];
            let bytes = word.as_bytes();
            let take = bytes.len().min(16);
            hash[..take].copy_from_slice(&bytes[..take]);
            set.insert(hash);
        }
        return set;
    }
    for window in words.windows(5) {
        let joined = window.join(" ");
        let digest = fnv(&joined);
        let mut hash = [0u8; 16];
        hash[..8].copy_from_slice(&digest.to_le_bytes());
        set.insert(hash);
    }
    set
}

fn fnv(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn normalize_word(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Pages → chapters, with no I/O so it can be tested on strings.
pub fn chapters_from_pages(pages: &[String]) -> Vec<Chapter> {
    let heads = furniture_heads(pages);

    let mut groups: Vec<(Option<String>, Vec<String>)> = Vec::new();
    for (page, head) in pages.iter().zip(heads.iter()) {
        let body = page_body(page, head.as_deref());
        match groups.last_mut() {
            Some((current, lines)) if current == head => lines.extend(body),
            _ => groups.push((head.clone(), body)),
        }
    }

    let groups: Vec<(Option<String>, Vec<String>)> = groups
        .into_iter()
        .filter(|(_, lines)| !is_contents_page(lines))
        .collect();

    // A chapter's opening page carries its own head ("Chapter 5") before the running
    // head takes over ("CHAPTER 5. APIS"), and a part divider is a single line. Those
    // groups are the chapter's own front matter, not chapters — but a genuinely short
    // chapter must not be swallowed by the one after it, so a short group is only folded
    // into a neighbour whose head agrees with its own.
    let mut folded = groups;
    let mut index = 0;
    while index < folded.len() {
        let is_intro = {
            let (head, lines) = &folded[index];
            let next_agrees = folded
                .get(index + 1)
                .is_some_and(|(next, _)| match (head, next) {
                    (Some(intro), Some(next)) => heads_agree(intro, next),
                    _ => false,
                });
            next_agrees
                || (word_count(lines) < SHORT_GROUP_WORDS
                    && head.as_deref().is_none_or(is_part_divider))
        };
        if is_intro && index + 1 < folded.len() {
            let (head, lines) = folded.remove(index);
            let title = match (head, folded[index].0.clone()) {
                (Some(intro), Some(next)) if intro.len() >= next.len() => Some(intro),
                (_, Some(next)) => Some(next),
                (intro, None) => intro,
            };
            folded[index].0 = title;
            let mut merged = lines;
            merged.append(&mut folded[index].1);
            folded[index].1 = merged;
            continue; // the merged group may still be short
        }
        index += 1;
    }

    // Front matter: title page, copyright, dedication — untitled, before the first real
    // chapter. Dropped only when a titled chapter exists, so a headless PDF keeps its
    // text.
    if folded.iter().any(|(head, _)| head.is_some()) {
        while folded.first().is_some_and(|(head, _)| head.is_none()) {
            folded.remove(0);
        }
    }

    folded
        .into_iter()
        .map(|(head, lines)| Chapter {
            title: head,
            text: paragraphs(&lines).join("\n\n"),
        })
        .filter(|chapter| !is_word_pile(&chapter.text))
        .collect()
}

/// "Chapter 5" introduces "CHAPTER 5. APIS": same chapter, two spellings of its head.
fn heads_agree(intro: &str, next: &str) -> bool {
    let key = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let (intro, next) = (key(intro), key(next));
    !intro.is_empty() && (next.starts_with(&intro) || intro.starts_with(&next))
}

fn is_part_divider(head: &str) -> bool {
    let lowered = head.trim().to_lowercase();
    ["part", "section", "book"]
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
}

/// A page of short lines — an index, a table, a list of links — splits into sentences
/// that are one or two words long. Read aloud that is punctuation soup, so it goes.
fn is_word_pile(text: &str) -> bool {
    let words = text.split_whitespace().count();
    if words == 0 {
        return true;
    }
    let sentences = crate::text::sentences(text, 400).len().max(1);
    words < 1500 && words < sentences * 3
}

fn word_count(lines: &[String]) -> usize {
    lines
        .iter()
        .map(|line| line.split_whitespace().count())
        .sum()
}

/// Contents and index pages: lines that end in a page number. Read aloud they are
/// hours of numbers with no sentence around them.
fn is_contents_page(lines: &[String]) -> bool {
    let mut numbered = 0;
    let mut considered = 0;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        considered += 1;
        let ends_with_number = trimmed.split_whitespace().last().is_some_and(|last| {
            !last.is_empty() && last.len() <= 4 && last.chars().all(|c| c.is_ascii_digit())
        });
        if ends_with_number || trimmed.contains("....") {
            numbered += 1;
        }
    }
    considered >= 5 && numbered * 10 >= considered * 4
}

/// The line at the top of a page, when it is the page's running head.
///
/// Two different things look like a head and both are furniture:
/// - a book title, printed on every page (a large share of pages);
/// - a chapter title, printed only on the pages of that chapter. In a 25-chapter book
///   no chapter head reaches any global threshold, so the test is "does this line
///   repeat on consecutive pages", which is what sets it apart from a paragraph's
///   first line.
fn furniture_heads(pages: &[String]) -> Vec<Option<String>> {
    let candidates: Vec<Option<String>> = pages
        .iter()
        .map(|page| {
            page.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .filter(|line| line.chars().count() <= 80 && !is_page_number(line))
                .map(str::to_string)
        })
        .collect();

    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut runs: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut current: Option<(&str, usize)> = None;
    for candidate in candidates.iter().flatten() {
        *counts.entry(candidate.clone()).or_default() += 1;
        match current {
            Some((line, run)) if line == candidate.as_str() => {
                current = Some((line, run + 1));
            }
            _ => {
                if let Some((line, run)) = current {
                    let entry = runs.entry(line.to_string()).or_insert(0);
                    *entry = (*entry).max(run);
                }
                current = Some((candidate.as_str(), 1));
            }
        }
    }
    if let Some((line, run)) = current {
        let entry = runs.entry(line.to_string()).or_insert(0);
        *entry = (*entry).max(run);
    }

    let share = ((pages.len() as f32) * FURNITURE_PAGE_RATIO).ceil() as usize;
    candidates
        .into_iter()
        .map(|candidate| {
            candidate.filter(|line| {
                let repeated_across_pages = counts.get(line).copied().unwrap_or(0) >= share.max(2);
                let repeated_on_neighbours = runs.get(line).copied().unwrap_or(0) >= 2;
                looks_like_a_chapter(line)
                    || (pages.len() > 2 && (repeated_across_pages || repeated_on_neighbours))
            })
        })
        .collect()
}

/// `CHAPTER 7`, `Part II`, `Chapter Twelve`: a heading even on a page of its own.
fn looks_like_a_chapter(line: &str) -> bool {
    let lowered = line.trim().to_lowercase();
    ["chapter ", "part ", "section ", "appendix "]
        .iter()
        .any(|prefix| {
            lowered.strip_prefix(prefix).is_some_and(|rest| {
                rest.starts_with(|c: char| c.is_ascii_digit())
                    || rest.split_whitespace().next().is_some_and(|word| {
                        word.chars().all(|c| "ivxlcdm".contains(c)) && !word.is_empty()
                    })
            })
        })
}

/// The page without its running head and without single-line furniture.
fn page_body(page: &str, head: Option<&str>) -> Vec<String> {
    let mut dropped_head = false;
    page.lines()
        .map(str::trim_end)
        .filter(|line| {
            let trimmed = line.trim();
            if !dropped_head && Some(trimmed) == head {
                dropped_head = true;
                return false;
            }
            !is_page_number(trimmed) && !is_caption(trimmed)
        })
        .map(str::to_string)
        .collect::<Vec<String>>()
        .into_iter()
        .rev()
        .skip_while(|line| line.trim().is_empty())
        .collect::<Vec<String>>()
        .into_iter()
        .rev()
        .collect()
}

fn is_page_number(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 6
        && trimmed.chars().all(|c| c.is_ascii_digit() || c == ' ')
        && trimmed.chars().any(|c| c.is_ascii_digit())
}

/// `Figure 13.1: ...` describes an image that is not in the text; read aloud it is a
/// non sequitur, so it is dropped rather than spoken.
fn is_caption(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    let Some(rest) = lowered
        .strip_prefix("figure ")
        .or_else(|| lowered.strip_prefix("table "))
        .or_else(|| lowered.strip_prefix("listing "))
        .or_else(|| lowered.strip_prefix("algorithm "))
        .or_else(|| lowered.strip_prefix("example "))
    else {
        return false;
    };
    rest.starts_with(|c: char| c.is_ascii_digit())
}

/// Blank lines separate paragraphs; everything else is one wrapped paragraph.
pub fn paragraphs(lines: &[String]) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            flush(&mut current, &mut paragraphs);
            continue;
        }
        current.push(line.clone());
    }
    flush(&mut current, &mut paragraphs);
    paragraphs
}

fn flush(current: &mut Vec<String>, out: &mut Vec<String>) {
    if current.is_empty() {
        return;
    }
    let mut text = String::new();
    for line in current.drain(..) {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if text.is_empty() {
            text.push_str(&line);
        } else if text.ends_with('-') && starts_lowercase(&line) {
            // A word broken at the line end: "algo-" + "rithm" is one word.
            text.pop();
            text.push_str(&line);
        } else {
            text.push(' ');
            text.push_str(&line);
        }
    }
    let text = repair_ligatures(&text);
    if !text.is_empty() {
        out.push(text);
    }
}

fn starts_lowercase(line: &str) -> bool {
    line.chars().next().is_some_and(|c| c.is_lowercase())
}

/// Ligatures survive in many text layers as single glyphs, and a speech engine reads
/// "workﬂow" as "work" plus a replacement character.
pub fn repair_ligatures(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\u{fb00}' => out.push_str("ff"),
            '\u{fb01}' => out.push_str("fi"),
            '\u{fb02}' => out.push_str("fl"),
            '\u{fb03}' => out.push_str("ffi"),
            '\u{fb04}' => out.push_str("ffl"),
            '\u{fb05}' | '\u{fb06}' => out.push_str("st"),
            '\u{00ad}' => {}
            '\u{00a0}' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(chapters: Vec<Chapter>) -> Vec<String> {
        chapters.into_iter().map(|chapter| chapter.text).collect()
    }

    const HEAD_ONE: &str = "CHAPTER 1. THE FIRST THING";
    const HEAD_TWO: &str = "CHAPTER 2. THE SECOND THING";

    /// Long enough that a group is not mistaken for a chapter's opening page.
    fn padding(words: usize) -> String {
        "padding ".repeat(words).trim_end().to_string()
    }

    fn fixture_pages() -> Vec<String> {
        vec![
            format!(
                "{HEAD_ONE}\n1\nA sentence the layout wrapped across lines, so the text\narrives broken and has to be joined back.\n{}\n\nFigure 1.1: A workflow implementing a retry\n",
                padding(150)
            ),
            format!(
                "{HEAD_ONE}\n2\nA word split like algo-\nrithm is repaired.\n{}\n",
                padding(150)
            ),
            format!(
                "{HEAD_TWO}\n3\nParagraph one of chapter two.\n\nParagraph two follows a blank line.\n{}\n",
                padding(150)
            ),
            format!(
                "{HEAD_TWO}\n4\nThe last page ends the book.\n{}\n",
                padding(150)
            ),
        ]
    }

    #[test]
    fn the_running_head_becomes_the_chapter_title_and_never_the_prose() {
        let chapters = chapters_from_pages(&fixture_pages());
        assert_eq!(chapters.len(), 2, "two heads, two chapters");
        assert_eq!(chapters[0].title.as_deref(), Some(HEAD_ONE));
        assert_eq!(chapters[1].title.as_deref(), Some(HEAD_TWO));
        for chapter in &chapters {
            assert!(
                !chapter.text.contains("CHAPTER"),
                "the head must not be spoken: {}",
                chapter.text
            );
        }
    }

    #[test]
    fn wrapped_lines_are_joined_and_a_blank_line_stays_a_paragraph_break() {
        let chapters = chapters_from_pages(&fixture_pages());
        let first = &chapters[0].text;
        assert!(
            first.contains("text arrives broken and has to be joined back."),
            "{first}"
        );
        let second = &chapters[1].text;
        assert_eq!(
            second.split("\n\n").count(),
            2,
            "the blank line survives as a paragraph break: {second}"
        );
    }

    #[test]
    fn page_numbers_and_captions_are_not_read_aloud() {
        let text = chapters_from_pages(&fixture_pages()).remove(0).text;
        assert!(!text.contains("Figure 1.1"), "{text}");
        assert!(
            !text.lines().any(|line| line.trim() == "1"),
            "the page number is gone: {text}"
        );
    }

    #[test]
    fn a_word_broken_at_the_line_end_is_repaired() {
        let text = chapters_from_pages(&fixture_pages()).remove(0).text;
        assert!(text.contains("algorithm"), "{text}");
        assert!(!text.contains("algo- rithm"), "{text}");
    }

    #[test]
    fn ligatures_are_repaired_before_anything_is_spoken() {
        assert_eq!(repair_ligatures("work\u{fb02}ow"), "workflow");
        assert_eq!(repair_ligatures("\u{fb01}nd con\u{fb01}g"), "find config");
        assert_eq!(repair_ligatures("o\u{fb03}ce \u{fb04}a"), "office ffla");
    }

    #[test]
    fn a_hyphen_that_ends_a_real_word_is_left_alone() {
        // Only a lowercase continuation means the word was split.
        let lines = vec!["a well-".to_string(), "Known term.".to_string()];
        assert_eq!(paragraphs(&lines), vec!["a well- Known term."]);
    }

    #[test]
    fn a_head_that_appears_on_from_pages_is_furniture() {
        let pages: Vec<String> = (1..=10)
            .map(|n| {
                format!(
                    "THE BOOK TITLE\n{n}\nProse for page {n}.\n{}\n",
                    padding(40)
                )
            })
            .collect();
        let chapters = chapters_from_pages(&pages);
        assert_eq!(chapters.len(), 1, "one head, one chapter");
        assert_eq!(chapters[0].title.as_deref(), Some("THE BOOK TITLE"));
        assert!(chapters[0].text.contains("Prose for page 10."));
    }

    #[test]
    fn a_single_page_pdf_keeps_its_only_line() {
        let pages = vec!["Just one line of prose, nothing else to strip.".to_string()];
        let chapters = chapters_from_pages(&pages);
        assert_eq!(
            book(chapters),
            vec!["Just one line of prose, nothing else to strip."]
        );
    }

    #[test]
    fn interleaved_columns_score_low_and_single_column_scores_high() {
        let prose = "The coordinator persists its state so that a crash cannot lose \
                     the transaction and a restart resumes from the last checkpoint";
        assert!(
            layout_agreement(prose, prose) > 0.99,
            "identical text agrees with itself"
        );
        let interleaved = "The persists transaction a resumes coordinator its state so \
                           that crash cannot lose the and restart from last checkpoint";
        assert!(
            layout_agreement(prose, interleaved) < MIN_LAYOUT_AGREEMENT,
            "a scrambled order must not pass the guard"
        );
    }

    #[test]
    fn pages_split_on_form_feed_and_empty_pages_are_dropped() {
        let pages = split_pages("one\u{c}two\u{c}\u{c}three");
        assert_eq!(pages, vec!["one", "two", "three"]);
    }
}
