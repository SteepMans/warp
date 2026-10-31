//! HTTP calls of the worker API. The base URL is the service address from the settings
//! (`https://chimney.steep-man.ru/agents`); paths are `/api/v1/worker/...` below it. Requests
//! carry `Authorization: Bearer <worker token>`; neither the token nor the key is logged.

use std::time::Duration;

use super::contract::{
    self, AckRequest, HeartbeatRequest, HeartbeatResponse, Inbox, RegisterRequest,
    RegisterResponse, StatusRequest,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Extra time for a long poll on top of its `wait`.
const LONG_POLL_MARGIN: Duration = Duration::from_secs(15);

/// Largest response body Ward reads.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    /// 401: the token (or, for registration, the key) is not accepted.
    Unauthorized,
    /// 403 on registration: the key is revoked or not allowed.
    Forbidden,
    Status(u16),
    Network(String),
    Parse,
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            Self::Unauthorized => "the service rejected the credentials (HTTP 401)".to_owned(),
            Self::Forbidden => "the service refused access (HTTP 403)".to_owned(),
            Self::Status(status) => format!("HTTP {status}"),
            Self::Network(err) => format!("network error: {err}"),
            Self::Parse => "unexpected response".to_owned(),
        }
    }
}

/// Trims and validates the service address: `http(s)://host[/path]` without a trailing slash.
pub fn normalize_base_url(url: &str) -> Option<String> {
    crate::ward::chimney::normalize_base_url(url)
}

pub struct Api<'a> {
    pub client: &'a http_client::Client,
    pub base_url: &'a str,
}

impl Api<'_> {
    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/worker{path}", self.base_url)
    }

    async fn send(
        &self,
        request: http_client::RequestBuilder<'_>,
    ) -> Result<(u16, String), ApiError> {
        let response = request
            .send()
            .await
            .map_err(|err| ApiError::Network(err.without_url().to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|err| ApiError::Network(err.without_url().to_string()))?;
        match status {
            200..=299 => {
                if body.len() > MAX_RESPONSE_BYTES {
                    return Err(ApiError::Parse);
                }
                Ok((status, body))
            }
            401 => Err(ApiError::Unauthorized),
            403 => Err(ApiError::Forbidden),
            status => Err(ApiError::Status(status)),
        }
    }

    fn post_json(
        &self,
        path: &str,
        token: Option<&str>,
        body: String,
    ) -> http_client::RequestBuilder<'_> {
        let mut request = self
            .client
            .post(self.url(path))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .timeout(REQUEST_TIMEOUT)
            .body(body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request
    }

    pub async fn register(&self, request: &RegisterRequest) -> Result<RegisterResponse, ApiError> {
        let body = serde_json::to_string(request).map_err(|_| ApiError::Parse)?;
        let (_, text) = self.send(self.post_json("/register", None, body)).await?;
        contract::parse_register(&text).map_err(|_| ApiError::Parse)
    }

    pub async fn heartbeat(
        &self,
        token: &str,
        request: &HeartbeatRequest,
    ) -> Result<HeartbeatResponse, ApiError> {
        let body = serde_json::to_string(request).map_err(|_| ApiError::Parse)?;
        let (_, text) = self
            .send(self.post_json("/heartbeat", Some(token), body))
            .await?;
        contract::parse_heartbeat(&text).map_err(|_| ApiError::Parse)
    }

    /// Long poll: waits up to `wait_secs` for work; an empty inbox on `204`.
    pub async fn inbox(&self, token: &str, wait_secs: u64) -> Result<Inbox, ApiError> {
        let request = self
            .client
            .get(self.url(&format!("/inbox?wait={wait_secs}")))
            .header("Accept", "application/json")
            .bearer_auth(token)
            .timeout(Duration::from_secs(wait_secs) + LONG_POLL_MARGIN);
        let (status, text) = self.send(request).await?;
        if status == 204 {
            return Ok(Inbox::default());
        }
        contract::parse_inbox(&text).map_err(|_| ApiError::Parse)
    }

    pub async fn ack(
        &self,
        token: &str,
        session_id: &str,
        ack: &AckRequest,
    ) -> Result<(), ApiError> {
        let body = serde_json::to_string(ack).map_err(|_| ApiError::Parse)?;
        let path = format!("/assignments/{}/ack", path_segment(session_id));
        self.send(self.post_json(&path, Some(token), body))
            .await
            .map(|_| ())
    }

    pub async fn status(
        &self,
        token: &str,
        session_id: &str,
        status: &StatusRequest,
    ) -> Result<(), ApiError> {
        let body = serde_json::to_string(status).map_err(|_| ApiError::Parse)?;
        let path = format!("/sessions/{}/status", path_segment(session_id));
        self.send(self.post_json(&path, Some(token), body))
            .await
            .map(|_| ())
    }

    pub async fn journal(
        &self,
        token: &str,
        session_id: &str,
        seq: u64,
        bytes: Vec<u8>,
    ) -> Result<(), ApiError> {
        let request = self
            .client
            .post(self.url(&format!(
                "/sessions/{}/journal?seq={seq}",
                path_segment(session_id)
            )))
            .header("Content-Type", "application/x-ndjson")
            .bearer_auth(token)
            .timeout(REQUEST_TIMEOUT * 2)
            .body(bytes);
        self.send(request).await.map(|_| ())
    }
}

/// A session id as one URL path segment.
pub fn path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
