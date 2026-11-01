//! The owner's view of every session, not only this machine's.
//!
//! The worker API ([`super::client`]) is about this Ward's own sessions and is signed with the
//! worker token. Reading the chat of a session that runs on another machine, and writing into
//! it, is the owner's business, so it goes the way the Agents screen of Chimney goes:
//! `/api/agents/...` of Chimney proxies the admin API of agents-service with the admin token
//! (which no worker has) and lets through only the owner. Ward signs those requests with the
//! Chimney credentials it already has — the access token or the service account, see
//! [`crate::ward::chimney::auth`] — so there is no second secret to configure.
//!
//! Three calls: the list of sessions, the journal of one session from a byte offset, and a
//! command for the worker that runs it. The journal is the very ndjson the workers ship
//! ([`super::journal`]), so it reads as a chat through [`super::provider::chat`]; the offset is
//! what keeps the chat a top-up instead of a reread of the whole transcript every few seconds.
//! A `message` command reaches the agent the same way it always has: the service hands it to
//! that worker, which types it into the tab of the session.

use std::time::{Duration, Instant};

use serde::Deserialize;

use super::contract::TaskRef;
use crate::ward::chimney::RequestConfig;
use crate::ward::chimney::auth::{Auth, HttpAuthTransport};
use crate::ward::chimney::model::FetchError;

/// Prefix of the gateway under the Chimney base URL.
const PREFIX: &str = "/api/agents";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A bigger answer is not a list of sessions Ward can use.
const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;

/// How much of a journal chunk is kept. Only the first read of a long session is ever this
/// big: afterwards the offset leaves only the tail to fetch.
const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// A journal bigger than this is not read at all: something is wrong with it, not long.
const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;

/// How many sessions the list asks for.
pub const LIST_LIMIT: u32 = 50;

/// One session as the list shows it: the fields of `sessionSummary` Ward reads, everything else
/// ignored and everything optional defaulted, so a newer service does not break the screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Summary {
    pub id: String,
    pub task_ref: TaskRef,
    pub title: Option<String>,
    /// `queued`, `running`, `blocked`, `review`, … as the contract names them.
    pub status: Option<String>,
    /// Name of the worker the session runs on; `None` while it has none.
    pub worker: Option<String>,
    pub provider: Option<String>,
    pub branch: Option<String>,
    pub repo: Option<String>,
    /// The last message of the worker about the session.
    pub message: Option<String>,
    pub created_at: Option<String>,
    pub finished_at: Option<String>,
}

impl Summary {
    /// `#<number> <title>`, as the panel and the chat header name a session.
    pub fn label(&self) -> String {
        let number = self
            .task_ref
            .task_number
            .or(Some(self.task_ref.task_id).filter(|id| *id > 0));
        let title = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or("без названия");
        match number {
            Some(number) => format!("#{number} {title}"),
            None => title.to_owned(),
        }
    }

    /// Whether a command still has a worker to go to: that is the one case the service takes a
    /// `message` in, and the screen says so instead of letting the owner type into nothing.
    pub fn is_live(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("queued" | "assigned" | "preparing" | "plan-review" | "running" | "blocked")
        )
    }
}

pub fn parse_sessions(json: &str) -> Result<Vec<Summary>, serde_json::Error> {
    serde_json::from_str(json)
}

/// A piece of a journal as the screen gets it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// Leading bytes of the chunk that were dropped because it was bigger than
    /// [`MAX_CHUNK_BYTES`]: the chat shows the end of a long transcript, and the offset has to
    /// count the beginning it never read.
    pub skipped: u64,
}

/// Keeps at most [`MAX_CHUNK_BYTES`] of the end of a chunk, cut at a line boundary, instead of
/// failing the read of a transcript that grew to tens of megabytes. A newline is one ASCII
/// byte, so the byte after it is always a character boundary.
fn cap(text: String) -> Chunk {
    if text.len() <= MAX_CHUNK_BYTES {
        return Chunk { text, skipped: 0 };
    }
    let from = text.len() - MAX_CHUNK_BYTES;
    let start = match text.as_bytes()[from..]
        .iter()
        .position(|byte| *byte == b'\n')
    {
        Some(at) => from + at + 1,
        None => text.len(),
    };
    Chunk {
        text: text[start..].to_owned(),
        skipped: start as u64,
    }
}

