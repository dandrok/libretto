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
/// The waiting mark's own pace: fast enough to read as motion, slow enough not to flicker.
const WAITING_STEP: Duration = Duration::from_millis(90);
/// How long a wait has to last before it is worth drawing at all. An EPUB is parsed in
/// milliseconds, and a line that appears and vanishes inside a single frame is read as a
/// glitch rather than as progress; a PDF is seconds, and then the line is the whole point.
const WAITING_DELAY: Duration = Duration::from_millis(150);
/// Ten frames of a braille spinner — the one animation in the program that means "working"
/// rather than "audio is playing". Braille because every mono face on this machine carries
/// the block (U+2800–U+28FF); the quarter circles fall back to another font and change size.
const WAITING: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// One breath: the circle opens and closes. Six frames, symmetric on purpose.
const BREATH: [char; 6] = ['◡', '∘', '⊙', '◎', '⊙', '∘'];
/// Ticks per breath frame (3 × 90ms ≈ 270ms), so one breath lasts about 1.6s.
const BREATH_TICKS: usize = 3;

/// One cell per chapter, or fewer for a book with more chapters than the line can hold.
/// The meter's width, brackets aside. Twenty is about where a reader stops counting cells and
/// starts seeing a proportion.
const METER_CELLS: usize = 20;
/// Partial cells, so a chapter the reader is inside reads as progress rather than as empty.
const PARTIALS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
const EMPTY_CELL: char = '░';
/// What the breath becomes when the player is paused: a held sentence, not a silent one.
const PAUSE_MARK: &str = "❚❚";

/// Six rows, sixty-two columns, drawn with full blocks and box drawing — the only family of
/// glyphs that fills its cell on both axes, so the letters close up at any size and at any
/// downscale. Two earlier marks were tried and both failed the same way: `figlet small` and
/// `figlet standard` draw with `_`, `/` and `|`, and an underscore sits mid-cell in a monospace
/// face, so the art read as a row of detached dashes in the very font the README records in.
/// A rendering was checked by eye, not just measured: a mark that fits and cannot be read is
/// worse than a wide one.
const WORDMARK: &str = "\
██╗     ██╗██████╗ ██████╗ ███████╗████████╗████████╗ ██████╗
██║     ██║██╔══██╗██╔══██╗██╔════╝╚══██╔══╝╚══██╔══╝██╔═══██╗
██║     ██║██████╔╝██████╔╝█████╗     ██║      ██║   ██║   ██║
██║     ██║██╔══██╗██╔══██╗██╔══╝     ██║      ██║   ██║   ██║
███████╗██║██████╔╝██║  ██║███████╗   ██║      ██║   ╚██████╔╝
╚══════╝╚═╝╚═════╝ ╚═╝  ╚═╝╚══════╝   ╚═╝      ╚═╝    ╚═════╝";

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

    /// The wordmark, and — if the work outlasts a moment — a line saying what it waits for.
    ///
    /// The wordmark is printed first because opening a book can take seconds before any of
    /// the book is known: `pdftotext` converts the whole file on every run, and the opening
    /// header only exists once the chapters do. A terminal with nothing on it reads as a hang.
    ///
    /// The line itself waits: on a fast open it is never drawn, because a row that blinks in
    /// and out is worse than no row at all.
    pub fn opening(&self, label: &str) -> Working {
        let mut working = Working::silent();
        if !self.interactive {
            return working;
        }
        let mut err = io::stderr();
        self.wordmark(&mut err);
        let _ = err.flush();
        working.start(label, self.colour, self.width);
        working
    }

    /// The waiting line without the wordmark — for a command with no live view of its own.
    /// `add` parses the file exactly as `read` does, but an import does not need four rows of
    /// banner over it.
    pub fn waiting(&self, label: &str) -> Working {
        let mut working = Working::silent();
        if self.interactive {
            working.start(label, self.colour, self.width);
        }
        working
    }

    /// Four rows of dim art, once per run.
    fn wordmark(&self, err: &mut impl Write) {
        let _ = writeln!(err);
        for line in WORDMARK.lines() {
            if self.colour {
                let _ = writeln!(err, "  {DIM}{line}{RESET}");
            } else {
                let _ = writeln!(err, "  {line}");
            }
        }
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
        // Drawn through the same painter the breath uses. It alone knows where the frame
        // stands, so a draw after the pulse has stopped rewrites that row instead of writing
        // a second bar under the first.
        let row = bar_line_with(&redrawn, tick, false, self.colour, self.width);
        let _ = self.paint.draw(&mut err, &[row]);
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
        // A painted frame parks the cursor on the row below it already; only a run that never
        // painted (a pipe) stopped mid-line and needs the newline.
        if self.paint.rows() == 0 {
            let _ = writeln!(err);
        }
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
    /// How many rows the last frame took, or 0 if nothing has been drawn yet.
    fn rows(&self) -> usize {
        self.drawn.load(Ordering::Relaxed)
    }

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

/// The waiting line, alive for as long as the work it describes.
///
/// On a terminal it owns one row under the wordmark and gives it back empty when it is
/// dropped, so whatever is printed next starts where the line was. Off a terminal it owns
/// nothing and writes nothing: a pipe's bytes are the spec's, and the wordmark and the
/// spinner are the terminal's flourish alone.
pub struct Working {
    label: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Working {
    /// A guard that draws nothing. Constructed for pipes, `--quiet`, and tests.
    fn silent() -> Self {
        Self {
            label: Arc::new(Mutex::new(String::new())),
            stop: Arc::new(AtomicBool::new(true)),
            handle: None,
        }
    }

    /// Change what the line says — the phases of opening a book are not known up front.
    pub fn say(&self, label: impl Into<String>) {
        if let Ok(mut slot) = self.label.lock() {
            *slot = label.into();
        }
    }

    fn start(&mut self, label: &str, colour: bool, width: usize) {
        self.say(label);
        let text = Arc::clone(&self.label);
        let stop = Arc::clone(&self.stop);
        stop.store(false, Ordering::Relaxed);
        let handle = thread::spawn(move || {
            let mut err = io::stderr();
            // Nothing is drawn until the wait has lasted long enough to be worth saying.
            let deadline = Instant::now() + WAITING_DELAY;
            while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(remaining.min(WAITING_STEP));
            }
            let mut tick = 0;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let label = match text.lock() {
                    Ok(slot) => slot.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let frame = working_line(&label, tick, colour, width);
                if write!(err, "{frame}").is_err() || err.flush().is_err() {
                    break;
                }
                tick += 1;
                thread::sleep(WAITING_STEP);
            }
        });
        self.handle = Some(handle);
    }
}

impl Drop for Working {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
            // Leave an empty row behind: the header that follows is written by another
            // stream, in another place, and must not land on a half-erased line.
            let mut err = io::stderr();
            let _ = write!(err, "{CLEAR}");
            let _ = err.flush();
        }
    }
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

