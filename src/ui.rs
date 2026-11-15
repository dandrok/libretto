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
use std::time::{Duration, Instant};

use crate::captions;
use crate::follow;

use crate::play::{PlayError, Player, Position};

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
/// What the breath becomes when the player is paused: a held sentence, not a silent one.
const PAUSE_MARK: &str = "❚❚";

const WORDMARK: &str = "\
 _ _ _            _   _
| (_) |__ _ _ ___| |_| |_ ___
| | | '_ \\ '_/ -_)  _|  _/ _ \\
|_|_|_.__/_| \\___|\\__|\\__\\___/
";

/// Where playback is now, asked once per frame. `None` when that instant has no position
/// — before the player has answered, or for a player with no IPC socket at all.
pub type Clock = Arc<dyn Fn() -> Option<Position> + Send + Sync>;

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
    /// The sentence the read-along draws, if any. `read` never sets it.
    follow: Arc<Mutex<Option<follow::Sentence>>>,
    /// Columns available to the block, margin already taken off.
    width: usize,
    /// The player's own position, once it has one. A wall clock is the fallback.
    clock_source: Arc<Mutex<Option<Clock>>>,
    /// Where the previous frame stood. Shared across pulses: a block is taller than one
    /// line, and the next sentence's pulse must know how far up to go.
    paint: Arc<Paint>,
    /// Shared across pulses, so the drop keeps falling between sentences instead
    /// of restarting at the tap every time the player exits.
    clock: Arc<AtomicUsize>,
}

impl Ui {
    /// `quiet` wins over everything; after that, a terminal decides.
    pub fn new(quiet: bool) -> Self {
        Self::with_width(quiet, None)
    }

