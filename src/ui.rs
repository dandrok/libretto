//! The terminal presentation: a wordmark, one live status line, and a slow breath that
//! moves only while a sentence is actually being spoken.
//!
//! Presentation never changes what a pipe sees. With `--quiet`, on a non-terminal,
//! or when `NO_COLOR` is set, the status line falls back to the plain `\r` form,
//! and the wordmark and the breath are skipped entirely. Everything below is
//! `stderr`; `stdout` stays the machine-readable record the spec describes.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::play::{PlayError, Player};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
/// Warm amber. Swap for `38;5;109` (green) or `38;5;74` (blue) to taste.
const ACCENT: &str = "\x1b[38;5;179m";
const CLEAR: &str = "\r\x1b[2K";

const TICK: Duration = Duration::from_millis(90);
const SWEEP_STEP: Duration = Duration::from_millis(16);
const SWEEP_RELEASE: Duration = Duration::from_millis(120);
const SWEEP_CELLS: usize = 28;
/// One breath: the circle opens and closes. Six frames, symmetric on purpose.
const BREATH: [char; 6] = ['◡', '∘', '⊙', '◎', '⊙', '∘'];
/// Ticks per breath frame (3 × 90ms ≈ 270ms), so one breath lasts about 1.6s.
const BREATH_TICKS: usize = 3;
const BAR_CELLS: usize = 20;

const WORDMARK: &str = "\
 _ _ _            _   _
| (_) |__ _ _ ___| |_| |_ ___
| | | '_ \\ '_/ -_)  _|  _/ _ \\
|_|_|_.__/_| \\___|\\__|\\__\\___/
";

/// What the live line is currently saying. Cheap to clone: the breath thread takes a
/// snapshot per frame rather than holding the lock while it sleeps.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Already shortened by the caller; the UI never truncates book-derived text.
    pub title: String,
    /// 0-based, as stored.
    pub chapter: usize,
    pub chapters: usize,
    pub sentence: usize,
    pub total: usize,
    pub voice: String,
    pub speed: f32,
}

/// The presentation for one `read` run.
pub struct Ui {
    interactive: bool,
    colour: bool,
    state: Arc<Mutex<Status>>,
    /// Shared across pulses, so the drop keeps falling between sentences instead
    /// of restarting at the tap every time the player exits.
    clock: Arc<AtomicUsize>,
}

