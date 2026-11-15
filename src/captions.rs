//! Word timings for one sentence: what the engine can tell us, and a fallback when it
//! cannot tell us anything.
//!
//! Kokoro-FastAPI answers `POST /dev/captioned_speech` with base64 audio plus a
//! `timestamps` array of `{word, start_time, end_time}` in seconds, generated during
//! synthesis rather than by a second pass over the audio. That is the only way to know
//! which word is being spoken, and it exists only on that endpoint and only from the
//! Kokoro backend — every other OpenAI-shaped server returns none.
//!
//! So nothing here is required: [`parse_reply`] turns a reply into words, [`interpolate`]
//! spreads a sentence's words evenly over a known duration, and a caller that gets the
//! first uses it and falls back to the second. The engine also normalises text before
//! speaking (`50,000` may come back as two words), which is why [`aligns`] exists: a
//! timing list that does not cover the sentence word for word would highlight the wrong
//! word from the first mismatch onwards, so it is discarded instead.

use serde::Deserialize;

/// One spoken word, timed against the start of its sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    /// The word as the engine rendered it, without surrounding whitespace.
    pub text: String,
    /// Seconds from the start of the sentence's audio.
    pub start: f32,
    /// Seconds from the start of the sentence's audio.
    pub end: f32,
}

#[derive(Deserialize)]
struct RawWord {
    word: String,
    start_time: f32,
    end_time: f32,
}

#[derive(Deserialize)]
struct RawReply {
    #[serde(default)]
    timestamps: Vec<RawWord>,
}

/// Read a captioned-speech reply. `None` when the reply carries no usable timings:
/// missing or empty `timestamps`, a time that is not finite, a word that ends before it
/// starts, or a blank word. One bad entry spoils the list on purpose — a list with a
/// hole in it would shift every following word by one.
pub fn parse_reply(body: &[u8]) -> Option<Vec<Word>> {
    let raw: RawReply = serde_json::from_slice(body).ok()?;
    if raw.timestamps.is_empty() {
        return None;
    }
    let mut words = Vec::with_capacity(raw.timestamps.len());
    for raw in raw.timestamps {
        let text = raw.word.trim();
        let usable = !text.is_empty()
            && raw.start_time.is_finite()
            && raw.end_time.is_finite()
            && raw.end_time >= raw.start_time;
        if !usable {
            return None;
        }
        words.push(Word {
            text: text.to_string(),
            start: raw.start_time,
            end: raw.end_time,
        });
    }
    Some(words)
}

/// Whether a timing list covers `text` word for word. Timings that describe a
/// normalised sentence ("fifty thousand" for `50,000`) cannot be laid against the
/// printed one, so the caller interpolates instead.
pub fn aligns(words: &[Word], text: &str) -> bool {
    words.len() == text.split_whitespace().count() && !words.is_empty()
}

/// Spread `text`'s words evenly over `duration` seconds, in proportion to their length.
/// This is the fallback that makes the feature work on any endpoint: a word that is a
/// little late is still the right word, which is all a highlight has to be.
pub fn interpolate(text: &str, duration: f32) -> Vec<Word> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() || !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    let total: usize = tokens.iter().map(|token| token.chars().count()).sum();
    if total == 0 {
        return Vec::new();
    }
    let mut words = Vec::with_capacity(tokens.len());
    let mut counted = 0usize;
    let mut cursor = 0.0f32;
    for (index, token) in tokens.iter().enumerate() {
        counted += token.chars().count();
        // The last word ends exactly at the duration; the others share the remainder
        // by their length, so rounding never leaves a gap at the end.
        let end = if index + 1 == tokens.len() {
            duration
        } else {
            duration * counted as f32 / total as f32
        };
        words.push(Word {
            text: (*token).to_string(),
            start: cursor,
            end,
        });
        cursor = end;
    }
    words
}

