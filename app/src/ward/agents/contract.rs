//! JSON shapes of the worker API of `agents-service` (`/api/v1/worker/...`), field for field as
//! in the contract note «Agents — контракт». Responses are parsed leniently: unknown fields are
//! ignored and everything Ward can live without has a default, so the service can grow fields
//! without breaking older builds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Card the session works on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRef {
    #[serde(default)]
    pub board_id: u64,
    #[serde(default)]
    pub task_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_number: Option<u64>,
}

/// Toolchains found in `PATH`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tools {
    pub git: bool,
    pub claude: bool,
    pub codex: bool,
    pub node: bool,
    pub java: bool,
    pub cargo: bool,
    pub dotnet: bool,
}

/// One coding agent on the machine. Found without network calls and without reading token
/// values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub logged_in: bool,
    /// `max`, `pro`, `plus`, `api` or whatever the agent reports; `None` when unknown.
    pub plan: Option<String>,
}

/// What the machine is, sent at registration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Passport {
    pub cpu_cores: u64,
    pub ram_total_bytes: u64,
    pub disk_total_bytes: u64,
    pub tools: Tools,
    /// By provider id (`claude`, `codex`).
    pub agents: BTreeMap<String, AgentInfo>,
    /// `owner/repo` of the clones already in the repository root.
    pub repos: Vec<String>,
}

/// Current load, sent with every heartbeat.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    pub cpu_percent: f64,
    pub ram_free_bytes: u64,
    pub disk_free_bytes: u64,
}

/// One subscription limit window of one provider, sent with every heartbeat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub provider: String,
    /// `5h` or `week` for claude, `primary` or `secondary` for codex.
    pub window: String,
    pub window_minutes: Option<i64>,
    /// `0..=100`, `None` when it could not be found out.
    pub used_percent: Option<f64>,
    /// RFC 3339.
    pub resets_at: Option<String>,
    /// When the numbers were true (RFC 3339).
    pub observed_at: Option<String>,
    /// Where the numbers come from, e.g. `oauth_usage` or `rollout`.
    pub source: String,
}

/// `POST /register`. The key is a secret: the type has no `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub key: String,
    pub name: String,
    pub os: String,
    pub arch: String,
    pub ward_version: String,
    pub passport: Passport,
}

/// Limits the owner set for this worker on the Agents screen. Empty strings mean no limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    pub max_sessions: u32,
    pub cpu_percent: f64,
    pub ram_reserve_gb: f64,
    pub disk_min_free_gb: f64,
    pub active_hours: String,
    pub allowed_repos: String,
    /// Allowed providers, most preferred first, comma separated.
    pub providers: String,
    pub usage_max_percent: f64,
    pub paused: bool,
}

/// The defaults of a new worker, from the contract.
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_sessions: 2,
            cpu_percent: 80.0,
            ram_reserve_gb: 4.0,
            disk_min_free_gb: 20.0,
            active_hours: String::new(),
            allowed_repos: String::new(),
            providers: "claude,codex".to_owned(),
            usage_max_percent: 90.0,
            paused: false,
        }
    }
}

/// Heartbeat interval when the service does not say.
pub const DEFAULT_HEARTBEAT_SECS: u64 = 15;

fn default_heartbeat_secs() -> u64 {
    DEFAULT_HEARTBEAT_SECS
}

/// Response of `POST /register`. The token is a secret: `Debug` leaves it out.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub worker_id: String,
    pub token: String,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default = "default_heartbeat_secs")]
    pub heartbeat_sec: u64,
}

impl std::fmt::Debug for RegisterResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterResponse")
            .field("worker_id", &self.worker_id)
            .field("token", &"<redacted>")
            .field("limits", &self.limits)
            .field("heartbeat_sec", &self.heartbeat_sec)
            .finish()
    }
}

/// Status of a session as the contract names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Queued,
    Assigned,
    Running,
    Blocked,
    Review,
    Failed,
    Cancelled,
    Lost,
    Complete,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Assigned => "assigned",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Review => "review",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Lost => "lost",
            Self::Complete => "complete",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeat {
    pub session_id: String,
    pub status: SessionStatus,
}

/// `POST /heartbeat`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {
    pub resources: Resources,
    pub usage: Vec<UsageReport>,
    pub sessions: Vec<SessionBeat>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HeartbeatResponse {
    pub limits: Option<Limits>,
    pub paused: bool,
    #[serde(deserialize_with = "lenient_list")]
    pub commands: Vec<Command>,
}

/// One task for this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Assignment {
    pub session_id: String,
    pub task_ref: TaskRef,
    pub attempt: u32,
    pub title: String,
    pub repo: String,
    pub base_branch: String,
    pub branch: String,
    pub prompt: String,
    /// `claude`, `codex` or `auto`; empty in assignments from before providers existed.
    pub provider: String,
    /// `None`: the agent's default model, no model flag.
    pub model: Option<String>,
    pub max_duration_min: u64,
}

/// An instruction for a running session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Command {
    Stop {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    Message {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        text: String,
    },
}

/// `200` body of `GET /inbox`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Inbox {
    #[serde(deserialize_with = "lenient_list")]
    pub assignments: Vec<Assignment>,
    #[serde(deserialize_with = "lenient_list")]
    pub commands: Vec<Command>,
}

/// A list where broken or unknown items (a command type from a newer service) are skipped
/// without losing the rest.
fn lenient_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?.unwrap_or_default();
    Ok(values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

/// `POST /assignments/{sessionId}/ack`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AckRequest {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl AckRequest {
    pub fn accept() -> Self {
        Self {
            accepted: true,
            reason: None,
        }
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            accepted: false,
            reason: Some(reason.into()),
        }
    }
}

/// `reason` of a `blocked` status after the agent hit its subscription limit.
pub const REASON_USAGE_LIMIT: &str = "usage_limit";

/// `POST /sessions/{sessionId}/status`. Every field is sent, unknown ones as `null`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusRequest {
    pub status: SessionStatus,
    pub provider: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub agent_session_id: Option<String>,
    /// The same as `agent_session_id` for claude, `None` for other providers.
    pub claude_session_id: Option<String>,
    pub remote_control_url: Option<String>,
    pub pr_url: Option<String>,
    pub message: Option<String>,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    /// RFC 3339.
    pub resets_at: Option<String>,
}

impl StatusRequest {
    pub fn new(status: SessionStatus) -> Self {
        Self {
            status,
            provider: None,
            branch: None,
            worktree: None,
            agent_session_id: None,
            claude_session_id: None,
            remote_control_url: None,
            pr_url: None,
            message: None,
            exit_code: None,
            reason: None,
            resets_at: None,
        }
    }
}

pub fn parse_register(json: &str) -> Result<RegisterResponse, serde_json::Error> {
    serde_json::from_str(json)
}

pub fn parse_heartbeat(json: &str) -> Result<HeartbeatResponse, serde_json::Error> {
    if json.trim().is_empty() {
        return Ok(HeartbeatResponse::default());
    }
    serde_json::from_str(json)
}

pub fn parse_inbox(json: &str) -> Result<Inbox, serde_json::Error> {
    if json.trim().is_empty() {
        return Ok(Inbox::default());
    }
    serde_json::from_str(json)
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
