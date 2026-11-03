//! AC-9: the splitter never splits after an abbreviation and never emits a
//! segment longer than the limit. Text-level tests; no EPUB, no network.

use libretto::text::{sentences, strip_html};

const MAX: usize = 400;

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