impl Ui {
    /// `quiet` wins over everything; after that, a terminal decides.
    pub fn new(quiet: bool) -> Self {
        let interactive = !quiet && io::stderr().is_terminal();
        let colour = interactive && std::env::var_os("NO_COLOR").is_none();
        Self {
            interactive,
            colour,
            state: Arc::new(Mutex::new(Status::default())),
            clock: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn interactive(&self) -> bool {
        self.interactive
    }

    /// The wordmark, then one sweep that fills left to right and clears itself.
    /// About half a second, once per run: enough to read, short enough to ignore.
    pub fn splash(&self) {
        if !self.interactive {
            return;
        }
        let mut err = io::stderr();

        let _ = writeln!(err);
        for line in WORDMARK.lines() {
            if self.colour {
                let _ = writeln!(err, "  {DIM}{line}{RESET}");
            } else {
                let _ = writeln!(err, "  {line}");
            }
        }

        for filled in 0..=SWEEP_CELLS {
            self.write_partial(&mut err, &sweep(filled, self.colour));
            thread::sleep(SWEEP_STEP);
        }
        thread::sleep(SWEEP_RELEASE);
        self.write_partial(&mut err, CLEAR);
    }

    /// Draw the first frame before any sentence has played, so the breath starts
    /// over real counters. Silent in plain mode: that line only moves when the
    /// position actually does.
    pub fn open(&self, status: Status) {
        if self.interactive {
            self.draw(status, None);
        }
    }

    /// Move the line to a new position and draw it once, without the breath.
    pub fn note(&self, status: Status) {
        if !self.interactive {
            let mut err = io::stderr();
            let _ = write!(err, "{}", plain_line(&status));
            let _ = err.flush();
            return;
        }
        self.draw(status, None);
    }

    fn draw(&self, status: Status, tick: Option<usize>) {
        let mut err = io::stderr();
        let redrawn = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            *state = status;
            state.clone()
        };
        self.write_partial(&mut err, &bar_line(&redrawn, tick, self.colour));
    }

    /// Animate the breath until the returned guard is dropped — i.e. exactly
    /// while the player owns the sentence.
    pub fn pulse(&self) -> Option<Pulse> {
        if !self.interactive {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::clone(&self.state);
        let clock = Arc::clone(&self.clock);
        let flag = Arc::clone(&stop);
        let colour = self.colour;

        let handle = thread::spawn(move || {
            let mut err = io::stderr();
            while !flag.load(Ordering::Relaxed) {
                let snapshot = match state.lock() {
                    Ok(state) => state.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let tick = clock.fetch_add(1, Ordering::Relaxed);
                let line = bar_line(&snapshot, Some(tick), colour);
                if write!(err, "{line}").is_err() {
                    break;
                }
                let _ = err.flush();
                thread::sleep(TICK);
            }
        });

        Some(Pulse {
            stop,
            handle: Some(handle),
        })
    }

    /// End the live line, so the summary that follows starts on a clean row.
    pub fn finish(&self) {
        let mut err = io::stderr();
        let _ = writeln!(err);
        let _ = err.flush();
    }

    fn write_partial(&self, err: &mut impl Write, text: &str) {
        let _ = write!(err, "{text}");
        let _ = err.flush();
    }
}

/// Stops the breath when it goes out of scope, whether the player succeeded or not.
pub struct Pulse {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Pulse {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A player that animates while the sentence is playing. The loop in `play` knows
/// nothing about terminals: it hands us a file, and the breath lives as long as the
/// call does.
pub struct Speaking<'a> {
    inner: &'a dyn Player,
    ui: &'a Ui,
}

impl<'a> Speaking<'a> {
    pub fn new(inner: &'a dyn Player, ui: &'a Ui) -> Self {
        Self { inner, ui }
    }
}

impl Player for Speaking<'_> {
    fn play(&self, file: &Path) -> Result<(), PlayError> {
        let _pulse = self.ui.pulse();
        self.inner.play(file)
    }
}

/// The pre-UI status line, byte for byte. Scripts and tests have seen this shape
/// since 1.0, so it stays.
pub fn plain_line(status: &Status) -> String {
    format!(
        "\r  {} · ch {}/{} · {}/{} · {} ×{:.2}   ",
        status.title,
        status.chapter + 1,
        status.chapters,
        status.sentence,
        status.total,
        status.voice,
        status.speed
    )
}

/// The same information with a bar, a percentage and — while a sentence plays —
/// a breath. `tick` is `Some` only while the player is running.
///
/// The breath is ambient on purpose: it says "audio is
/// happening", which is exactly what the player call proves. A waveform here
/// would claim to follow the voice, and nothing in this program knows that.
pub fn bar_line(status: &Status, tick: Option<usize>, colour: bool) -> String {
    let percent = status
        .sentence
        .checked_mul(100)
        .and_then(|scaled| scaled.checked_div(status.total))
        .unwrap_or(0);
    let track = bar(status.sentence, status.total, BAR_CELLS);
    let counters = format!(
        "ch {}/{} · {}/{} · {} ×{:.2}",
        status.chapter + 1,
        status.chapters,
        status.sentence,
        status.total,
        status.voice,
        status.speed
    );
    let tail = match tick {
        None => "   ".to_string(),
        Some(tick) if colour => format!("  {ACCENT}{}{RESET}", breath(tick)),
        Some(tick) => format!("  {}", breath(tick)),
    };
    if !colour {
        return format!("{CLEAR}  {track} {percent:>3}%  {counters}{tail}");
    }
    format!("{CLEAR}  {ACCENT}{track}{RESET} {percent:>3}%  {DIM}{counters}{RESET}{tail}")
}

fn sweep(filled: usize, colour: bool) -> String {
    let line = "▔".repeat(filled.min(SWEEP_CELLS));
    if colour {
        format!("{CLEAR}  {ACCENT}{line}{RESET}")
    } else {
        format!("{CLEAR}  {line}")
    }
}

/// A slow breath: `◡ ∘ ⊙ ◎ ⊙ ∘` over about 1.6 seconds.
///
/// It says exactly one true thing — audio is playing — and it has no direction,
/// so there is nothing for it to fall out of sync with. `tick` is the clock; the
/// caller owns it.
pub fn breath(tick: usize) -> char {
    BREATH[(tick / BREATH_TICKS) % BREATH.len()]
}

/// `████░░░░` proportional to `done / total`, `cells` wide.
pub fn bar(done: usize, total: usize, cells: usize) -> String {
    let filled = done
        .checked_mul(cells)
        .and_then(|scaled| scaled.checked_add(total / 2))
        .and_then(|scaled| scaled.checked_div(total))
        .unwrap_or(0)
        .min(cells);
    format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(cells.saturating_sub(filled))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(sentence: usize, total: usize) -> Status {
        Status {
            title: "Grokking Algorithms".to_string(),
            chapter: 8,
            chapters: 25,
            sentence,
            total,
            voice: "af_heart".to_string(),
            speed: 1.0,
        }
    }

    #[test]
    fn the_plain_line_matches_what_scripts_have_always_seen() {
        assert_eq!(
            plain_line(&status(38, 287)),
            "\r  Grokking Algorithms · ch 9/25 · 38/287 · af_heart ×1.00   "
        );
    }

    #[test]
    fn the_bar_is_proportional_and_never_overflows() {
        assert_eq!(bar(0, 100, 10), "░░░░░░░░░░");
        assert_eq!(bar(100, 100, 10), "██████████");
        assert_eq!(bar(50, 100, 10), "█████░░░░░");
        assert_eq!(bar(200, 100, 10), "██████████", "clamped past the end");
        assert_eq!(bar(5, 0, 10), "░░░░░░░░░░", "no total, no fill");
        assert_eq!(bar(1, 3, 10), "███░░░░░░░", "rounded, not truncated");
    }

    #[test]
    fn the_breath_opens_and_closes() {
        let frames: Vec<char> = (0..BREATH.len() * BREATH_TICKS)
            .step_by(BREATH_TICKS)
            .map(breath)
            .collect();
        assert_eq!(frames, BREATH.to_vec(), "every frame, in order");

        // A breath widens to a peak and comes back: same shape both ways.
        let peak = frames.iter().position(|c| *c == '◎').expect("a peak");
        assert_eq!(peak, BREATH.len() / 2, "the widest glyph is in the middle");
        assert_eq!(breath(0), breath(BREATH.len() * BREATH_TICKS), "wraps");
        assert_eq!(
            breath(usize::MAX),
            breath(usize::MAX % (BREATH.len() * BREATH_TICKS))
        );
    }

    #[test]
    fn the_bar_line_carries_the_counters_and_never_colours_without_asking() {
        let line = bar_line(&status(38, 287), Some(3), false);
        assert!(line.contains("13%"), "got {line}");
        assert!(line.contains("ch 9/25"));
        assert!(line.contains("38/287"));
        assert!(line.contains("af_heart ×1.00"));
        // Erasing the line needs `ESC[2K`; colour is a separate promise.
        for escape in [ACCENT, DIM, RESET] {
            assert!(!line.contains(escape), "coloured without asking: {line:?}");
        }
    }

    #[test]
    fn colour_appears_only_when_asked_for() {
        let coloured = bar_line(&status(38, 287), None, true);
        assert!(coloured.contains(ACCENT));
        assert!(coloured.contains(RESET));
    }

    #[test]
    fn quiet_wins_over_a_terminal() {
        let ui = Ui::new(true);
        assert!(!ui.interactive(), "--quiet has to mean quiet");
    }
}