    /// The read-along needs a width before it can wrap: `--width`, then `$COLUMNS`, then
    /// eighty columns, always with two columns of margin so no row touches the edge.
    pub fn with_width(quiet: bool, width: Option<usize>) -> Self {
        let interactive = !quiet && io::stderr().is_terminal();
        let colour = interactive && std::env::var_os("NO_COLOR").is_none();
        let columns = width
            .or_else(|| {
                std::env::var("COLUMNS")
                    .ok()
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap_or(80)
            .clamp(22, 400);
        Self {
            interactive,
            colour,
            state: Arc::new(Mutex::new(Status::default())),
            follow: Arc::new(Mutex::new(None)),
            width: columns - 2,
            clock_source: Arc::new(Mutex::new(None)),
            paint: Arc::new(Paint::default()),
            clock: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// Teach the view to ask the player where it is. Without this the underline follows a
    /// wall clock, which keeps running while a paused player holds the file.
    pub fn set_clock(&self, clock: Clock) {
        let mut slot = match self.clock_source.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = Some(clock);
    }

    /// Whether a read-along sentence is on screen.
    fn following(&self) -> bool {
        match self.follow.lock() {
            Ok(slot) => slot.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        }
    }

    /// Hand the read-along the sentence about to be spoken. Called between sentences,
    /// which is how the block moves from one sentence to the next.
    pub fn set_follow(&self, sentence: Option<follow::Sentence>) {
        let mut slot = match self.follow.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = sentence;
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
        // Following draws the block *and* the bar every frame, so a lone bar row here
        // would land under the block and start a new one below the old.
        if self.following() {
            return;
        }
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
        let follow = Arc::clone(&self.follow);
        let clock = Arc::clone(&self.clock);
        let flag = Arc::clone(&stop);
        let colour = self.colour;
        let width = self.width;
        let paint = Arc::clone(&self.paint);
        let clock_source = Arc::clone(&self.clock_source);

        let handle = thread::spawn(move || {
            let mut err = io::stderr();
            // The clock starts when the player is handed the file, so the highlight leads
            // the voice by however long the player takes to make sound — a fraction of a
            // word, and the same on every sentence.
            let started = Instant::now();
            while !flag.load(Ordering::Relaxed) {
                let snapshot = match state.lock() {
                    Ok(state) => state.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let tick = clock.fetch_add(1, Ordering::Relaxed);
                let sentence = follow.lock().ok().and_then(|slot| slot.clone());
                let source = clock_source.lock().ok().and_then(|slot| slot.clone());
                let moment = moment(source.as_ref(), started);
                let frame = frame(&snapshot, tick, colour, sentence.as_ref(), moment, width);
                if paint.draw(&mut err, &frame).is_err() {
                    break;
                }
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

/// One frame's worth of clock: where the underline goes, and whether the audio is held.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Moment {
    at: f32,
    paused: bool,
}

/// The player's own position when it reports one, and the wall clock when it cannot. A
/// paused player stops reporting progress, so the underline stops with it — which is the
/// whole reason to ask — and it also says so, so the line can stop breathing.
fn moment(clock: Option<&Clock>, started: Instant) -> Moment {
    match clock.and_then(|clock| clock()) {
        Some(position) => Moment {
            at: position.seconds.max(0.0) as f32,
            paused: position.paused,
        },
        None => Moment {
            at: started.elapsed().as_secs_f32(),
            paused: false,
        },
    }
}

/// One frame: the read-along block, if a sentence was handed over, and always the bar line.
fn frame(
    status: &Status,
    tick: usize,
    colour: bool,
    sentence: Option<&follow::Sentence>,
    moment: Moment,
    width: usize,
) -> Vec<String> {
    let mut frame: Vec<String> = Vec::new();
    if let Some(sentence) = sentence {
        let active = captions::index_at(&sentence.words, moment.at);
        let rows = follow::layout(sentence, width, active);
        frame.extend(follow::render(&rows, colour));
    }
    frame.push(bar_line_with(status, Some(tick), moment.paused, colour));
    frame
}

/// Where the previous frame stood, so the next one can be drawn over it instead of
/// below it. One of these lives for the whole run: a block is taller than one line, and
/// a fresh counter per sentence would leave every sentence on screen.
#[derive(Debug, Default)]
struct Paint {
    drawn: AtomicUsize,
}

impl Paint {
    /// Move up over the previous frame, rewrite every row, and erase whatever is left
    /// below when the new frame is shorter than the old.
    fn draw(&self, out: &mut impl Write, frame: &[String]) -> io::Result<()> {
        let drawn = self.drawn.load(Ordering::Relaxed);
        let mut buffer = String::new();
        if drawn > 0 {
            buffer.push_str(&format!("\x1b[{drawn}F"));
        }
        for line in frame {
            buffer.push_str("\r\x1b[2K");
            buffer.push_str(line);
            buffer.push('\n');
        }
        if frame.len() < drawn {
            buffer.push_str("\x1b[J");
        }
        self.drawn.store(frame.len(), Ordering::Relaxed);
        write!(out, "{buffer}")?;
        out.flush()
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
    bar_line_with(status, tick, false, colour)
}

/// [bar_line] with the player's own state: a breath while a sentence is being spoken, and
/// a pause mark while it is held. Nothing here animates on a pause, because nothing is
/// happening — that is the point of the mark.
pub fn bar_line_with(status: &Status, tick: Option<usize>, paused: bool, colour: bool) -> String {
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
        Some(_) if paused && colour => format!("  {ACCENT}{PAUSE_MARK}{RESET}"),
        Some(_) if paused => format!("  {PAUSE_MARK}"),
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

    fn following_sentence() -> follow::Sentence {
        follow::Sentence {
            previous: Some("before this".to_string()),
            current: "the quick brown fox jumps over the lazy dog".to_string(),
            next: Some("after this".to_string()),
            words: captions::interpolate("the quick brown fox jumps over the lazy dog", 4.0),
        }
    }

    #[test]
    fn a_frame_with_a_sentence_draws_the_block_and_then_the_bar() {
        let status = status(3, 40);
        let with = frame(
            &status,
            0,
            false,
            Some(&following_sentence()),
            Moment {
                at: 1.0,
                paused: false,
            },
            40,
        );
        assert!(with.len() > 2, "block plus bar: {with:?}");
        assert!(
            with.iter().any(|row| row.contains("quick brown fox")),
            "the sentence is on screen: {with:?}"
        );
        assert!(
            with.last().expect("bar").contains("af_heart"),
            "the bar is last"
        );

        let without = frame(
            &status,
            0,
            false,
            None,
            Moment {
                at: 0.0,
                paused: false,
            },
            40,
        );
        assert_eq!(without.len(), 1, "read draws one line and nothing else");
    }

    #[test]
    fn the_highlight_advances_with_the_clock() {
        let sentence = following_sentence();
        let start = frame(
            &status(3, 40),
            0,
            true,
            Some(&sentence),
            Moment {
                at: 0.0,
                paused: false,
            },
            40,
        );
        let later = frame(
            &status(3, 40),
            0,
            true,
            Some(&sentence),
            Moment {
                at: 3.0,
                paused: false,
            },
            40,
        );
        assert!(
            start.join("\n").contains(&format!("{ACCENT}{}", "\x1b[4m")),
            "something is lit at zero"
        );
        assert_ne!(start, later, "a different moment lights a different word");
    }

    #[test]
    fn the_second_frame_is_drawn_over_the_first() {
        let paint = Paint::default();
        let four = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];

        let mut out = Vec::new();
        paint.draw(&mut out, &four).expect("draw");
        let first = String::from_utf8_lossy(&out).to_string();
        assert!(!first.contains("\x1b[0F"), "nothing above yet: {first:?}");
        assert!(first.contains("\r\x1b[2Ka\n"), "{first:?}");

        // The same `Paint` is what carries the height from one sentence to the next: a
        // fresh counter here is the bug where every sentence lands below the last one.
        let mut out = Vec::new();
        paint.draw(&mut out, &four).expect("draw");
        let second = String::from_utf8_lossy(&out).to_string();
        assert!(
            second.starts_with("\x1b[4F"),
            "the next sentence moves up over all four rows: {second:?}"
        );

        let mut out = Vec::new();
        paint.draw(&mut out, &["only".to_string()]).expect("draw");
        let smaller = String::from_utf8_lossy(&out).to_string();
        assert!(
            smaller.ends_with("\x1b[J"),
            "and clears the rows it no longer fills: {smaller:?}"
        );
    }

    #[test]
    fn the_player_position_beats_the_wall_clock() {
        let frozen: Clock = Arc::new(|| {
            Some(Position {
                seconds: 2.5,
                paused: true,
            })
        });
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        let held = moment(Some(&frozen), started);
        assert_eq!(held.at, 2.5, "a paused player's position does not advance");
        assert!(held.paused, "and it says why");
        assert!(
            moment(None, started).at > 0.0,
            "without a player the wall clock answers"
        );
        assert!(
            !moment(None, started).paused,
            "a wall clock is never paused: it cannot know"
        );
        let silent: Clock = Arc::new(|| None);
        assert!(
            moment(Some(&silent), started).at > 0.0,
            "a player that has not answered yet falls back rather than freezing at zero"
        );
    }

    #[test]
    fn a_paused_player_stops_breathing_and_shows_a_mark() {
        let status = status(3, 40);
        let sentence = following_sentence();
        let playing = frame(
            &status,
            5,
            false,
            Some(&sentence),
            Moment {
                at: 0.5,
                paused: false,
            },
            40,
        );
        let held = frame(
            &status,
            5,
            false,
            Some(&sentence),
            Moment {
                at: 0.5,
                paused: true,
            },
            40,
        );

        let bar_playing = playing.last().expect("bar");
        let bar_held = held.last().expect("bar");
        assert!(!bar_playing.contains(PAUSE_MARK), "{bar_playing:?}");
        assert!(bar_held.contains(PAUSE_MARK), "{bar_held:?}");
        assert!(
            !bar_held.chars().any(|glyph| BREATH.contains(&glyph)),
            "nothing breathes while the book is held: {bar_held:?}"
        );
        let coloured = bar_line_with(&status, Some(5), true, true);
        assert!(
            coloured.contains(PAUSE_MARK),
            "and the colour path agrees: {coloured:?}"
        );
    }

    #[test]
    fn the_width_flag_wins_over_the_terminal_and_stays_sane() {
        assert_eq!(
            Ui::with_width(true, Some(60)).width(),
            58,
            "two columns of margin"
        );
        assert_eq!(Ui::with_width(true, Some(5)).width(), 20, "never a sliver");
        assert_eq!(Ui::with_width(true, Some(9999)).width(), 398);
    }
}
