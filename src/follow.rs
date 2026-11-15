//! The read-along view: three sentences on screen, one word lit.
//!
//! Layout is a pure function of (sentences, width, active word) so it can be tested
//! without a terminal, and drawing is one escape-styled string per row so the caller
//! only has to move the cursor. Words are never hyphenated or split: the highlight is
//! addressed by word index, so a row must contain whole words or the mapping breaks.
//!
//! Column arithmetic counts characters, not display width. A sentence of CJK text can
//! therefore paint a row wider than the terminal asks for; every Latin-script book, which
//! is what this reads today, is exact.

use crate::captions::Word;

const UNDERLINE: &str = "\x1b[4m";
const ACCENT: &str = "\x1b[38;5;179m";
const RESET: &str = "\x1b[0m";
const ELLIPSIS: char = '…';

/// The sentence being spoken, and its neighbours for context.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sentence {
    pub previous: Option<String>,
    pub current: String,
    pub next: Option<String>,
    /// Timings for `current`, in the same word order the text splits into. Empty when
    /// the engine gave none — the caller then has no word to light.
    pub words: Vec<Word>,
}

/// One drawn row. `active` is the character span to underline, if this row holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub text: String,
    pub active: Option<(usize, usize)>,
}

/// How many rows of the current sentence a view may show at once. A sentence that needs more
/// scrolls inside this window rather than pushing the terminal around.
pub const SENTENCE_ROWS: usize = 4;

/// Lay the block out. `active` is an index into the sentence's words; before speech starts
/// and after it ends the caller passes the index `index_at` returns, which never leaves the
/// sentence.
///
/// `rows` is the window for the *current* sentence: when the text needs more, the rows shown
/// are the ones ending with the word being spoken, so the highlight is never scrolled off.
pub fn layout(sentence: &Sentence, width: usize, active: Option<usize>, rows: usize) -> Vec<Row> {
    let width = width.max(MIN_WIDTH);
    let rows = rows.max(1);
    let mut block = Vec::new();
    // A blank neighbour is not a row: an empty one would push the current sentence down
    // a line for no reason.
    if let Some(previous) = sentence
        .previous
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        block.push(Row {
            text: shorten(previous, width),
            active: None,
        });
    }
    let (lines, spans) = wrap(&sentence.current, width);
    let lit = active.and_then(|index| spans.get(index).copied());
    // Where the window starts: the row holding the word being spoken has to be inside it, so a
    // sentence longer than the window scrolls down as the voice moves through it.
    let first = match lit {
        Some((row, _, _)) if lines.len() > rows => (row + 1).saturating_sub(rows),
        _ => 0,
    }
    .min(lines.len().saturating_sub(rows));
    for (row, text) in lines.iter().enumerate().skip(first).take(rows) {
        let active = lit
            .filter(|(at, _, _)| *at == row)
            .map(|(_, start, end)| (start, end));
        block.push(Row {
            text: text.clone(),
            active,
        });
    }
    if let Some(next) = sentence
        .next
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        block.push(Row {
            text: shorten(next, width),
            active: None,
        });
    }
    block
}

/// Rows with their styling applied. Without colour the block is plain text, so a log
/// or a `NO_COLOR` terminal still gets the sentence, just no light.
pub fn render(rows: &[Row], colour: bool) -> Vec<String> {
    rows.iter()
        .map(|row| match (colour, row.active) {
            (true, Some((start, end))) => underline(&row.text, start, end),
            _ => row.text.clone(),
        })
        .collect()
}

/// Wrap one sentence, and report where each word landed. Spans are `(row, start, end)`
/// character offsets, in the order the words appear in the text — the same order the
/// timings are in.
pub fn wrap(text: &str, width: usize) -> (Vec<String>, Vec<(usize, usize, usize)>) {
    let width = width.max(MIN_WIDTH);
    let mut rows: Vec<String> = Vec::new();
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    let mut current = String::new();
    let mut row = 0usize;
    for word in text.split_whitespace() {
        let length = word.chars().count();
        let filled = current.chars().count();
        // A word that does not fit starts a new row; a word longer than the row keeps
        // it whole rather than being cut, because a split word cannot be highlighted.
        if filled > 0 && filled + 1 + length > width {
            rows.push(std::mem::take(&mut current));
            row += 1;
        }
        let start = if current.is_empty() {
            0
        } else {
            current.chars().count() + 1
        };
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        spans.push((row, start, start + length));
    }
    if !current.is_empty() {
        rows.push(current);
    }
    (rows, spans)
}

const MIN_WIDTH: usize = 20;

fn shorten(text: &str, width: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= width {
        return flat;
    }
    let kept: String = flat.chars().take(width - 1).collect();
    format!("{}{}", kept.trim_end(), ELLIPSIS)
}