/// `[██████████░░░░░░░░░░]` — the book's own percentage across twenty cells, with the cell
/// holding the reader cut to the nearest eighth.
///
/// Bracketed so the bar has edges: a bar that fades into the line reads as a gap that happens to
/// be full of block characters, and at 1% it reads as a rendering fault.
pub fn meter(percent: u8, room: usize) -> String {
    let cells = METER_CELLS.min(room.saturating_sub(2));
    if cells == 0 {
        return String::new();
    }
    let eighths = (percent.min(100) as usize) * cells * 8 / 100;
    let full = (eighths / 8).min(cells);
    let partial = eighths % 8;
    let mut inside = String::with_capacity(cells);
    for _ in 0..full {
        inside.push(PARTIALS[PARTIALS.len() - 1]);
    }
    if partial > 0 && full < cells {
        inside.push(PARTIALS[partial - 1]);
    }
    while inside.chars().count() < cells {
        inside.push(EMPTY_CELL);
    }
    format!("[{inside}]")
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
    // No padding: a right-aligned `1%` in a three-column field leaves a hole the eye reads as a
    // gap between the cells and the number.
    let pct = format!("{percent}%");
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

    // Wide lines first, but the voice name yields before the meter does: the meter is the part a
    // reader glances at, and the voice is the same on every line of a run.
    let room_for_meter = width >= 40;
    let mut fitted: Option<(String, String)> = None;
    for with_meter in [room_for_meter, false] {
        for (time, sentences, voice) in [
            (true, true, false),
            (true, true, true),
            (false, true, false),
            (false, false, false),
        ] {
            let candidate = counters(time, sentences, voice);
            let shape = if with_meter {
                meter(percent, width)
            } else {
                String::new()
            };
            let total =
                2 + shape.chars().count() + 1 + pct.len() + 2 + candidate.chars().count() + 3;
            if total <= width {
                fitted = Some((candidate, shape));
                break;
            }
        }
        if fitted.is_some() {
            break;
        }
    }
    let (chosen, track) = fitted.unwrap_or_else(|| (counters(false, false, false), String::new()));
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
        return cut(format!("{CLEAR}  {track} {pct}  {chosen}{tail}"), width);
    }
    let coloured = format!("{CLEAR}  {ACCENT}{track}{RESET} {pct}  {DIM}{chosen}{RESET}{tail}");
    if visible_columns(&coloured) <= width {
        return coloured;
    }
    // The styling costs no columns but the escapes count as characters if the line is cut, so a
    // narrow terminal gets the readable plain line instead of a broken coloured one.
    cut(format!("{CLEAR}  {track} {pct}  {chosen}"), width)
}

