//! The terminal presentation: a wordmark, one live status line, and a slow breath that
//! moves only while a sentence is actually being spoken.
//!
//! Presentation never changes what a pipe sees. With `--quiet`, on a non-terminal,
//! or when `NO_COLOR` is set, the status line falls back to the plain `\r` form,
//! and the wordmark and the breath are skipped entirely. Everything below is
//! `stderr`; `stdout` stays the machine-readable record the spec describes.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
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

/// How often the frame is considered. Drawing more often than the content changes is wasted
/// bytes; drawing less often than the words change is what a reader sees as lag, so the tick
/// is short and identical frames are dropped.
const TICK: Duration = Duration::from_millis(30);
const SWEEP_STEP: Duration = Duration::from_millis(16);
const SWEEP_RELEASE: Duration = Duration::from_millis(120);
const SWEEP_CELLS: usize = 28;
/// One breath: the circle opens and closes. Six frames, symmetric on purpose.
const BREATH: [char; 6] = ['◡', '∘', '⊙', '◎', '⊙', '∘'];
/// Ticks per breath frame (3 × 90ms ≈ 270ms), so one breath lasts about 1.6s.
const BREATH_TICKS: usize = 3;
/// A line that cannot hold at least this many cells drops a counter instead.
const MIN_CELLS: usize = 6;

