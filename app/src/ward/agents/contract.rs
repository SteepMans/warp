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
    /// No assignments until the profile is applied and the proxy check passes.
    pub require_proxy: bool,
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
            require_proxy: true,
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
    /// The profile version the service has now.
    #[serde(default)]
    pub profile_version: u32,
    /// The profile itself, so the first assignment needs no extra request.
    #[serde(default)]
    pub profile: Option<ProfileBody>,
}

impl std::fmt::Debug for RegisterResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterResponse")
            .field("worker_id", &self.worker_id)
            .field("token", &"<redacted>")
            .field("limits", &self.limits)
            .field("heartbeat_sec", &self.heartbeat_sec)
            .field("profile_version", &self.profile_version)
            // The parts carry the secrets the service substituted for this machine.
            .field("profile", &self.profile.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// `GET /profile` and the `profile` of `POST /register`: the machine profile with the secrets
/// of this worker already substituted. The service keeps the contents as they are; Ward parses
/// them in [`super::profile`]. The body is never logged.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileBody {
    pub version: u32,
    /// Part contents by name (`dnsProxy`, `mcp`, `claudeSettings`, `skills`, `vault`).
    pub parts: BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for ProfileBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileBody")
            .field("version", &self.version)
            .field("parts", &self.parts.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Result of the proxy check by `dnsProxy.checkUrl`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyCheck {
    pub ok: bool,
    pub message: String,
}

/// One part in the report of `POST /profile/applied`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PartReport {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

/// `POST /profile/applied`: the version Ward applied, not the one the service has now, so a
/// machine left behind stays visible on the Agents screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedRequest {
    pub version: u32,
    pub ok: bool,
    pub message: String,
    pub parts: Vec<PartReport>,
    pub proxy_check: ProxyCheck,
}

/// The `profile` block of a heartbeat: the freshest proxy check for the scheduler.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBeat {
    pub version: u32,
    pub ok: bool,
    pub proxy_check: ProxyCheck,
}

/// Status of a session as the contract names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Queued,
    Assigned,
    /// The orchestrator session collects the dossier and writes the plan.
    Preparing,
    /// The plan is ready and waits for the owner's `approve`.
    #[serde(rename = "plan-review")]
    PlanReview,
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
            Self::Preparing => "preparing",
            Self::PlanReview => "plan-review",
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileBeat>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HeartbeatResponse {
    pub limits: Option<Limits>,
    pub paused: bool,
    #[serde(deserialize_with = "lenient_list")]
    pub commands: Vec<Command>,
    /// The version of the machine profile the service has now; `None` from a service that
    /// knows no profile yet.
    pub profile_version: Option<u32>,
}

/// One task for this machine. `repo`, `baseBranch`, `branch` and `prompt` are optional: what
/// the service does not say, the orchestrator of the preparation phase decides (see
/// [`super::prepare`]), and the service sends them as `null`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Assignment {
    pub session_id: String,
    pub task_ref: TaskRef,
    pub attempt: u32,
    #[serde(deserialize_with = "string_or_empty")]
    pub title: String,
    #[serde(deserialize_with = "string_or_empty")]
    pub repo: String,
    #[serde(deserialize_with = "string_or_empty")]
    pub base_branch: String,
    #[serde(deserialize_with = "string_or_empty")]
    pub branch: String,
    #[serde(deserialize_with = "string_or_empty")]
    pub prompt: String,
    /// `claude`, `codex` or `auto`; empty in assignments from before providers existed.
    #[serde(deserialize_with = "string_or_empty")]
    pub provider: String,
    /// `None`: the agent's default model, no model flag.
    pub model: Option<String>,
    pub max_duration_min: u64,
    /// Wait for the owner's `approve` of the plan before the executor starts.
    pub plan_approval: bool,
    /// `None`: [`super::prepare::DEFAULT_MAX_DURATION_MIN`].
    pub prepare_max_duration_min: Option<u64>,
}

/// A field the service may send as `null`: an empty string then.
fn string_or_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
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
    /// The owner accepted the plan of the preparation phase: the executor starts.
    Approve {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    /// The owner refused the plan: the session ends.
    Reject {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        reason: String,
    },
}

impl Command {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Stop { session_id }
            | Self::Message { session_id, .. }
            | Self::Approve { session_id }
            | Self::Reject { session_id, .. } => session_id,
        }
    }
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

/// `reason` of a `blocked` status after the agent asked the owner a question and stopped. The
/// session is not over: the owner answers it with a `message` command and the agent goes on.
pub const REASON_OWNER_QUESTION: &str = "owner_question";

/// `reason` of a `failed` status when the preparation phase left no usable plan.
pub const REASON_PREPARE_FAILED: &str = "prepare_failed";

/// `reason` of a `failed` status after the owner refused the plan.
pub const REASON_PLAN_REJECTED: &str = "plan_rejected";

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

pub fn parse_profile(json: &str) -> Result<ProfileBody, serde_json::Error> {
    if json.trim().is_empty() {
        return Ok(ProfileBody::default());
    }
    serde_json::from_str(json)
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
