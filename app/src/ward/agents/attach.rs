//! "Подключиться" in the card at a computer that is not the session's: the session's tab, which
//! the Ward of its own machine streams ([`super::stream`]), opens here as a tab of this Ward.
//!
//! The page of the card knows the session and can get a viewer ticket from the service (the
//! browser viewer uses the same one); this Ward knows neither. So the page asks this Ward on the
//! loopback answer of [`super::presence`]: `POST http://127.0.0.1:{PORT}/ward/attach` with
//! `{"sessionId":…,"ticket":…,"title":…,"worker":…}`. Only the origin of the Chimney this worker
//! is connected to is heard, as for the presence answer. The page never names a host: the
//! viewer socket is built from this worker's own service address, so a page cannot point Ward
//! anywhere else.
//!
//! The tab runs a small viewer ([`SCRIPT`], Windows PowerShell with a few lines of C#): it
//! connects to the viewer socket, prints the raw output of the session's tab as it comes and
//! sends the keys typed here as binary frames, like the viewer in the browser. The address with
//! the ticket goes to it in an environment variable, not on the command line, and the ticket is
//! good for one connection within a minute. Ctrl+] disconnects; the session goes on. What the
//! output asks of the terminal of the session's machine is not for this one: queries (the
//! answers would go to the session as keys), the marks of Warp's shell and agent integration,
//! titles and keyboard modes are left out of what is printed.
//!
//! The viewer also sends the size of its tab, and the session's tab takes it, so the raw output
//! fits this screen ([`super::stream`]); until it does, the viewer holds the output back. The
//! tab itself is marked as remote control of that machine ([`super::remote_tab`]).

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const PATH: &str = "/ward/attach";

/// The viewer socket with the ticket, for the viewer in the tab.
pub const URL_ENV_VAR: &str = "WARD_ATTACH_URL";
/// The task, for the messages of the viewer.
pub const TITLE_ENV_VAR: &str = "WARD_ATTACH_TITLE";
/// The machine of the session, for the messages of the viewer.
pub const WORKER_ENV_VAR: &str = "WARD_ATTACH_WORKER";

/// The viewer run in the tab, see the module docs.
pub const SCRIPT: &str = include_str!("attach.ps1");
const SCRIPT_FILE_NAME: &str = "ward-attach.ps1";

/// Only Windows has the viewer; elsewhere the card keeps the terminal in the browser.
pub const SUPPORTED: bool = cfg!(windows);

const MAX_TITLE_CHARS: usize = 120;
const MAX_WORKER_CHARS: usize = 64;

/// A tab to open: the viewer socket with its ticket, the name of the tab, the task and the
/// machine of the session (empty when the page did not say).
#[derive(Clone, PartialEq, Eq)]
pub struct Attach {
    pub url: String,
    pub title: String,
    pub task: String,
    pub worker: String,
}

/// The ticket is a secret for a minute: it is not printed even in debug output.
impl std::fmt::Debug for Attach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attach")
            .field("title", &self.title)
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Body {
    session_id: String,
    ticket: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    worker: Option<String>,
}

/// Why a request to connect was turned down; goes back to the page as is.
#[derive(Debug, PartialEq, Eq)]
pub enum Rejected {
    NotJson,
    BadSession,
    BadTicket,
    NoAddress,
}

impl Rejected {
    pub fn message(&self) -> &'static str {
        match self {
            Rejected::NotJson => "the request is not a session to connect to",
            Rejected::BadSession => "the session id is not a UUID",
            Rejected::BadTicket => "the ticket is not one of the service",
            Rejected::NoAddress => "the service address of this worker has no stream socket",
        }
    }
}

/// Reads the request of the page and builds the tab for the service at `base_url`.
pub fn parse(body: &str, base_url: &str) -> Result<Attach, Rejected> {
    let body: Body = serde_json::from_str(body).map_err(|_| Rejected::NotJson)?;
    let session_id = body.session_id.trim();
    if !is_session_id(session_id) {
        return Err(Rejected::BadSession);
    }
    let ticket = body.ticket.trim();
    if !is_ticket(ticket) {
        return Err(Rejected::BadTicket);
    }
    let url = viewer_url(base_url, session_id, ticket).ok_or(Rejected::NoAddress)?;
    Ok(Attach {
        url,
        title: tab_title(body.title.as_deref(), body.worker.as_deref()),
        task: task_title(body.title.as_deref()),
        worker: clean(body.worker.as_deref(), MAX_WORKER_CHARS),
    })
}

/// A UUID in its usual form: 36 characters of hex digits and dashes.
fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(at, c)| match at {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The tickets of the service are URL-safe base64 of random bytes.
fn is_ticket(ticket: &str) -> bool {
    (16..=256).contains(&ticket.len())
        && ticket
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// `wss://…/api/v1/stream/{sessionId}?ticket=…` for the service address of the worker.
pub fn viewer_url(base_url: &str, session_id: &str, ticket: &str) -> Option<String> {
    let mut url = url::Url::parse(base_url).ok()?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    let path = format!(
        "{}/api/v1/stream/{session_id}",
        url.path().trim_end_matches('/')
    );
    url.set_path(&path);
    url.set_fragment(None);
    url.query_pairs_mut().clear().append_pair("ticket", ticket);
    Some(url.to_string())
}

/// One line of plain text, at most `max` characters.
fn clean(text: Option<&str>, max: usize) -> String {
    let text: String = text
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().take(max).collect()
}

/// The task of the session as one short line.
pub fn task_title(title: Option<&str>) -> String {
    let title = clean(title, MAX_TITLE_CHARS);
    if title.is_empty() {
        "Сессия агента".to_owned()
    } else {
        title
    }
}

/// The name of the tab: the machine the session runs on first, so that a narrow tab still
/// shows it, then the task.
pub fn tab_title(title: Option<&str>, worker: Option<&str>) -> String {
    let title = task_title(title);
    let worker = clean(worker, MAX_WORKER_CHARS);
    if worker.is_empty() {
        format!("Удалённо · {title}")
    } else {
        format!("{worker} · {title}")
    }
}

/// Where the viewer is written before a tab runs it.
pub fn script_path() -> PathBuf {
    warp_core::paths::state_dir().join(SCRIPT_FILE_NAME)
}

/// The file Windows PowerShell reads: without a byte order mark it takes UTF-8 for the ANSI
/// code page and spoils the Russian text.
pub fn script_bytes() -> Vec<u8> {
    let mut bytes = "\u{feff}".as_bytes().to_vec();
    bytes.extend_from_slice(SCRIPT.as_bytes());
    bytes
}

/// Writes the viewer of this Ward version, unless the file holds it already.
pub fn write_script(path: &Path) -> std::io::Result<()> {
    let bytes = script_bytes();
    if std::fs::read(path).is_ok_and(|kept| kept == bytes) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

/// The command typed into the tab: Windows PowerShell is there on every Windows.
pub fn tab_command(script: &Path) -> String {
    format!(
        "powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        script.display()
    )
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod tests;