/// One cell per chapter, or fewer for a book with more chapters than the line can hold.
const SPARK_CELLS: usize = 40;
/// Partial cells, so a chapter the reader is inside reads as progress rather than as empty.
const PARTIALS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
const EMPTY_CELL: char = '░';
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
    /// Whole-book progress. Everywhere else — `ls`, the opening line — a number means the
    /// book, so the live line stops meaning "within this chapter" as well.
    pub percent: u8,
    /// Seconds of listening left, when the record can say. `None` on an index that predates
    /// the per-chapter counts.
    pub left: Option<u64>,
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
    /// Milliseconds to shift the underline by; positive makes it lag the voice.
    sync_offset: AtomicI64,
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
            sync_offset: AtomicI64::new(0),
            paint: Arc::new(Paint::default()),
            clock: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// Shift the underline in time. Everything left is smaller than a word — the engine's
    /// own accuracy and the audio buffer — and neither is ours to measure, so the reader
    /// gets the dial a subtitle player has.
    pub fn set_sync_offset(&self, milliseconds: i64) {
        self.sync_offset.store(milliseconds, Ordering::Relaxed);
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
        let offset_ms = self.sync_offset.load(Ordering::Relaxed);

        let handle = thread::spawn(move || {
            let mut err = io::stderr();
            // The clock starts when the player is handed the file, so the highlight leads
            // the voice by however long the player takes to make sound — a fraction of a
            // word, and the same on every sentence.
            let started = Instant::now();
            let mut last: Vec<String> = Vec::new();
            while !flag.load(Ordering::Relaxed) {
                let snapshot = match state.lock() {
                    Ok(state) => state.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let tick = clock.fetch_add(1, Ordering::Relaxed);
                let sentence = follow.lock().ok().and_then(|slot| slot.clone());
                let source = clock_source.lock().ok().and_then(|slot| slot.clone());
                let moment = moment(source.as_ref(), started, offset_ms);
                let frame = frame(&snapshot, tick, colour, sentence.as_ref(), moment, width);
                // The clock is sampled every tick, but the terminal is only written when the
                // picture differs: a held word and a still breath cost nothing.
                if frame != last {
                    if paint.draw(&mut err, &frame).is_err() {
                        break;
                    }
                    last = frame;
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
fn moment(clock: Option<&Clock>, started: Instant, offset_ms: i64) -> Moment {
    let shift = offset_ms as f32 / 1000.0;
    match clock.and_then(|clock| clock()) {
        Some(position) => Moment {
            at: (position.seconds as f32 - shift).max(0.0),
            paused: position.paused,
        },
        None => Moment {
            at: (started.elapsed().as_secs_f32() - shift).max(0.0),
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
        let rows = follow::layout(sentence, width, active, follow::SENTENCE_ROWS);
        frame.extend(follow::render(&rows, colour));
    }
    frame.push(bar_line_with(
        status,
        Some(tick),
        moment.paused,
        colour,
        width,
    ));
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

/// Columns a line occupies, escape sequences taking none.
fn visible_columns(line: &str) -> usize {
    let mut columns = 0;
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        if character == '\x1b' {
            // Skip to the final byte of the sequence: letters end CSI, `m` included.
            for next in characters.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        columns += 1;
    }
    columns
}

/// Cut a plain line to the width, never mid-escape.
fn cut(line: String, width: usize) -> String {
    if visible_columns(&line) <= width {
        return line;
    }
    line.chars().take(width).collect()
}

/// A cell per chapter: filled for the chapters behind the reader, a partial cell for the one
/// they are inside, faint for the rest. A plain bar says "12% of the way through"; this says
/// "chapter 4 of 27, a third into it", which is the shape of the book the numbers describe.
///
/// A book with more chapters than the line has cells has them grouped — the boundary cell is
/// then the group holding the reader, still partial for how far into the group they are.
pub fn sparkline(status: &Status, room: usize) -> String {
    let chapters = status.chapters.max(1);
    let cells = chapters.min(SPARK_CELLS).min(room);
    let filled = if status.total == 0 {
        0.0
    } else {
        (status.sentence as f32 / status.total as f32).clamp(0.0, 1.0)
    };
    let mut line = String::with_capacity(cells);
    for cell in 0..cells {
        // Which chapters this cell stands for, and how much of them is done.
        let start = cell * chapters / cells;
        let end = ((cell + 1) * chapters / cells).max(start + 1);
        let done = if status.chapter >= end {
            1.0
        } else if status.chapter < start {
            0.0
        } else {
            // The cell holding the reader: whole chapters behind them in this group, plus
            // how far through the current chapter they are.
            let behind = (status.chapter - start) as f32;
            let span = (end - start) as f32;
            ((behind + filled) / span).clamp(0.0, 1.0)
        };
        line.push(cell_glyph(done));
    }
    line
}

fn cell_glyph(done: f32) -> char {
    if done <= 0.0 {
        return EMPTY_CELL;
    }
    if done >= 1.0 {
        return PARTIALS[PARTIALS.len() - 1];
    }
    // Nearest eighth, not the eighth below: half a chapter should read as half a cell.
    let eighths = (done * PARTIALS.len() as f32).round() as usize;
    PARTIALS[eighths.saturating_sub(1).min(PARTIALS.len() - 2)]
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
    bar_line_with(status, tick, false, colour, 80)
}

/// [bar_line] with the player's own state: a breath while a sentence is being spoken, and
/// a pause mark while it is held. Nothing here animates on a pause, because nothing is
/// happening — that is the point of the mark.
pub fn bar_line_with(
    status: &Status,
    tick: Option<usize>,
    paused: bool,
    colour: bool,
    width: usize,
) -> String {
    // The book's number, not this chapter's: the same figure `ls` prints, so a reader never
    // has to remember which of the two they are looking at.
    let percent = status.percent;
    // The line has to fit: a wrapped row breaks the cursor arithmetic the whole block depends
    // on, so when the terminal is narrow the counters give way before the cells do — time left
    // first, then the sentence counters, then the voice, because those are the parts a reader
    // can live without for one chapter.
    let counters = |time: bool, sentences: bool, voice: bool| -> String {
        let mut parts: Vec<String> = vec![format!("ch {}/{}", status.chapter + 1, status.chapters)];
        if sentences {
            parts.push(format!("{}/{}", status.sentence, status.total));
        }
        if let (true, Some(seconds)) = (time, status.left) {
            parts.push(format!("~{} left", crate::estimate::human(seconds)));
        }
        if voice {
            parts.push(format!("{} ×{:.2}", status.voice, status.speed));
        }
        parts.join(" · ")
    };

    // Below this a cell per chapter is a sliver and the numbers are what a reader can use.
    let cells_worth_it = width >= 40;
    let wanted_cells = status.chapters.min(SPARK_CELLS);
    // The longest line first, but the voice name goes before the cells do: it is the same on
    // every line of a run, while the shape of the book is what the line is for.
    let mut chosen = counters(false, false, false);
    let mut track = String::new();
    let mut fitted: Option<(String, String)> = None;
    for (time, sentences, voice) in [
        (true, true, false),
        (true, true, true),
        (false, true, false),
        (false, false, false),
    ] {
        let candidate = counters(time, sentences, voice);
        let reserved = 2 + 1 + 4 + 2 + candidate.chars().count() + 3;
        let budget = width.saturating_sub(reserved);
        let shape = if cells_worth_it && budget >= MIN_CELLS {
            sparkline(status, budget)
        } else {
            String::new()
        };
        let total = 2 + shape.chars().count() + 1 + 4 + 2 + candidate.chars().count() + 3;
        if total > width {
            continue;
        }
        // One cell per chapter when the width allows it, otherwise the best fit seen so far.
        if fitted.is_none() {
            fitted = Some((candidate.clone(), shape.clone()));
        }
        if shape.chars().count() >= wanted_cells || !cells_worth_it {
            chosen = candidate;
            track = shape;
            break;
        }
        if let Some((best_counters, best_shape)) = &fitted {
            chosen = best_counters.clone();
            track = best_shape.clone();
        }
    }
    let tail = match tick {
        None => "   ".to_string(),
        Some(_) if paused && colour => format!("  {ACCENT}{PAUSE_MARK}{RESET}"),
        Some(_) if paused => format!("  {PAUSE_MARK}"),
        Some(tick) if colour => format!("  {ACCENT}{}{RESET}", breath(tick)),
        Some(tick) => format!("  {}", breath(tick)),
    };
    // A terminal narrower than the counters themselves cannot be satisfied by dropping parts;
    // the line is then cut rather than allowed to wrap.
    if !colour {
        return cut(
            format!("{CLEAR}  {track} {percent:>3}%  {chosen}{tail}"),
            width,
        );
    }
    let coloured =
        format!("{CLEAR}  {ACCENT}{track}{RESET} {percent:>3}%  {DIM}{chosen}{RESET}{tail}");
    if visible_columns(&coloured) <= width {
        return coloured;
    }
    // The styling costs no columns but the escapes count as characters if the line is cut, so a
    // narrow terminal gets the readable plain line instead of a broken coloured one.
    cut(format!("{CLEAR}  {track} {percent:>3}%  {chosen}"), width)
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
            percent: 33,
            left: None,
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
    fn the_line_fits_the_width_it_was_given() {
        let mut wide = status(300, 600);
        wide.chapter = 12;
        wide.chapters = 27;
        wide.left = Some(2_400);
        // 60 columns is where the cells and the counters can still share the line.
        let line = bar_line_with(&wide, Some(1), false, false, 60);
        let visible = line.trim_start_matches('\r');
        assert!(
            visible_columns(line.as_str()) <= 60,
            "a wrapped row breaks the frame arithmetic: {visible:?} is {} columns",
            visible_columns(line.as_str())
        );
        assert!(
            visible.contains("ch 13/27") && visible.contains("300/600"),
            "the counters survive the cells, not the other way round: {visible:?}"
        );
        assert!(
            visible.contains('█') || visible.contains('░'),
            "and some of the book's shape is on the line: {visible:?}"
        );

        // Narrower than the counters themselves: the cells and the time give way, and the line
        // is still one row.
        let narrow = bar_line_with(&wide, Some(1), false, false, 30);
        let visible = narrow.trim_start_matches('\r');
        assert!(visible.contains("ch 13/27"), "{visible:?}");
        assert!(
            visible_columns(narrow.as_str()) <= 30,
            "still one row: {visible:?} is {} columns",
            visible_columns(narrow.as_str())
        );
        assert!(
            !visible.contains("left"),
            "time left is the first thing dropped: {visible:?}"
        );
        assert!(
            !visible.contains('█') && !visible.contains('░'),
            "and below forty columns a cell per chapter is a sliver, not information: {visible:?}"
        );
    }

    #[test]
    fn the_sparkline_shows_the_book_by_chapter_not_the_chapter_by_sentence() {
        let mut one = status(0, 100);
        one.chapter = 0;
        one.chapters = 8;
        assert_eq!(
            sparkline(&one, SPARK_CELLS),
            "░░░░░░░░",
            "nothing read yet: every chapter ahead"
        );

        let mut half = status(50, 100);
        half.chapter = 3;
        half.chapters = 8;
        let line = sparkline(&half, SPARK_CELLS);
        assert_eq!(line.chars().count(), 8, "one cell per chapter: {line:?}");
        assert!(
            line.starts_with("███"),
            "three whole chapters behind: {line:?}"
        );
        assert_eq!(
            line.chars().nth(3),
            Some('▌'),
            "and half of the fourth: {line:?}"
        );
        assert!(line.ends_with("░░░░"), "four chapters ahead: {line:?}");

        let mut last = status(100, 100);
        last.chapter = 7;
        last.chapters = 8;
        assert_eq!(sparkline(&last, SPARK_CELLS), "████████");
    }

    #[test]
    fn a_book_with_more_chapters_than_cells_keeps_the_boundary_honest() {
        let mut huge = status(50, 100);
        huge.chapter = 100;
        huge.chapters = 400;
        let line = sparkline(&huge, SPARK_CELLS);
        assert_eq!(line.chars().count(), SPARK_CELLS, "grouped, not truncated");
        let filled = line.chars().filter(|glyph| *glyph == '█').count();
        assert_eq!(
            filled, 10,
            "a quarter of the book is a quarter of the cells: {line:?}"
        );
        assert_ne!(
            line.chars().nth(10),
            Some('░'),
            "the cell holding the reader is partial: {line:?}"
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
    fn the_percentage_is_the_books_not_the_chapters() {
        let mut early = status(0, 17);
        early.chapter = 4;
        early.percent = 11;
        let line = bar_line(&early, None, false);
        assert!(line.contains(" 11%"), "the book's figure: {line:?}");
        assert!(
            !line.contains("  0%"),
            "not the chapter's zero-of-seventeen: {line:?}"
        );
    }

    #[test]
    fn the_bar_line_carries_the_counters_and_never_colours_without_asking() {
        let mut sample = status(38, 287);
        sample.percent = 13;
        let line = bar_line(&sample, Some(3), false);
        assert!(line.contains("13%"), "got {line}");
        assert!(line.contains("ch 9/25"));
        assert!(line.contains("38/287"));
        // The voice name yields to the book's shape when the line is tight: it is the same on
        // every line of a run, and it is audible anyway.
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
            100,
        );
        assert!(with.len() > 2, "block plus bar: {with:?}");
        assert!(
            with.iter().any(|row| row.contains("quick brown fox")),
            "the sentence is on screen: {with:?}"
        );
        assert!(
            with.last().expect("bar").contains("ch 9/25"),
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
            100,
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
            100,
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
            100,
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
        let held = moment(Some(&frozen), started, 0);
        assert_eq!(held.at, 2.5, "a paused player's position does not advance");
        assert!(held.paused, "and it says why");
        assert!(
            moment(None, started, 0).at > 0.0,
            "without a player the wall clock answers"
        );
        assert!(
            !moment(None, started, 0).paused,
            "a wall clock is never paused: it cannot know"
        );
        let silent: Clock = Arc::new(|| None);
        assert!(
            moment(Some(&silent), started, 0).at > 0.0,
            "a player that has not answered yet falls back rather than freezing at zero"
        );
    }

    #[test]
    fn the_sync_dial_shifts_both_clocks() {
        let clock: Clock = Arc::new(|| {
            Some(Position {
                seconds: 2.0,
                paused: false,
            })
        });
        let started = Instant::now();
        assert_eq!(moment(Some(&clock), started, 0).at, 2.0);
        assert_eq!(
            moment(Some(&clock), started, 250).at,
            1.75,
            "positive lags the voice"
        );
        assert_eq!(
            moment(Some(&clock), started, -250).at,
            2.25,
            "negative leads it"
        );
        assert_eq!(
            moment(Some(&clock), started, 9_000).at,
            0.0,
            "and never winds past the start of the sentence"
        );
        assert!(
            moment(None, started, -5_000).at > 0.0,
            "the fallback shifts too"
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
            100,
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
            100,
        );

        let bar_playing = playing.last().expect("bar");
        let bar_held = held.last().expect("bar");
        assert!(!bar_playing.contains(PAUSE_MARK), "{bar_playing:?}");
        assert!(bar_held.contains(PAUSE_MARK), "{bar_held:?}");
        assert!(
            !bar_held.chars().any(|glyph| BREATH.contains(&glyph)),
            "nothing breathes while the book is held: {bar_held:?}"
        );
        let coloured = bar_line_with(&status, Some(5), true, true, 80);
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
