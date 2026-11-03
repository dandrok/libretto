//! `cargo run --example speak -- <endpoint> "<sentence>" [voice]`
//!
//! Synthesizes one sentence through the real engine and prints where it landed
//! in the cache. Run it twice: the second run prints `cache hit` and issues no
//! request at all, which is the whole point of the content-addressed cache.

use libretto::cache::Cache;
use libretto::tts::{Kokoro, SpeechRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let endpoint = args
        .next()
        .unwrap_or_else(|| String::from("http://127.0.0.1:8880/v1"));
    let text = args
        .next()
        .unwrap_or_else(|| String::from("Hello from libretto."));
    let voice = args.next();

    let engine = Kokoro::new(endpoint)?;
    let cache = Cache::open()?;
    let mut request = SpeechRequest::new(text);
    if let Some(voice) = voice {
        request = request.with_voice(voice);
    }

    let key = Cache::key(&request);
    let was_cached = cache.get(&key).is_some();
    let path = cache.audio_for(&engine, &request)?;
    let bytes = std::fs::metadata(&path)?.len();

    println!(
        "{}\n  {bytes} bytes · {}",
        path.display(),
        if was_cached {
            "cache hit, no request"
        } else {
            "synthesized"
        }
    );
    Ok(())
}
