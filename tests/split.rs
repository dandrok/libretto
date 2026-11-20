//! AC-9: the splitter never splits after an abbreviation and never emits a
//! segment longer than the limit. Text-level tests; no EPUB, no network.

use libretto::text::{sentences, strip_html};

const MAX: usize = 400;

#[test]
fn strip_html_drops_a_footnote_and_its_marker() {
    // The shape EPUB 3 uses: a noteref anchor holding the raised marker, and the
    // note itself in an aside at the end of the chapter.
    let html = "<html><body>\
        <p>Binary search halves the range \
        <a epub:type=\"noteref\" href=\"#fn1\"><sup>1</sup></a> each step.</p>\
        <aside epub:type=\"footnote\" id=\"fn1\">\
        <p>1. Assuming the array is sorted.</p></aside>\
        </body></html>";
    let text = strip_html(html);

    assert_eq!(
        text, "Binary search halves the range each step.",
        "the sentence is whole and unmarked"
    );
    assert!(
        !text.contains("Assuming the array"),
        "the note is not spoken: {text}"
    );
    assert!(
        !text.contains('1'),
        "and neither is the marker that pointed at it: {text}"
    );
}

#[test]
fn strip_html_keeps_a_superscript_that_is_not_a_note() {
    // A `sup` outside a noteref is real text — an exponent, not apparatus.
    let text = strip_html("<p>The area grows as r<sup>2</sup>.</p>");
    assert_eq!(text, "The area grows as r2.");
}

#[test]
fn strip_html_announces_a_table_instead_of_reading_its_cells() {
    let html = "<p>Here are the times.</p>\
        <table><tr><th>Runner</th><th>Time</th></tr>\
        <tr><td>Alice</td><td>3:41</td></tr>\
        <tr><td>Bob</td><td>3:58</td></tr></table>\
        <p>Both finished.</p>";
    let text = strip_html(html);

    assert_eq!(
        text, "Here are the times.\nTable, 3 rows, 2 columns.\nBoth finished.",
        "the shape replaces the cells"
    );
    for cell in ["Alice", "3:41", "Bob", "Runner"] {
        assert!(!text.contains(cell), "no bare cell is spoken: {cell}");
    }
}

#[test]
fn strip_html_names_a_table_by_its_caption() {
    let html = "<table><caption>Table 7-2. Running times</caption>\
        <tr><td>Alice</td><td>3:41</td></tr>\
        <tr><td>Bob</td><td>3:58</td></tr></table>";
    let text = strip_html(html);

    assert_eq!(text, "Table 7-2. Running times, 2 rows, 2 columns.");
    assert!(
        !text.contains("3:41"),
        "the cells are still dropped: {text}"
    );
}

#[test]
fn strip_html_says_nothing_for_an_empty_table() {
    // A shape it does not have is worse than silence.
    assert_eq!(
        strip_html("<p>Before.</p><table></table><p>After.</p>"),
        "Before.\nAfter."
    );
}

#[test]
fn strip_html_announces_a_code_sample_instead_of_reading_it() {
    let html = "<p>The loop below sums the range.</p>\
        <pre><code>total = 0\nfor i in range(n):\n    total += i\nprint(total)</code></pre>\
        <p>It runs in linear time.</p>";
    let text = strip_html(html);

    assert_eq!(
        text, "The loop below sums the range.\ncode sample, 4 lines.\nIt runs in linear time.",
        "the listing is replaced by its size"
    );
    for fragment in ["total", "range(n)", "print", "+="] {
        assert!(!text.contains(fragment), "no code is spoken: {fragment}");
    }
}

#[test]
fn strip_html_reads_inline_code_in_its_sentence() {
    // Inline code is part of the sentence; dropping it would lose the verb.
    let text = strip_html("<p>Run <code>kubectl get pods</code> to list them.</p>");
    assert_eq!(text, "Run kubectl get pods to list them.");
}

#[test]
fn strip_html_says_nothing_for_an_empty_code_sample() {
    assert_eq!(
        strip_html("<p>Before.</p><pre></pre><p>After.</p>"),
        "Before.\nAfter."
    );
    assert_eq!(
        strip_html("<p>Before.</p><pre>   </pre><p>After.</p>"),
        "Before.\nAfter.",
        "whitespace is not a listing"
    );
}

