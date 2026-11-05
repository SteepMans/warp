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
//!   text frames are JSON: first `{"type":"hello","version":…,"features":[…]}`
//!   ([`hello_message`]), then `{"type":"size","cols":…,"rows":…}` when the size of the tab is
//!   known or changes, `{"type":"ping"}` to keep proxies from closing a quiet socket.
//! - The viewers to Ward: binary frames are keystrokes, written into the tab as if typed there.
//!   Text frames are JSON ([`ViewerFrame`]); any other text is ignored.
//!
//! A screen is not a single moment of output, so a viewer who joins later would see nothing
//! until the agent draws again. Ward therefore keeps the recent output of every session's tab
//! ([`Tap`]) from the moment it knows the tab, and the stream starts with it.
//!
//! The raw output only fits a screen of the same size: a TUI moves the cursor relative to where
//! it is and wraps at its own width, so a viewer of another size sees the input box repeated
//! and lines falling apart. The browser viewer takes the size of the tab itself. The viewer in
//! Ward on another machine ([`super::attach`]) cannot, it is a real terminal: it sends its own
//! size instead (`{"type":"size",…}` when it connects and whenever its tab is resized), and the
//! tab here takes that size for as long as the stream lasts (the last viewer to send one wins;
//! the service passes on only the latest). `{"type":"release"}` from the service, or the end of
//! the stream, gives the tab back the size of its pane ([`Sizing`]). After the change the agent
//! and the console redraw the screen at the new size, and the new size goes to the viewers at
//! once.
//!
//! A viewer is given the whole screen even when the tab has its size already and the recent
//! output is empty or starts in the middle of the screen: the tab then takes a row less for a
//! moment and the size of the viewer after ([`Resize::steps`]), so the console and the agent
//! draw again. The hello tells a viewer that this Ward takes its size (`size`) and redraws
//! (`redraw`); a viewer that gets no hello is streamed by an older Ward and says so.

use std::collections::VecDeque;
use std::sync::Arc;

use serde::Deserialize;

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

/// What the tab does for a viewer, as the hello says: it takes the size of the viewer's screen
/// (`size`) and draws the screen again even at a size it has already (`redraw`).
pub const FEATURES: [&str; 2] = ["size", "redraw"];

/// The first frame of a stream: which Ward streams and what it does for a viewer.
pub fn hello_message(version: &str) -> String {
    serde_json::json!({ "type": "hello", "version": version, "features": FEATURES }).to_string()
}

/// The biggest screen a viewer may give the tab; a bigger one is not a terminal.
pub const MAX_COLS: usize = 1000;
pub const MAX_ROWS: usize = 500;

/// What a viewer asks of the tab in a text frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerFrame {
    /// `{"type":"size","cols":…,"rows":…}`: the screen of a viewer in Ward on another machine.
    Size { cols: usize, rows: usize },
    /// `{"type":"release"}`: no viewer asks for a size any more.
    Release,
}

#[derive(Deserialize)]
struct RawViewerFrame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    cols: Option<u64>,
    #[serde(default)]
    rows: Option<u64>,
}

/// Reads a text frame of the viewers; `None` for anything that is not one of [`ViewerFrame`].
pub fn parse_viewer_frame(text: &str) -> Option<ViewerFrame> {
    let frame: RawViewerFrame = serde_json::from_str(text).ok()?;
    match frame.kind.as_str() {
        "size" => {
            let cols = usize::try_from(frame.cols?).ok()?;
            let rows = usize::try_from(frame.rows?).ok()?;
            ((1..=MAX_COLS).contains(&cols) && (1..=MAX_ROWS).contains(&rows))
                .then_some(ViewerFrame::Size { cols, rows })
        }
        "release" => Some(ViewerFrame::Release),
        _ => None,
    }
}

/// What a stream does to the PTY of its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resize {
    /// The size of a viewer: columns and rows.
    To { cols: usize, rows: usize },
    /// Back to the size of the pane.
    Back,
}

/// How long the tab keeps the other size of a redraw, see [`Resize::steps`].
pub const REDRAW_PAUSE: std::time::Duration = std::time::Duration::from_millis(300);

impl Resize {
    /// Columns and rows to give the tab; `None` for the size of its pane.
    pub fn size(self) -> Option<(usize, usize)> {
        match self {
            Resize::To { cols, rows } => Some((cols, rows)),
            Resize::Back => None,
        }
    }

    /// The resizes of the PTY that carry this one out while the tab is `current`, in order and
    /// [`REDRAW_PAUSE`] apart. A size the tab has already changes nothing in the PTY, so neither
    /// the console nor the agent would draw the screen again for the viewer who asked: the tab
    /// takes a row less (a row more at a single row) first.
    pub fn steps(self, current: Option<(usize, usize)>) -> Vec<Resize> {
        match self {
            Resize::To { cols, rows } if current == Some((cols, rows)) => {
                let other = if rows > 1 { rows - 1 } else { rows + 1 };
                vec![Resize::To { cols, rows: other }, self]
            }
            _ => vec![self],
        }
    }
}

/// The size the viewers gave the tab during one stream, so that the tab gets the size of its
/// pane back once they are gone.
#[derive(Debug, Default)]
pub struct Sizing {
    given: Option<(usize, usize)>,
}

impl Sizing {
    /// A frame of the viewers: the size to give the tab, if any. A size sent again is applied
    /// again, so the viewer who sent it last wins.
    pub fn on_frame(&mut self, frame: ViewerFrame) -> Option<Resize> {
        match frame {
            ViewerFrame::Size { cols, rows } => {
                self.given = Some((cols, rows));
                Some(Resize::To { cols, rows })
            }
            ViewerFrame::Release => self.given.take().map(|_| Resize::Back),
        }
    }

    /// The stream is over: the tab goes back to the size of its pane if a viewer changed it.
    pub fn on_end(&mut self) -> Option<Resize> {
        self.given.take().map(|_| Resize::Back)
    }

    /// The size a viewer gave the tab, while it holds.
    #[cfg(test)]
    pub fn given(&self) -> Option<(usize, usize)> {
        self.given
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
