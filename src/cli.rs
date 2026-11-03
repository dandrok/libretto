//! The command line surface. Types only — behaviour lives in `main.rs`, so the
//! shape of the interface can be read in one screen.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "libretto",
    version,
    about = "Read EPUB books aloud, from where you stopped",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Add an EPUB to the library
    Add {
        /// Path to a DRM-free .epub
        path: PathBuf,
    },
    /// List the library with reading progress
    Ls,
    /// Remove a book from the library; the EPUB file itself is untouched
    Rm {
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
    Read {
        /// Full id or any unique prefix
        id: Option<String>,
        /// Start at this chapter (1-based), ignoring the stored position
        #[arg(long)]
        chapter: Option<usize>,
        /// Start this many sentences after the stored position
        #[arg(long, default_value_t = 0)]
        skip: usize,
        /// Replay this many sentences before the stored position
        #[arg(long, default_value_t = 0)]
        rewind: usize,
        /// Stop after this many sentences
        #[arg(long)]
        sentences: Option<usize>,
        /// Voice name, e.g. af_heart
        #[arg(long)]
        voice: Option<String>,
        /// Speech speed multiplier, 0.25 to 4.0
        #[arg(long)]
        speed: Option<f32>,
        /// TTS base URL, e.g. http://127.0.0.1:8880/v1
        #[arg(long)]
        endpoint: Option<String>,
        /// Model name sent to the endpoint
        #[arg(long)]
        model: Option<String>,
        /// Player program (default mpv)
        #[arg(long)]
        player: Option<String>,
        /// Sentences to synthesize ahead
        #[arg(long)]
        prefetch: Option<usize>,
    },
}
