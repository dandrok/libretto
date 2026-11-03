//! libretto — add EPUB books to a local library and have them read aloud.
//!
//! The crate is a library first so every module can be tested without a
//! terminal, a network or an audio device. `main.rs` is a thin shell on top.

#![forbid(unsafe_code)]

pub mod cache;
pub mod epub;
pub mod paths;
pub mod store;
pub mod text;
pub mod tts;
