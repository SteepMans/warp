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

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::time::Duration;

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

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            204 => "No Content",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
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

/// The request line and the headers, as far as the answer needs them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub origin: Option<String>,
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
    let origin = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("origin"))
        .map(|(_, value)| value.trim().to_owned());
    Some(Request {
        method,
        path,
        origin,
    })
}

/// The answer to one request. Pages of other origins get nothing: without the CORS headers the
/// browser hides even the status from them.
pub fn respond(request: &Request, presence: Option<&Presence>) -> Response {
    if request.path != PATH {
        return Response::plain(404);
    }
    let Some(presence) = presence else {
        return Response::plain(404);
    };
    if request.origin.as_deref() != Some(presence.origin.as_str()) {
        return Response::plain(403);
    }
    let mut headers = vec![
        ("Access-Control-Allow-Origin", presence.origin.clone()),
        ("Vary", "Origin".to_owned()),
    ];
    match request.method.as_str() {
        "OPTIONS" => {
            headers.push(("Access-Control-Allow-Methods", "GET".to_owned()));
            headers.push(("Access-Control-Allow-Private-Network", "true".to_owned()));
            headers.push(("Access-Control-Max-Age", "600".to_owned()));
            Response {
                status: 204,
                headers,
                body: String::new(),
            }
        }
        "GET" => {
            headers.push(("Content-Type", "application/json".to_owned()));
            headers.push(("Cache-Control", "no-store".to_owned()));
            Response {
                status: 200,
                headers,
                body: serde_json::json!({ "worker": presence.worker }).to_string(),
            }
        }
        _ => Response::plain(405),
    }
}

fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST {
            return None;
        }
    }
    String::from_utf8(buffer).ok()
}

fn serve(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let response = match read_head(&mut stream).as_deref().and_then(parse) {
        Some(request) => {
            let presence = shared.read().ok().and_then(|presence| presence.clone());
            respond(&request, presence.as_ref())
        }
        None => Response::plain(400),
    };
    let _ = stream.write_all(&response.to_bytes());
}

/// Answers on `listener` until Ward quits, one connection after another: the card asks once
/// per click, so there is nothing to serve in parallel.
pub fn run(listener: TcpListener, shared: Shared) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve(stream, &shared),
            Err(err) => log::debug!("Ward presence: accept failed: {err}"),
        }
    }
}

/// Starts the answer on its own thread. A second Ward on the machine finds the port taken and
/// leaves the answer to the first one.
pub fn start(shared: Shared) {
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
        .spawn(move || run(listener, shared));
    if let Err(err) = spawned {
        log::warn!("Ward presence: no thread: {err}");
    }
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
