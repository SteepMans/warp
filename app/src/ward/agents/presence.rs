//! "Is this the machine of the session?" for the Chimney page in the owner's browser.
//!
//! A browser does not know which computer it runs on, so the card cannot tell by itself
//! whether the session's worker is the machine in front of the owner. Ward answers that on a
//! fixed loopback port: `GET http://127.0.0.1:{PORT}/ward/presence` gives the name of this
//! worker. Only Ward on the same computer can answer there; from a phone or another computer
//! the request fails, and the card turns to remote control.
//!
//! Only the origin of the Chimney the worker is connected to gets an answer (CORS), together
//! with the private network preflight browsers send before a public page reaches loopback.
//! The answer carries the worker name and nothing else.
//!
//! The same port takes `POST /ward/attach` from that page: open here the tab of a session that
//! runs on another machine, see [`super::attach`].

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::attach;

/// The loopback port of the answer. The card asks it, so it must not change.
pub const PORT: u16 = 47615;
pub const PATH: &str = "/ward/presence";

/// A request bigger than this is not one of ours.
const MAX_REQUEST: usize = 8 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// What Ward answers with: the worker name and the origin of the page that may ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    pub worker: String,
    pub origin: String,
    /// The service address: the viewer socket of [`attach`] is built from it.
    pub base_url: String,
    /// Whether this Ward can open the tab of a session on another machine.
    pub can_attach: bool,
}

/// Kept up to date by the worker loop; `None` while the worker has no settings.
pub type Shared = Arc<RwLock<Option<Presence>>>;

/// `https://host[:port]` of the service address: the origin of the Chimney page.
pub fn origin_of(base_url: &str) -> Option<String> {
    let url = url::Url::parse(base_url).ok()?;
    let origin = url.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

pub fn presence_of(worker: &str, base_url: &str) -> Option<Presence> {
    let worker = worker.trim();
    if worker.is_empty() {
        return None;
    }
    Some(Presence {
        worker: worker.to_owned(),
        origin: origin_of(base_url)?,
        base_url: base_url.trim().to_owned(),
        can_attach: attach::SUPPORTED,
    })
}

/// One HTTP response, written by [`Response::to_bytes`].
#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

impl Response {
    fn plain(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    fn json(status: u16, mut headers: Vec<(&'static str, String)>, body: String) -> Self {
        headers.push(("Content-Type", "application/json".to_owned()));
        headers.push(("Cache-Control", "no-store".to_owned()));
        Self {
            status,
            headers,
            body,
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            202 => "Accepted",
            204 => "No Content",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            501 => "Not Implemented",
            503 => "Service Unavailable",
            _ => "Bad Request",
        };
        let mut out = format!("HTTP/1.1 {} {reason}\r\n", self.status);
        for (key, value) in &self.headers {
            out.push_str(&format!("{key}: {value}\r\n"));
        }
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.body.len()
        ));
        out.push_str(&self.body);
        out.into_bytes()
    }
}

/// The request line and the headers, as far as the answer needs them, and the body.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub origin: Option<String>,
    /// `Content-Length`; the body itself is read after the head.
    pub content_length: usize,
    pub body: String,
}

/// Reads the head of a request; `None` for anything that is not HTTP.
pub fn parse(head: &str) -> Option<Request> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let target = first.next()?;
    first
        .next()
        .filter(|version| version.starts_with("HTTP/"))?;
    let path = target.split('?').next().unwrap_or_default().to_owned();
    let headers: Vec<(&str, &str)> = lines.filter_map(|line| line.split_once(':')).collect();
    let header = |wanted: &str| {
        headers
            .iter()
            .find(|(name, _)| name.trim().eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.trim())
    };
    let origin = header("origin").map(str::to_owned);
    let content_length = header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    Some(Request {
        method,
        path,
        origin,
        content_length,
        body: String::new(),
    })
}

/// The answer to one request. Pages of other origins get nothing: without the CORS headers the
/// browser hides even the status from them.
pub fn respond(request: &Request, presence: Option<&Presence>) -> Response {
    handle(request, presence).0
}

