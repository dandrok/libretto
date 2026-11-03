//! A fake OpenAI-compatible speech endpoint.
//!
//! Tests must not need a real Kokoro: this counts requests, records the last
//! request body, and answers with canned bytes — which is the only way "the
//! second run issues zero requests" becomes an assertion instead of a hope.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// The JSON body the engine sent last, for asserting what was asked for.
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

fn serve(
    mut stream: TcpStream,
    reply: &Reply,
    hits: &AtomicUsize,
    last_body: &Mutex<Vec<u8>>,
) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut body_start: Option<usize> = None;
    let mut content_length = 0usize;

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if body_start.is_none()
                    && let Some(head_end) = find(&buffer, b"\r\n\r\n")
                {
                    body_start = Some(head_end + 4);
                    let head = String::from_utf8_lossy(&buffer[..head_end]).to_lowercase();
                    content_length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                }
                if let Some(start) = body_start
                    && buffer.len() >= start + content_length
                {
                    hits.fetch_add(1, Ordering::SeqCst);
                    if let Ok(mut guard) = last_body.lock() {
                        *guard = buffer[start..start + content_length].to_vec();
                    }
                    break;
                }
            }
            Err(_) => break,
        }
    }

    match reply {
        Reply::Audio(bytes) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(bytes)?;
        }
        Reply::Empty => {
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
        }
        Reply::Status(code) => {
            let body = b"nope";
            let head = format!(
                "HTTP/1.1 {code} Nope\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(body)?;
        }
    }
    stream.flush()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