fn underline(text: &str, start: usize, end: usize) -> String {
    let length = text.chars().count();
    let mut out = String::with_capacity(text.len() + 16);
    for (index, character) in text.chars().enumerate() {
        if index == start {
            out.push_str(ACCENT);
            out.push_str(UNDERLINE);
        }
        if index == end {
            out.push_str(RESET);
        }
        out.push(character);
    }
    if end >= length {
        out.push_str(RESET);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::interpolate;

    fn sentence(previous: &str, current: &str, next: &str) -> Sentence {
        Sentence {
            previous: Some(previous.to_string()),
            current: current.to_string(),
            next: Some(next.to_string()),
            words: interpolate(current, 4.0),
        }
    }

    #[test]
    fn a_long_sentence_wraps_without_breaking_a_word() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let (rows, spans) = wrap(text, 20);
        assert!(rows.len() > 1, "twenty columns cannot hold that: {rows:?}");
        for row in &rows {
            assert!(row.chars().count() <= 20, "row too wide: {row:?}");
        }
        let rebuilt = rows.join(" ");
        assert_eq!(rebuilt, text, "wrapping must not lose or split a word");
        assert_eq!(spans.len(), 10, "one span per word");
        for (index, (row, start, end)) in spans.iter().enumerate() {
            let word = rows[*row]
                .chars()
                .skip(*start)
                .take(end - start)
                .collect::<String>();
            assert_eq!(word, text.split_whitespace().nth(index).unwrap());
        }
    }

    #[test]
    fn the_active_word_is_lit_on_the_row_that_holds_it() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let (_, spans) = wrap(text, 20);
        let sentence = sentence("", text, "");
        let rows = layout(&sentence, 20, Some(7), SENTENCE_ROWS);
        let expected_row = spans[7].0;
        for (index, row) in rows.iter().enumerate() {
            if index == expected_row {
                assert_eq!(row.active, Some((spans[7].1, spans[7].2)));
            } else {
                assert_eq!(row.active, None, "only one row is lit");
            }
        }
    }

    #[test]
    fn the_neighbours_get_one_row_each_and_are_shortened_rather_than_wrapped() {
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let sentence = sentence(long, "small sentence", long);
        let rows = layout(&sentence, 24, None, SENTENCE_ROWS);
        assert_eq!(rows.len(), 1 + 1 + 1, "current fits one row here: {rows:?}");
        assert!(rows[0].text.ends_with(ELLIPSIS), "{:?}", rows[0].text);
        assert!(rows[2].text.ends_with(ELLIPSIS));
        for row in &rows {
            assert!(row.text.chars().count() <= 24, "{:?}", row.text);
        }
    }

    #[test]
    fn no_colour_means_no_escape_sequences() {
        let sentence = sentence("before", "the quick brown fox", "after");
        let rows = layout(&sentence, 40, Some(2), SENTENCE_ROWS);
        let plain = render(&rows, false);
        assert!(plain.iter().all(|row| !row.contains('\x1b')), "{plain:?}");
        assert!(plain.iter().any(|row| row.contains("brown")));
    }

    #[test]
    fn colour_underlines_exactly_the_active_word() {
        let sentence = sentence("before", "the quick brown fox", "after");
        let rows = layout(&sentence, 40, Some(2), SENTENCE_ROWS);
        let painted = render(&rows, true);
        let lit: Vec<&String> = painted
            .iter()
            .filter(|row| row.contains(UNDERLINE))
            .collect();
        assert_eq!(lit.len(), 1, "one word lit: {painted:?}");
        let stripped = lit[0]
            .replace(UNDERLINE, "")
            .replace(ACCENT, "")
            .replace(RESET, "");
        assert_eq!(
            stripped, "the quick brown fox",
            "styling does not change text"
        );
        assert!(
            lit[0].contains(&format!("{ACCENT}{UNDERLINE}brown{RESET}")),
            "{:?}",
            lit[0]
        );
    }

    #[test]
    fn a_blank_neighbour_does_not_take_a_row() {
        let sentence = sentence("   ", "only the current sentence", "");
        let rows = layout(&sentence, 40, None, SENTENCE_ROWS);
        assert_eq!(rows.len(), 1, "blank neighbours are not rows: {rows:?}");
    }

    #[test]
    fn a_sentence_longer_than_the_window_scrolls_with_the_voice() {
        // Twelve words at twenty columns: about four rows of three, so a window of two has to
        // move, and it has to move *with* the word being spoken rather than away from it.
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        let spoken = Sentence {
            previous: None,
            current: text.to_string(),
            next: None,
            words: interpolate(text, 12.0),
        };

        let first = layout(&spoken, 20, Some(0), 2);
        assert_eq!(first.len(), 2, "the window is the whole block: {first:?}");
        assert!(first[0].text.starts_with("alpha"), "{first:?}");
        assert_eq!(first[0].active, Some((0, 5)), "the first word is lit");

        let late = layout(&spoken, 20, Some(11), 2);
        assert_eq!(late.len(), 2, "still two rows: {late:?}");
        assert!(
            !late.iter().any(|row| row.text.starts_with("alpha")),
            "the beginning has scrolled away: {late:?}"
        );
        assert_eq!(
            late[1].active.map(|(start, end)| start < end),
            Some(true),
            "and the word being spoken is the last row, not off the screen: {late:?}"
        );
    }

    #[test]
    fn a_sentence_with_no_timings_still_draws() {
        let sentence = Sentence {
            previous: None,
            current: "no timings here".to_string(),
            next: None,
            words: Vec::new(),
        };
        let rows = layout(&sentence, 40, None, SENTENCE_ROWS);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].active, None);
        assert_eq!(render(&rows, true), vec!["no timings here".to_string()]);
    }
}
