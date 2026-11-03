//! A fake OpenAI-compatible speech endpoint.
//!
//! Tests must not need a real Kokoro: this counts requests, records the last
//! request body, and answers with canned bytes — which is the only way "the
//! second run issues zero requests" becomes an assertion instead of a hope.
//!
//! It is a real HTTP/1.1 server in the two ways that matter here:
//!
//! - **Several requests per connection.** ureq pools sockets, so a server that
//!   answers once and closes makes the client race it into `Broken pipe`.
//! - **No timing assumption between requests.** The client writes each mp3 to
//!   disk with an fsync before asking for the next one, and on a loaded runner
//!   that gap is longer than any read timeout you would pick. So the socket is
//!   non-blocking, the idle allowance is generous, and shutdown comes from the
//!   stop flag rather than from a timeout firing.

#![allow(dead_code)]

use std::io::ErrorKind;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a connection may sit idle before the server gives up on it. The
/// client is allowed to be slow; it is not allowed to abandon a socket forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll interval for the "should I stop?" check while a socket is idle.
const POLL: Duration = Duration::from_millis(5);
/// A request body larger than this is not a sentence; refuse it.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub enum Reply {
    /// 200 with these bytes as `audio/mpeg`.
    Audio(Vec<u8>),
    /// 200 with an empty body: a server that "succeeds" without audio.
    Empty,
    /// An error status, to exercise the caller's handling.
    Status(u16),
}

pub struct FakeTts {
    base: String,
    hits: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    last_body: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeTts {
    pub fn start(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake tts");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let base = format!("http://{}", listener.local_addr().expect("local addr"));

        let hits = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(AtomicUsize::new(0));
        let last_body = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let counts = Arc::clone(&hits);
        let accepts = Arc::clone(&connections);
        let bodies = Arc::clone(&last_body);
        let stopping = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        accepts.fetch_add(1, Ordering::SeqCst);
                        let _ = serve(stream, &reply, &counts, &bodies, &stopping);
                    }
                    Err(_) => thread::sleep(POLL),
                }
            }
        });

        Self {
            base,
            hits,
            connections,
            last_body,
            stop,
            handle: Some(handle),
        }
    }

    /// Base URL, e.g. `http://127.0.0.1:41234` — the engine appends `/v1`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn endpoint(&self) -> String {
        format!("{}/v1", self.base)
    }

    /// Requests seen, across all connections.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Sockets accepted. Two requests on one connection is the difference between
    /// "the server kept the socket open" and "the client had to reconnect".
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// The JSON body of the most recent request, for asserting what was asked.
    pub fn last_body(&self) -> String {
        self.last_body
            .lock()
            .map(|guard| String::from_utf8_lossy(&guard).to_string())
            .unwrap_or_default()
    }
}

impl Drop for FakeTts {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Serve requests on one connection until the client leaves, the server is shut
/// down, or the socket idles out.
fn serve(
    mut stream: TcpStream,
    reply: &Reply,
    hits: &AtomicUsize,
    last_body: &Mutex<Vec<u8>>,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_nonblocking(true)?;
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut idle_since = Instant::now();

    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }

        match take_request(&mut buffer) {
            Ok(Some(body)) => {
                hits.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut guard) = last_body.lock() {
                    *guard = body;
                }
                if write_response(&mut stream, reply).is_err() {
                    return Ok(());
                }
                idle_since = Instant::now();
                continue;
            }
            Ok(None) => {}
            Err(()) => {
                // Oversized body: answer and hang up rather than buffer it.
                let _ = write_response(&mut stream, &Reply::Status(413));
                return Ok(());
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                idle_since = Instant::now();
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if idle_since.elapsed() > IDLE_TIMEOUT {
                    return Ok(());
                }
                thread::sleep(POLL);
            }
            Err(_) => return Ok(()),
        }
    }
}

/// Pull one complete request out of the buffer, leaving the next one in place.
///
/// `Ok(None)` means "need more bytes"; `Err(())` means the request is refuseable.
fn take_request(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, ()> {
    let Some(head_end) = find(buffer, b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_lowercase();
    let content_length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    if content_length > MAX_REQUEST_BYTES {
        return Err(());
    }

    let body_start = head_end + 4;
    if buffer.len() < body_start + content_length {
        return Ok(None);
    }
    let body = buffer[body_start..body_start + content_length].to_vec();
    buffer.drain(..body_start + content_length);
    Ok(Some(body))
}

/// A client that hits its own read limit closes the socket mid-body, so a write
/// error here is the expected outcome and must not fail the fake: the test is
/// about the client's ceiling, not about how the server dies.
fn write_response(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let (status, body): (String, &[u8]) = match reply {
        Reply::Audio(bytes) => ("200 OK".to_string(), bytes),
        Reply::Empty => ("200 OK".to_string(), b""),
        Reply::Status(code) => (format!("{code} Fake"), b"nope"),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    for piece in body.chunks(64 * 1024) {
        stream.write_all(piece)?;
    }
    stream.flush()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
