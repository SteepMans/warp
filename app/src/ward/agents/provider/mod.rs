//! Coding agents a worker can run (`provider` in the contract). Everything that differs between
//! agents lives behind [`Provider`]: the launch command and its environment, how the session id
//! and the transcript of a launch are found, the account and the subscription limits, and how a
//! refusal because of the limit looks. A new agent is one more module plus an entry in [`all`].

pub mod claude;
pub mod codex;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::contract::UsageReport;

/// Where the agents keep their files; the home directories in production, temp dirs in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Homes {
    /// `~/.claude`.
    pub claude_dir: PathBuf,
    /// `~/.claude.json`.
    pub claude_json: PathBuf,
    /// `$CODEX_HOME`, by default `~/.codex`.
    pub codex_dir: PathBuf,
}

impl Homes {
    pub fn of_user() -> Option<Self> {
        let home = dirs::home_dir()?;
        let codex_dir = std::env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        Some(Self {
            claude_dir: home.join(".claude"),
            claude_json: home.join(".claude.json"),
            codex_dir,
        })
    }
}

/// Usage Ward already knows from its own header chips, captured on the main thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WardUsage {
    /// The last `api/oauth/usage` report of the Claude chip.
    pub claude: Option<crate::ward::claude_usage::usage::Usage>,
}

/// What to start, without the prompt: the runner script appends `-- <prompt>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Inherited variables starting with one of these are removed.
    pub remove_env_prefixes: Vec<&'static str>,
    /// Variables set for the agent.
    pub set_env: Vec<(String, String)>,
}

/// The agent session of one launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundSession {
    pub agent_session_id: String,
    /// The agent process, when the agent records it.
    pub pid: Option<u32>,
    pub remote_control_url: Option<String>,
    /// The transcript, when it is known already.
    pub transcript: Option<PathBuf>,
}

/// The agent stopped because the subscription limit was hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitHit {
    pub resets_at: Option<DateTime<Utc>>,
}

/// What the end of a transcript says the agent is doing, for a session whose transcript stopped
/// growing. An agent in remote control mode does not end its process after its answer, so the
/// last turn is the only thing that tells a finished session from one still going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastTurn {
    /// The agent owes something: a tool call without its result, the owner's message not
    /// answered yet, or an end of transcript Ward cannot read. Nothing is concluded from it.
    Working,
    /// The agent ended its turn with an answer of its own and waits for nothing.
    Answered,
    /// The agent ended its turn with a question for the owner and waits for the answer.
    Asked,
}

/// Login state of the agent, found without network calls and without using token values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    pub logged_in: bool,
    pub plan: Option<String>,
}

pub trait Provider: Send + Sync {
    /// `claude`, `codex`: the id in the contract.
    fn id(&self) -> &'static str;

    /// The executable looked up in `PATH`.
    fn binary(&self) -> &'static str;

    /// `session_name`: `<taskNumber> <title>`; `model`: `None` for the agent's default.
    fn command(&self, session_name: &str, model: Option<&str>) -> AgentCommand;

    /// Answers in the agent's own files, before its tab starts, whatever it would otherwise ask
    /// the owner about a working directory it does not know: nobody is watching that tab, and a
    /// question left on the screen is a session that never begins.
    fn prepare_workspace(&self, homes: &Homes, cwd: &Path) -> Result<(), String>;

    /// The session the agent started in `worktree` at or after `launched_at_ms` (Unix ms).
    fn find_session(
        &self,
        homes: &Homes,
        worktree: &Path,
        launched_at_ms: i64,
    ) -> Option<FoundSession>;

    /// The transcript file of `session`.
    fn transcript(&self, homes: &Homes, worktree: &Path, session: &FoundSession)
    -> Option<PathBuf>;

    fn account(&self, homes: &Homes) -> Account;

    /// One entry per limit window; `used_percent: None` when it is not known.
    fn usage(&self, homes: &Homes, ward: &WardUsage, now: DateTime<Utc>) -> Vec<UsageReport>;

    /// Whether the end of the transcript (`tail`, whole lines) says the agent hit its limit.
    fn limit_hit(&self, tail: &str) -> Option<LimitHit>;

    /// What the end of the transcript (`tail`, whole lines) says about the agent's last turn.
    fn last_turn(&self, tail: &str) -> LastTurn;
}

static CLAUDE: claude::Claude = claude::Claude;
static CODEX: codex::Codex = codex::Codex;

/// Every provider Ward can run, in the default order of preference.
pub fn all() -> [&'static dyn Provider; 2] {
    [&CLAUDE, &CODEX]
}

pub fn by_id(id: &str) -> Option<&'static dyn Provider> {
    all().into_iter().find(|provider| provider.id() == id)
}

/// Lower case, forward slashes, no trailing slash on Windows; as is elsewhere. For comparing
/// the `cwd` agents record with the worktree.
pub fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if cfg!(windows) {
        let path = path.replace('\\', "/").to_lowercase();
        let path = path.strip_prefix("//?/").unwrap_or(&path).to_owned();
        path.trim_end_matches('/').to_owned()
    } else {
        path.trim_end_matches('/').to_owned()
    }
}

pub fn same_path(a: &str, b: &Path) -> bool {
    normalize_path(a) == normalize_path(&b.to_string_lossy())
}

pub fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
