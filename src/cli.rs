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
}