/// Whole lines of a journal chunk and how many bytes they take. A chunk ends wherever the
/// transcript happened to be written to, so the tail of a half line is left for the next
/// offset instead of being parsed as a broken entry.
pub fn whole_lines(chunk: &str) -> (&str, u64) {
    match chunk.rfind('\n') {
        Some(end) => (&chunk[..=end], end as u64 + 1),
        None => ("", 0),
    }
}

/// A session id as one URL path segment.
fn segment(value: &str) -> String {
    super::client::path_segment(value)
}

/// The sessions the service knows, newest first.
pub async fn sessions(
    client: &http_client::Client,
    auth: &Auth,
    config: &RequestConfig,
    limit: u32,
) -> Result<Vec<Summary>, FetchError> {
    let text = get(
        client,
        auth,
        config,
        &format!("/sessions?limit={limit}"),
        MAX_LIST_BYTES,
    )
    .await?;
    parse_sessions(&text).map_err(|_| FetchError::Parse)
}

/// The journal of a session from `offset` bytes on: only what was written after it.
pub async fn journal(
    client: &http_client::Client,
    auth: &Auth,
    config: &RequestConfig,
    session_id: &str,
    offset: u64,
) -> Result<Chunk, FetchError> {
    let path = format!("/sessions/{}/journal?offset={offset}", segment(session_id));
    let text = get(client, auth, config, &path, MAX_JOURNAL_BYTES).await?;
    Ok(cap(text))
}

/// Types `text` into the agent of the session, wherever it runs: the service queues the
/// command for that session's worker.
pub async fn send_message(
    client: &http_client::Client,
    auth: &Auth,
    config: &RequestConfig,
    session_id: &str,
    text: &str,
) -> Result<(), FetchError> {
    let body = serde_json::json!({ "type": "message", "text": text }).to_string();
    let path = format!("/sessions/{}/commands", segment(session_id));
    let transport = HttpAuthTransport {
        client,
        timeout: REQUEST_TIMEOUT,
    };
    auth.send_authorized(
        &config.base_url,
        &config.credentials,
        &transport,
        &Instant::now,
        |token| post(client, &config.base_url, token, &path, body.clone()),
    )
    .await
}

/// `GET {base}{PREFIX}{path}` with a token from [`Auth`], signing in or refreshing as needed.
async fn get(
    client: &http_client::Client,
    auth: &Auth,
    config: &RequestConfig,
    path: &str,
    max_bytes: usize,
) -> Result<String, FetchError> {
    let transport = HttpAuthTransport {
        client,
        timeout: REQUEST_TIMEOUT,
    };
    auth.send_authorized(
        &config.base_url,
        &config.credentials,
        &transport,
        &Instant::now,
        |token| read(client, &config.base_url, token, path, max_bytes),
    )
    .await
}

async fn read(
    client: &http_client::Client,
    base_url: &str,
    token: String,
    path: &str,
    max_bytes: usize,
) -> Result<String, FetchError> {
    let response = client
        .get(format!("{base_url}{PREFIX}{path}"))
        .bearer_auth(&token)
        .header("Accept", "application/json, application/x-ndjson")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|_| FetchError::Network)?;
    let status = response.status().as_u16();
    match status {
        200..=299 => {}
        401 | 403 => return Err(FetchError::Unauthorized),
        _ => return Err(FetchError::Status(status)),
    }
    let body = response.text().await.map_err(|_| FetchError::Network)?;
    if body.len() > max_bytes {
        return Err(FetchError::Parse);
    }
    Ok(body)
}

async fn post(
    client: &http_client::Client,
    base_url: &str,
    token: String,
    path: &str,
    body: String,
) -> Result<(), FetchError> {
    let response = client
        .post(format!("{base_url}{PREFIX}{path}"))
        .bearer_auth(&token)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .body(body)
        .send()
        .await
        .map_err(|_| FetchError::Network)?;
    let status = response.status().as_u16();
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(FetchError::Unauthorized),
        status => Err(FetchError::Status(status)),
    }
}

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;