/// Which word is being spoken at `at` seconds. Before the first word the first word is
/// the answer; past the last, the last — a highlight that never goes blank mid-sentence.
pub fn index_at(words: &[Word], at: f32) -> Option<usize> {
    if words.is_empty() {
        return None;
    }
    Some(
        words
            .partition_point(|word| word.start <= at)
            .saturating_sub(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(body: &str) -> Option<Vec<Word>> {
        parse_reply(body.as_bytes())
    }

    #[test]
    fn a_captioned_reply_becomes_word_timings() {
        let words = reply(
            r#"{"audio":"AAAA","audio_format":"audio/mpeg","timestamps":[
                {"word":"Hello","start_time":0.0,"end_time":0.4},
                {"word":"world","start_time":0.4,"end_time":0.9}]}"#,
        )
        .expect("timings");
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello");
        assert!((words[1].end - 0.9).abs() < 1e-6);
    }

    #[test]
    fn a_reply_without_timings_is_not_a_failure_just_no_timings() {
        assert!(reply(r#"{"audio":"AAAA"}"#).is_none());
        assert!(reply("<html>404</html>").is_none());
        assert!(reply(r#"{"timestamps":[]}"#).is_none());
    }

    #[test]
    fn one_unusable_entry_discards_the_whole_list() {
        // A blank word, a time that is not a number, and a word that ends before it
        // starts: each would shift every following highlight by one.
        assert!(
            reply(
                r#"{"timestamps":[{"word":"a","start_time":0.0,"end_time":0.2},
                {"word":" ","start_time":0.2,"end_time":0.4}]}"#
            )
            .is_none()
        );
        assert!(
            reply(
                r#"{"timestamps":[{"word":"a","start_time":0.0,"end_time":0.2},
                {"word":"b","start_time":0.2,"end_time":0.1}]}"#
            )
            .is_none()
        );
    }

    #[test]
    fn long_words_take_longer_than_short_ones() {
        let words = interpolate("I extraordinary am", 3.0);
        assert_eq!(words.len(), 3);
        assert!(words[0].end > 0.0, "even a one-letter word takes time");
        let second = words[1].end - words[1].start;
        let third = words[2].end - words[2].start;
        assert!(second > third, "extraordinary outlasts am: {words:?}");
        assert!(
            (words[2].end - 3.0).abs() < 1e-6,
            "the sentence ends on time"
        );
        assert_eq!(words[0].start, 0.0);
    }

    #[test]
    fn interpolation_covers_the_duration_without_gaps() {
        let words = interpolate("one two three four", 10.0);
        for pair in words.windows(2) {
            assert!(
                (pair[0].end - pair[1].start).abs() < 1e-6,
                "no gap between words"
            );
        }
        assert!((words[3].end - 10.0).abs() < 1e-6);
    }

    #[test]
    fn a_sentence_with_nothing_speakable_has_no_words() {
        assert!(interpolate("   ", 4.0).is_empty());
        assert!(interpolate("word", 0.0).is_empty());
        assert!(interpolate("word", f32::NAN).is_empty());
    }

    #[test]
    fn the_highlight_never_goes_blank_inside_a_sentence() {
        let words = interpolate("alpha beta gamma", 3.0);
        assert_eq!(index_at(&words, -1.0), Some(0), "before the first word");
        assert_eq!(index_at(&words, 0.0), Some(0));
        assert_eq!(index_at(&words, 1.5), Some(1));
        assert_eq!(index_at(&words, 99.0), Some(2), "past the last word");
        assert_eq!(index_at(&[], 1.0), None);
    }

    #[test]
    fn timings_only_drive_the_highlight_when_they_cover_the_sentence() {
        let words = interpolate("fifty thousand dollars", 2.0);
        assert!(aligns(&words, "fifty thousand dollars"));
        assert!(
            !aligns(&words, "50,000 dollars"),
            "normalised text is a mismatch"
        );
        assert!(!aligns(&[], ""));
    }
}
