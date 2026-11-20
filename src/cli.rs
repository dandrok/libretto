//! The command line surface. Types only — behaviour lives in `main.rs`, so the
//! shape of the interface can be read in one screen.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "libretto",
    version,
    about = "Read books aloud from the terminal, from where you stopped",
    long_about = None,
)]
pub struct Cli {
    /// No wordmark, no colours, no live line — for scripts, logs and CI
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Wrap the read-along text at this column (default: $COLUMNS, else 80)
    #[arg(long, global = true)]
    pub width: Option<usize>,

    /// Shift the underline by this many milliseconds: positive makes it lag the voice,
    /// negative makes it lead (`--sync-offset=-80`). For an audio pipeline whose buffer is
    /// longer than the rest of the error budget.
    #[arg(long, global = true, allow_hyphen_values = true, value_name = "MS")]
    pub sync_offset: Option<i64>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Add a book to the library (EPUB, or a PDF with a text layer)
    Add {
        /// Path to a DRM-free .epub, or a .pdf whose text can be extracted
        path: PathBuf,
    },
    /// List the library with reading progress
    Ls,
    /// Remove a book from the library; the EPUB file itself is untouched
    Rm {
        /// Full id or any unique prefix
        id: String,
    },
    /// Re-index a book whose file changed since it was added, keeping the position
    Refresh {
        /// Full id or any unique prefix
        id: String,
    },
    /// Check that the library, config, player, TTS endpoint and cache are healthy
    Doctor {
        /// TTS base URL to probe, e.g. http://127.0.0.1:8880/v1
        #[arg(long)]
        endpoint: Option<String>,
        /// Player program to look for (default mpv)
        #[arg(long)]
        player: Option<String>,
    },
    /// Read a book aloud from where you stopped (default: the last book)
    Read(BookArgs),
    /// Read aloud with the text on screen and the current word underlined
    Follow(BookArgs),
}

/// What `read` and `follow` both need: which book, where to start, how it sounds. The
/// two commands differ only in presentation, so they share one argument block.
#[derive(Debug, clap::Args)]
pub struct BookArgs {
    /// Full id or any unique prefix
    pub id: Option<String>,
    /// Start at this chapter (1-based), ignoring the stored position
    #[arg(long)]
    pub chapter: Option<usize>,
    /// Start this many sentences after the stored position
    #[arg(long, default_value_t = 0)]
    pub skip: usize,
    /// Replay this many sentences before the stored position
    #[arg(long, default_value_t = 0)]
    pub rewind: usize,
    /// Stop after this many sentences
    #[arg(long)]
    pub sentences: Option<usize>,
    /// Voice name, e.g. af_heart
    #[arg(long)]
    pub voice: Option<String>,
    /// Speech speed multiplier, 0.25 to 4.0
    #[arg(long)]
    pub speed: Option<f32>,
    /// TTS base URL, e.g. http://127.0.0.1:8880/v1
    #[arg(long)]
    pub endpoint: Option<String>,
    /// Model name sent to the endpoint
    #[arg(long)]
    pub model: Option<String>,
    /// Player program (default mpv)
    #[arg(long)]
    pub player: Option<String>,
    /// Sentences to synthesize ahead
    #[arg(long)]
    pub prefetch: Option<usize>,
}
