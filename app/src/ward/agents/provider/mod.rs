//! Coding agents a worker can run (`provider` in the contract). Everything that differs between
//! agents lives behind [`Provider`]: the launch command and its environment, how the session id
//! and the transcript of a launch are found, the account and the subscription limits, how a
//! refusal because of the limit looks and how one entry of its transcript reads as a line of
//! the chat ([`chat`]). A new agent is one more module plus an entry in [`all`].

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LastTurn {
    /// The agent owes something: a tool call without its result, the owner's message not
    /// answered yet, or an end of transcript Ward cannot read. Nothing is concluded from it.
    Working,
    /// The agent ended its turn with an answer of its own and waits for nothing.
    Answered,
    /// The agent ended its turn with a question for the owner and waits for the answer.
    Asked,
    /// The agent ended its turn on an error of its API instead of an answer and waits at an
    /// empty prompt: the text of the error as the owner reads it, empty when there was none.
    Failed(String),
}

/// Who said one line of a session's chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Voice {
    /// The owner: the prompt the session started with and every message typed into the agent.
    Owner,
    /// The agent's own text.
    Agent,
    /// What the agent did: one line per tool call.
    Tool,
    /// Not a reply of anybody: Ward's own line in the journal (the role of the part that
    /// follows, the restart it took the session over after) or an error of the agent's API.
    Ward,
}

impl Voice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Owner => "Владелец",
            Self::Agent => "Агент",
            Self::Tool => "Инструмент",
            Self::Ward => "Ward",
        }
    }
}

/// One line of the chat of a session, as the session screen shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    pub voice: Voice,
    pub text: String,
}

/// How much of one line of the chat is kept: a tool that printed a megabyte must not push
/// everything else off the screen.
pub const MAX_SAID_CHARS: usize = 4000;

/// How much of the arguments of a tool call one line of the chat shows.
pub const MAX_PREVIEW_CHARS: usize = 160;

impl Said {
    pub fn new(voice: Voice, text: impl AsRef<str>) -> Self {
        Self {
            voice,
            text: cut(text.as_ref().trim(), MAX_SAID_CHARS),
        }
    }
}

/// `text` up to `max` characters, with an ellipsis when something was left out. Counted in
/// characters, so a cut never lands inside one.
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// `text` as one line, runs of whitespace collapsed, up to `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    cut(&text.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

/// A short, one-line look at the arguments of a tool call: the field that says what was done,
/// else the arguments themselves.
pub fn preview(input: &serde_json::Value) -> String {
    const FIELDS: [&str; 8] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "question",
        "prompt",
        "description",
        "url",
    ];
    let field = FIELDS.iter().find_map(|name| {
        input
            .get(*name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
    });
    match field {
        Some(value) => one_line(value, MAX_PREVIEW_CHARS),
        None if input.is_null() => String::new(),
        None => one_line(&input.to_string(), MAX_PREVIEW_CHARS),
    }
}

/// One line for a tool call: its name and a short look at what it was called with.
pub fn tool_said(name: &str, input: Option<&serde_json::Value>) -> Said {
    let name = name.trim();
    let preview = input.map(preview).unwrap_or_default();
    let text = if preview.is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {preview}")
    };
    Said::new(Voice::Tool, text)
}

/// Ward's own marker of the journal, as a line of the chat; `None` for anything else. These are
/// the only entries of a journal that no agent wrote.
fn ward_said(entry: &serde_json::Value) -> Option<Said> {
    let text = match entry.get("type").and_then(serde_json::Value::as_str)? {
        "ward.role" => {
            let role = entry
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("агент");
            format!("Дальше транскрипт: {role}")
        }
        "ward.restart" => "Ward перезапустился и подхватил сессию".to_owned(),
        _ => return None,
    };
    Some(Said::new(Voice::Ward, text))
}

/// The chat of a session out of its journal (whole ndjson lines, as the service keeps them):
/// the owner's messages, the agent's text, the tools it called and Ward's own markers.
/// Entries that say nothing to a reader — a tool result, a file snapshot, a summary, a token
/// count — are left out, so the chat reads like a conversation and not like a log.
pub fn chat(provider: &dyn Provider, journal: &str) -> Vec<Said> {
    let mut chat = Vec::new();
    for line in journal.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match ward_said(&entry) {
            Some(said) => chat.push(said),
            None => chat.extend(provider.said(&entry)),
        }
    }
    chat
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

    /// `session_name`: `<taskNumber> <title>`; `model`: `None` for the agent's default, a model
    /// of this agent's family otherwise (see [`launch_model`]); `resume`: the agent session to
    /// continue instead of starting a new one.
    fn command(
        &self,
        session_name: &str,
        model: Option<&str>,
        resume: Option<&str>,
    ) -> AgentCommand;

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

    /// What one entry of the transcript says in the chat of the session: nothing for an entry
    /// that is no reply (a tool result, a snapshot, a token count), one line for a message,
    /// several for a message that also called tools. See [`chat`].
    fn said(&self, entry: &serde_json::Value) -> Vec<Said>;

    /// Whether the agent can find the owner's other sessions and write to them (`ListAgents`,
    /// `SendMessage`), so its prompt gets the note of [`super::peers`].
    fn messages_peers(&self) -> bool {
        false
    }
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

/// The agent a model belongs to, read the way the service reads it
/// (`ProviderSelector.modelFamily`): an id that starts with `claude` is Claude's, any other one
/// is Codex's.
pub fn model_family(model: &str) -> &'static str {
    if model.trim().to_lowercase().starts_with("claude") {
        claude::ID
    } else {
        codex::ID
    }
}

/// The model `provider` is started with: `model` trimmed, or `None` for the agent's default when
/// there is none or it belongs to the other agent. A model of the other family is no model for
/// this agent at all — Codex answers `claude-*` with `400 model_not_found` and does nothing — so
/// the agent starts with its own default instead, and the log says why.
pub fn launch_model<'a>(provider: &dyn Provider, model: Option<&'a str>) -> Option<&'a str> {
    let model = model.map(str::trim).filter(|model| !model.is_empty())?;
    if model_family(model) == provider.id() {
        return Some(model);
    }
    log::warn!(
        "Ward agents: {model} is not a model of {}, it starts with its default model",
        provider.id()
    );
    None
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