/// The answer to one request and, for a request to connect, the tab to open.
pub fn handle(
    request: &Request,
    presence: Option<&Presence>,
) -> (Response, Option<attach::Attach>) {
    let attaching = request.path == attach::PATH;
    if request.path != PATH && !attaching {
        return (Response::plain(404), None);
    }
    let Some(presence) = presence else {
        return (Response::plain(404), None);
    };
    if request.origin.as_deref() != Some(presence.origin.as_str()) {
        return (Response::plain(403), None);
    }
    let mut headers = vec![
        ("Access-Control-Allow-Origin", presence.origin.clone()),
        ("Vary", "Origin".to_owned()),
    ];
    match (attaching, request.method.as_str()) {
        (_, "OPTIONS") => {
            let methods = if attaching { "POST" } else { "GET" };
            headers.push(("Access-Control-Allow-Methods", methods.to_owned()));
            if attaching {
                headers.push(("Access-Control-Allow-Headers", "Content-Type".to_owned()));
            }
            headers.push(("Access-Control-Allow-Private-Network", "true".to_owned()));
            headers.push(("Access-Control-Max-Age", "600".to_owned()));
            let response = Response {
                status: 204,
                headers,
                body: String::new(),
            };
            (response, None)
        }
        (false, "GET") => {
            let body = serde_json::json!({ "worker": presence.worker }).to_string();
            (Response::json(200, headers, body), None)
        }
        (true, "POST") => {
            if !presence.can_attach {
                let body = serde_json::json!({
                    "error": "this Ward cannot open the tab of a session on another machine"
                })
                .to_string();
                return (Response::json(501, headers, body), None);
            }
            match attach::parse(&request.body, &presence.base_url) {
                Ok(tab) => {
                    let body = serde_json::json!({ "ok": true }).to_string();
                    (Response::json(202, headers, body), Some(tab))
                }
                Err(rejected) => {
                    let body = serde_json::json!({ "error": rejected.message() }).to_string();
                    (Response::json(400, headers, body), None)
                }
            }
        }
        _ => (Response::plain(405), None),
    }
}

/// Reads one request: the head, then as much of the body as `Content-Length` says.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    const END: &[u8] = b"\r\n\r\n";
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(at) = buffer.windows(END.len()).position(|window| window == END) {
            break at + END.len();
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST {
            return None;
        }
    };
    let mut request = parse(std::str::from_utf8(&buffer[..head_end]).ok()?)?;
    if request.content_length > MAX_REQUEST {
        return None;
    }
    let mut body = buffer.split_off(head_end);
    while body.len() < request.content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(request.content_length);
    request.body = String::from_utf8(body).ok()?;
    Some(request)
}

fn serve(mut stream: TcpStream, shared: &Shared, attaches: &async_channel::Sender<attach::Attach>) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let response = match read_request(&mut stream) {
        Some(request) => {
            let presence = shared.read().ok().and_then(|presence| presence.clone());
            match handle(&request, presence.as_ref()) {
                (response, Some(tab)) => {
                    if attaches.try_send(tab).is_ok() {
                        response
                    } else {
                        Response::plain(503)
                    }
                }
                (response, None) => response,
            }
        }
        None => Response::plain(400),
    };
    let _ = stream.write_all(&response.to_bytes());
}

/// Answers on `listener` until Ward quits, one connection after another: the card asks once
/// per click, so there is nothing to serve in parallel. The tabs the card asks for go to
/// `attaches`.
pub fn run(listener: TcpListener, shared: Shared, attaches: async_channel::Sender<attach::Attach>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve(stream, &shared, &attaches),
            Err(err) => log::debug!("Ward presence: accept failed: {err}"),
        }
    }
}

/// Starts the answer on its own thread. A second Ward on the machine finds the port taken and
/// leaves the answer to the first one.
pub fn start(shared: Shared, attaches: async_channel::Sender<attach::Attach>) {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, PORT);
    let listener = match TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(err) => {
            log::info!("Ward presence: port {PORT} is not free ({err}); not answering");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("ward-presence".to_owned())
        .spawn(move || run(listener, shared, attaches));
    if let Err(err) = spawned {
        log::warn!("Ward presence: no thread: {err}");
    }
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
