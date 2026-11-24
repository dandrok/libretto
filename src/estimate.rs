//! How long the rest of a book will take, without asking the engine anything.
//!
//! The speaking rate is measured, not assumed. Thirty consecutive sentences of a real
//! technical book, spoken through Kokoro's `af_heart` at speed 1.0, came to 332.3 s of
//! audio for 5,357 characters — 16.1 characters per second, with a per-sentence spread
//! of 14.3 to 18.5 that averages out over a book. Exact durations would mean probing
//! ten thousand cache files or asking the engine for word timings; an estimate within a
//! couple of percent of a ten-hour book is the right trade for a line of text.
//!
//! Nothing here is a promise, so every caller prints it with a `~`.

/// Characters per second at speed 1.0, measured for `af_heart` on Kokoro.
pub const CHARS_PER_SECOND: f32 = 16.1;

/// Seconds of speech for `chars` at `speed`, unrounded — for the read-along, which
/// spreads a sentence's words across it. `speed` is clamped to the range the engine
/// accepts, so a stray zero cannot divide by zero.
pub fn duration(chars: usize, speed: f32) -> f32 {
    let rate = CHARS_PER_SECOND * speed.clamp(0.25, 4.0);
    chars as f32 / rate
}

/// The same, rounded to whole seconds for anything a person reads.
pub fn seconds(chars: usize, speed: f32) -> u64 {
    duration(chars, speed).round() as u64
}

/// `11h 06m`, `59m`, `<1m` — no seconds, because nobody counts them on a book.
pub fn human(seconds: u64) -> String {
    let minutes = seconds / 60;
    if minutes >= 60 {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    } else if minutes >= 1 {
        format!("{minutes}m")
    } else {
        "<1m".to_string()
    }
}

/// How much listening a `--for` window means: `90s`, `30m`, `1h30m`, or a bare count of
/// seconds.
///
/// Strict on purpose. A duration is a number a listener typed to make a machine stop, so a
/// typo is refused here and becomes bad input at the boundary, rather than a run that reads
/// for the wrong length of time or dies halfway through a chapter.
pub fn parse_duration(text: &str) -> Option<u64> {
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut units = 0usize;
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return None,
        };
        // A unit with no number in front of it, or an amount that cannot be counted.
        total = total.checked_add(digits.parse::<u64>().ok()?.checked_mul(unit)?)?;
        digits.clear();
        units += 1;
    }
    if digits.is_empty() {
        // `30m`, `1h30m`, `0s`: every part ended in a unit.
        return (units > 0).then_some(total);
    }
    if units == 0 {
        // Nothing but digits: seconds, the unit a listener leaves out.
        return digits.parse::<u64>().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sentence_takes_its_characters_at_the_measured_rate() {
        assert_eq!(seconds(1610, 1.0), 100, "1610 chars at 16.1/s");
        assert_eq!(seconds(1610, 2.0), 50, "double speed, half the time");
        assert_eq!(seconds(0, 1.0), 0);
    }

    #[test]
    fn an_impossible_speed_is_clamped_instead_of_dividing_by_zero() {
        assert_eq!(seconds(1610, 0.0), 400, "clamped to 0.25x");
        assert_eq!(seconds(1610, 99.0), 25, "clamped to 4x");
    }

    #[test]
    fn hours_and_minutes_are_the_units_a_listener_thinks_in() {
        assert_eq!(human(0), "<1m");
        assert_eq!(human(59), "<1m");
        assert_eq!(human(60), "1m");
        assert_eq!(human(3599), "59m");
        assert_eq!(human(3600), "1h 00m");
        assert_eq!(human(40_000), "11h 06m");
    }

    #[test]
    fn a_duration_is_read_in_the_units_a_listener_says() {
        assert_eq!(parse_duration("45"), Some(45), "a bare number is seconds");
        assert_eq!(parse_duration("90s"), Some(90));
        assert_eq!(parse_duration("30m"), Some(1800));
        assert_eq!(parse_duration("1h"), Some(3600));
        assert_eq!(parse_duration("1h30m"), Some(5400), "the parts add up");
        assert_eq!(
            parse_duration("0s"),
            Some(0),
            "a closed window is still a window"
        );
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("m"), None, "a unit with no number");
        assert_eq!(parse_duration("30x"), None, "no such unit");
        assert_eq!(
            parse_duration("10m5"),
            None,
            "trailing seconds with no unit"
        );
        assert_eq!(parse_duration(" 30m"), None, "the flag is not a shell");
    }
}
