//! A fake OpenAI-compatible speech endpoint.
//!
//! Tests must not need a real Kokoro: this counts requests, records the last
//! request body, and answers with canned bytes — which is the only way "the
//! second run issues zero requests" becomes an assertion instead of a hope.
//!
//! It speaks HTTP/1.1 properly: one connection carries several requests, because
//! ureq pools connections. An earlier version answered one request per socket
//! and closed, and the client raced it into `Broken pipe` on a slower runner —
//! a defect in the fake that looked like a defect in the reader.

#![allow(dead_code)]

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How long a connection may sit idle before we treat the client as done.
const IDLE_TIMEOUT: Duration = Duration::from_millis(400);
/// A request body larger than this is not a sentence; refuse instead of buffering.
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
        let last_body = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let counts = Arc::clone(&hits);
        let bodies = Arc::clone(&last_body);
        let stopping = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve(stream, &reply, &counts, &bodies);
                    }
                    Err(_) => thread::sleep(Duration::from_millis(2)),
                }
            }
        });

        Self {
            base,
            hits,
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

/// Serve requests on one connection until the client goes away or idles out.
fn serve(
    mut stream: TcpStream,
    reply: &Reply,
    hits: &AtomicUsize,
    last_body: &Mutex<Vec<u8>>,
) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    let mut buffer: Vec<u8> = Vec::new();

    loop {
        // Headers: read until the blank line, or the client stops talking.
        let head_end = loop {
            if let Some(position) = find(&buffer, b"\r\n\r\n") {
                break position;
            }
            if !read_more(&mut stream, &mut buffer)? {
                return Ok(());
            }
        };

        let head = String::from_utf8_lossy(&buffer[..head_end]).to_lowercase();
        let content_length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0);
        let body_start = head_end + 4;

        if content_length > MAX_REQUEST_BYTES {
            write_response(&mut stream, &Reply::Status(413));
            return Ok(());
        }

        while buffer.len() < body_start + content_length {
            if !read_more(&mut stream, &mut buffer)? {
                return Ok(());
            }
        }

        let body = buffer[body_start..body_start + content_length].to_vec();
        buffer.drain(..body_start + content_length);

        hits.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = last_body.lock() {
            *guard = body;
        }

        // No `Connection: close`: the client is allowed to reuse this socket, and
        // we keep looping for its next request.
        write_response(&mut stream, reply);
    }
}

/// `true` while more bytes arrived; `false` on EOF or an idle timeout.
fn read_more(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> std::io::Result<bool> {
    let mut chunk = [0u8; 8192];
    match stream.read(&mut chunk) {
        Ok(0) => Ok(false),
        Ok(read) => {
            buffer.extend_from_slice(&chunk[..read]);
            Ok(true)
        }
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// A client that hits its own read limit closes the socket mid-body, so a write
/// error here is the expected outcome and must not fail the fake: the test is
/// about the client's ceiling, not about how the server dies.
fn write_response(stream: &mut TcpStream, reply: &Reply) {
    let (status, body): (String, &[u8]) = match reply {
        Reply::Audio(bytes) => ("200 OK".to_string(), bytes),
        Reply::Empty => ("200 OK".to_string(), b""),
        Reply::Status(code) => (format!("{code} Fake"), b"nope"),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    for chunk in body.chunks(64 * 1024) {
        if stream.write_all(chunk).is_err() {
            return;
        }
    }
    let _ = stream.flush();
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
