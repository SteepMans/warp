//! The tab of a session on the owner's screen elsewhere: Ward streams the terminal itself.
//!
//! The owner at another machine (a phone, another computer) asks the card for the session's
//! window. The service has no way into this machine, so Ward connects out: a WebSocket to
//! `{service}/api/v1/worker/stream/{sessionId}` with the worker token. The service relays it to
//! the viewers in the browser.
//!
//! What goes over the socket:
//! - Ward to the viewers: binary frames are the raw output of the tab, exactly what the
//!   terminal itself was given, so a terminal emulator in the browser draws the same screen;
//!   text frames are JSON: `{"type":"size","cols":…,"rows":…}` when the size of the tab is known
//!   or changes, `{"type":"ping"}` to keep proxies from closing a quiet socket.
//! - The viewers to Ward: binary frames are keystrokes, written into the tab as if typed there.
//!   Text frames are not commands and are ignored.
//!
//! A screen is not a single moment of output, so a viewer who joins later would see nothing
//! until the agent draws again. Ward therefore keeps the recent output of every session's tab
//! ([`Tap`]) from the moment it knows the tab, and the stream starts with it.

use std::collections::VecDeque;
use std::sync::Arc;

/// How much recent output of a tab is kept for a stream to start with.
pub const RING_BYTES: usize = 2 * 1024 * 1024;
/// The biggest binary frame Ward sends.
pub const CHUNK_BYTES: usize = 64 * 1024;
/// How often the size of a streamed tab is checked and a ping goes out.
pub const SIZE_CHECK: std::time::Duration = std::time::Duration::from_secs(2);
pub const PING_EVERY: u32 = 15;

/// The recent output of a tab, at most `cap` bytes.
#[derive(Debug)]
pub struct Ring {
    bytes: VecDeque<u8>,
    cap: usize,
    /// Older output was thrown away: the oldest kept line may be cut in the middle.
    trimmed: bool,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            cap,
            trimmed: false,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.bytes.extend(data.iter().copied());
        if self.bytes.len() > self.cap {
            let extra = self.bytes.len() - self.cap;
            self.bytes.drain(..extra);
            self.trimmed = true;
        }
    }

    /// What a new viewer is given first. When older output was thrown away, the stream starts
    /// at the next line: half an escape sequence at the very start would draw garbage.
    pub fn snapshot(&self) -> Vec<u8> {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        if !self.trimmed {
            return bytes;
        }
        match bytes.iter().position(|byte| *byte == b'\n') {
            Some(newline) => bytes[newline + 1..].to_vec(),
            None => Vec::new(),
        }
    }
}

/// The output of one tab: what was printed lately and who is watching now.
#[derive(Debug)]
pub struct Tap {
    ring: Ring,
    viewers: Vec<async_channel::Sender<Arc<Vec<u8>>>>,
}

impl Default for Tap {
    fn default() -> Self {
        Self::with_capacity(RING_BYTES)
    }
}

impl Tap {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            ring: Ring::new(cap),
            viewers: Vec::new(),
        }
    }

    /// One read of the tab's output. A stream that is over is forgotten here.
    pub fn feed(&mut self, data: Arc<Vec<u8>>) {
        self.ring.push(&data);
        self.viewers
            .retain(|viewer| viewer.try_send(data.clone()).is_ok());
    }

    /// A new stream: the recent output and everything printed from now on, with nothing lost
    /// or repeated between the two.
    pub fn subscribe(&mut self) -> (Vec<u8>, async_channel::Receiver<Arc<Vec<u8>>>) {
        let (sender, receiver) = async_channel::unbounded();
        self.viewers.push(sender);
        (self.ring.snapshot(), receiver)
    }

    pub fn viewers(&self) -> usize {
        self.viewers.len()
    }
}

pub type SharedTap = Arc<parking_lot::Mutex<Tap>>;

/// `wss://…/api/v1/worker/stream/{sessionId}` for the service address of the worker.
pub fn worker_url(base_url: &str, session_id: &str) -> Option<String> {
    let mut url = url::Url::parse(base_url).ok()?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    let path = format!(
        "{}/api/v1/worker/stream/{session_id}",
        url.path().trim_end_matches('/')
    );
    url.set_path(&path);
    url.set_query(None);
    Some(url.to_string())
}

pub fn size_message(cols: usize, rows: usize) -> String {
    serde_json::json!({ "type": "size", "cols": cols, "rows": rows }).to_string()
}

pub fn ping_message() -> String {
    serde_json::json!({ "type": "ping" }).to_string()
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