/// One frame of the waiting line: a spinner, then what is being waited for.
///
/// The line is the terminal's, so it stays inside the width it was handed — a wrapped row
/// would put the next write in the wrong place. A label too long for the row is cut, and the
/// mark is drawn first precisely so that the cut never takes it away.
pub fn working_line(label: &str, tick: usize, colour: bool, width: usize) -> String {
    let mark = WAITING[tick % WAITING.len()];
    let coloured = format!("{CLEAR}  {ACCENT}{mark}{RESET} {DIM}{label}{RESET}");
    if colour && visible_columns(&coloured) <= width {
        return coloured;
    }
    cut(format!("{CLEAR}  {mark} {label}"), width)
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

    #[test]
    fn a_second_frame_rewrites_the_row_it_stands_on() {
        // The painter is the only thing that knows where the frame is. A second writer
        // assuming the cursor sat on the bar row is what drew a duplicate bar under it, so
        // a redraw has to move up over the frame rather than on to the next row.
        let paint = Paint::default();
        let mut out: Vec<u8> = Vec::new();
        paint.draw(&mut out, &["first".to_string()]).expect("drawn");
        out.clear();
        paint
            .draw(&mut out, &["second".to_string()])
            .expect("drawn");
        let written = String::from_utf8_lossy(&out);
        assert!(
            written.starts_with("\x1b[1F"),
            "moves back over the row it drew: {written:?}"
        );
    }

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
    fn the_waiting_line_names_the_work_and_keeps_moving() {
        let first = working_line("reading a 9.8 MB PDF", 0, false, 60);
        let second = working_line("reading a 9.8 MB PDF", 1, false, 60);
        assert!(
            first.contains("reading a 9.8 MB PDF"),
            "the line says what is being waited for: {first:?}"
        );
        assert!(
            first.contains(WAITING[0]) && second.contains(WAITING[1]),
            "the mark advances, so a slow parse is visibly alive: {first:?} / {second:?}"
        );
        assert!(visible_columns(&first) <= 60, "{first:?}");

        // A label longer than the row is cut, and the cut never takes the mark: a spinner
        // that scrolls off the end of a long path is a line that looks stuck.
        let long = working_line(&"x".repeat(200), 0, false, 40);
        assert!(visible_columns(&long) <= 40, "{long:?}");
        assert!(
            long.contains(WAITING[0]),
            "the mark survives the cut: {long:?}"
        );

        // The coloured form is only used when it fits whole: cutting it by characters would
        // slice an escape sequence in half.
        let coloured = working_line("reading a 9.8 MB PDF", 0, true, 60);
        assert!(
            coloured.contains(ACCENT) && coloured.contains("reading"),
            "{coloured:?}"
        );
        assert!(visible_columns(&coloured) <= 60, "{coloured:?}");
        let tight = working_line("reading a 9.8 MB PDF", 0, true, 12);
        assert!(
            !tight.contains(ACCENT),
            "no half-escape on a narrow row: {tight:?}"
        );
        assert!(visible_columns(&tight) <= 12, "{tight:?}");
    }

    #[test]
    fn the_meter_has_edges_and_fills_in_proportion() {
        // Nothing read: an empty bracketed bar, not a line of stray blocks.
        assert_eq!(
            meter(0, 80),
            format!("[{}]", EMPTY_CELL.to_string().repeat(20))
        );
        // Half: half the cells, still bracketed.
        let half = meter(50, 80);
        assert!(half.starts_with('[') && half.ends_with(']'), "{half:?}");
        assert_eq!(half.chars().count(), 22, "twenty cells and two brackets");
        assert_eq!(
            half.matches('█').count(),
            10,
            "fifty per cent is ten whole cells: {half:?}"
        );
        // The ends.
        assert_eq!(meter(100, 80).matches('█').count(), 20);
        assert_eq!(meter(100, 80).matches(EMPTY_CELL).count(), 0);
        // A book barely started still shows that it started: one eighth of a cell.
        let start = meter(1, 80);
        assert_eq!(
            start.matches('█').count(),
            0,
            "one per cent is not a whole cell: {start:?}"
        );
        assert!(start.contains('▏'), "but it is not zero either: {start:?}");
        // A narrow room shrinks the bar rather than wrapping it.
        assert_eq!(
            meter(50, 10).chars().count(),
            10,
            "eight cells, two brackets"
        );
        assert_eq!(meter(50, 2), "", "no room, no bar");
    }
}