#[test]
fn splitter_respects_abbreviations() {
    let prose = "Dr. Smith met Mr. Jones at 5 p.m. They discussed the e.g. case. \
                 It was fine. Prof. Adams disagreed, i.e. she did. That is all.";
    let got = sentences(prose, MAX);

    assert!(
        got.iter().any(|s| s.starts_with("Dr. Smith")),
        "the first sentence must keep its honorific: {got:?}"
    );
    assert!(
        got.iter().all(|s| !s.ends_with("Dr.")),
        "no segment may end on an abbreviation: {got:?}"
    );
    assert!(
        got.iter()
            .all(|s| !s.ends_with("e.g.") && !s.ends_with("i.e.")),
        "no segment may end on a Latin abbreviation: {got:?}"
    );
    assert_eq!(got.iter().filter(|s| s.contains("case")).count(), 1);
}

#[test]
fn splitter_keeps_trailing_quote_with_its_sentence() {
    let got = sentences("\"Stop.\" He did not. \"Fine!\" She left.", MAX);
    assert_eq!(got[0], "\"Stop.\"");
    assert_eq!(got[1], "He did not.");
    assert_eq!(got[2], "\"Fine!\"");
}

#[test]
fn splitter_never_exceeds_the_limit() {
    let text = "word ".repeat(400);
    for chunk in sentences(&text, 120) {
        assert!(
            chunk.chars().count() <= 120,
            "segment of {} chars: {}",
            chunk.chars().count(),
            &chunk[..40.min(chunk.len())]
        );
    }
}

#[test]
fn splitter_splits_a_giant_sentence_at_word_boundaries() {
    let mut one_sentence = String::from("It began");
    for _ in 0..300 {
        one_sentence.push_str(" and then");
    }
    one_sentence.push('.');

    let got = sentences(&one_sentence, MAX);
    assert!(got.len() > 1, "a 3000-char sentence must be split");
    for chunk in &got {
        assert!(chunk.chars().count() <= MAX);
        assert!(!chunk.starts_with(' ') && !chunk.ends_with(' '));
    }
    // nothing is lost or duplicated at the seams
    let rejoined = got.join(" ");
    assert_eq!(rejoined.matches("and then").count(), 300);
}

#[test]
fn strip_html_drops_scripts_styles_and_markup() {
    let html = "<html><head><style>p{color:red}</style></head><body>\
                <p>First&nbsp;line.</p><script>var x = 1 < 2;</script>\
                <p>Second &amp; last.</p></body></html>";
    let text = strip_html(html);

    assert_eq!(text, "First line.\nSecond & last.");
    assert!(!text.contains("color:red"), "CSS must not be spoken");
    assert!(!text.contains("var x"), "JavaScript must not be spoken");
}

#[test]
fn strip_html_decodes_numeric_entities() {
    let text = strip_html("<p>caf&#233; &#x2014; open</p>");
    assert_eq!(text, "café — open");
}

#[test]
fn strip_html_leaves_a_bare_ampersand_alone() {
    let text = strip_html("<p>Q&amp;A & R&D</p>");
    assert_eq!(text, "Q&A & R&D");
}

#[test]
fn sentences_of_empty_text_are_empty() {
    assert!(sentences("", MAX).is_empty());
    assert!(sentences("   \n\n  ", MAX).is_empty());
    assert_eq!(strip_html(""), "");
}

#[test]
fn fragments_with_nothing_to_pronounce_are_not_sentences() {
    // A bullet, an em dash, a row of dots: the engine answers these with an empty
    // body, which used to stop a book mid-chapter. They should never get that far.
    let text = "\u{2022}\n\u{2014}\u{2014}\n\u{2026}\n...\n\nReal prose here.\n1.2\n";
    assert_eq!(sentences(text, MAX), vec!["Real prose here.", "1.2"]);
}

#[test]
fn a_control_character_never_reaches_the_terminal() {
    // A book body is untrusted input: an ESC in it could move the cursor, rewrite the
    // prompt or set the clipboard (OSC 52) the moment the read-along prints the sentence.
    let hostile = "clear the screen \u{1b}[2J and set the clipboard \u{1b}]52;c;zz\u{7}";
    let shown = libretto::text::visible(hostile);
    assert!(
        !shown.chars().any(|character| character.is_control()),
        "{shown:?}"
    );
    assert!(
        shown.contains("clear the screen"),
        "the words survive: {shown:?}"
    );
}
