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

/// Seconds of speech for `chars` at `speed`. `speed` is clamped to the range the
/// engine accepts, so a stray zero cannot divide by zero.
pub fn seconds(chars: usize, speed: f32) -> u64 {
    let rate = CHARS_PER_SECOND * speed.clamp(0.25, 4.0);
    ((chars as f32) / rate).round() as u64
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
}
