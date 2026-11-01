//! Claude Code (`claude`).
//!
//! - Session: `~/.claude/sessions/<pid>.json` of the process started in the worktree
//!   (`sessionId`, `bridgeSessionId` → `https://claude.ai/code/<bridgeSessionId>`).
//! - Transcript: `~/.claude/projects/<slug of the worktree>/<sessionId>.jsonl`.
//! - Folder trust: `projects[<cwd>].hasTrustDialogAccepted` in `~/.claude.json`. In a folder
//!   it does not know yet an interactive session asks "Do you trust the files in this
//!   folder?" and waits for the answer; Claude Code's own advice for an unattended run is to
//!   set that field.
//! - Account: `oauthAccount` in `~/.claude.json` (no tokens there) or `ANTHROPIC_API_KEY`.
//! - Limits: the `api/oauth/usage` report of Ward's Claude header chip; without it the windows
//!   are sent with `usedPercent: null`.
//! - Limit refusal: an API error entry in the transcript with `"error":"rate_limit"` and
//!   `quotaLimits.resetsAt`, or the "You've hit your limit" text.
//! - Last turn: the newest `assistant` or `user` entry of the transcript. A `tool_use` block
//!   without its `tool_result` means the turn goes on; a text block that nothing follows is the
//!   agent's closing answer; a `tool_use` of [`ASK_TOOLS`] is a question waiting on the screen.

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone as _, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{
    Account, AgentCommand, FoundSession, Homes, LastTurn, LimitHit, Provider, WardUsage, rfc3339,
};
use crate::ward::agents::contract::UsageReport;
use crate::ward::agents::launch::forward_slashes;
use crate::ward::agents::profile;

pub const ID: &str = "claude";

pub const REMOTE_CONTROL_URL_PREFIX: &str = "https://claude.ai/code/";

pub struct Claude;

/// `~/.claude/sessions/<pid>.json`; other fields are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionFile {
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    /// Unix ms.
    pub started_at: i64,
    pub bridge_session_id: Option<String>,
}

pub fn parse_session_file(json: &str) -> Option<SessionFile> {
    serde_json::from_str::<SessionFile>(json)
        .ok()
        .filter(|file| !file.session_id.is_empty())
}

/// How far a session file's `startedAt` may be before the launch (clock jitter, rounding).
const STARTED_AT_SLACK_MS: i64 = 5_000;

/// The newest session started in `worktree` since `launched_at_ms`.
pub fn pick_session<'a>(
    files: impl IntoIterator<Item = &'a SessionFile>,
    worktree: &Path,
    launched_at_ms: i64,
) -> Option<&'a SessionFile> {
    files
        .into_iter()
        .filter(|file| file.started_at + STARTED_AT_SLACK_MS >= launched_at_ms)
        .filter(|file| super::same_path(&file.cwd, worktree))
        .max_by_key(|file| file.started_at)
}

pub fn remote_control_url(bridge_session_id: Option<&str>) -> Option<String> {
    bridge_session_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| format!("{REMOTE_CONTROL_URL_PREFIX}{id}"))
}

/// The project directory name Claude Code uses for a working directory: every character but
/// ASCII letters and digits becomes `-` (`C:\work\a b` → `C--work-a-b`).
pub fn project_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The field of a `projects` entry that holds the answer to the folder trust question.
const TRUST_FIELD: &str = "hasTrustDialogAccepted";

/// Every spelling of `dir` Claude Code may look the folder up under: the path as Ward built it
/// and, when it differs, the one the file system resolves it to, which is what Claude Code gets
/// from the working directory of its own process. Forward slashes, as Claude Code writes the
/// keys on Windows.
pub fn project_keys(dir: &Path) -> Vec<String> {
    let mut keys = vec![forward_slashes(dir)];
    if let Ok(real) = dunce::canonicalize(dir) {
        let real = forward_slashes(&real);
        if !keys.contains(&real) {
            keys.push(real);
        }
    }
    keys
}

/// A `projects` entry with the fields Claude Code puts in a new one and both questions of an
/// unattended start answered: the folder is trusted, and the warning about `@` includes of a
/// CLAUDE.md outside the repository counts as shown while the includes themselves stay
/// unapproved, so the session starts without pulling them in.
fn new_project_entry() -> Value {
    serde_json::json!({
        "allowedTools": [],
        "mcpContextUris": [],
        "mcpServers": {},
        "enabledMcpjsonServers": [],
        "disabledMcpjsonServers": [],
        "hasTrustDialogAccepted": true,
        "hasClaudeMdExternalIncludesApproved": false,
        "hasClaudeMdExternalIncludesWarningShown": true,
    })
}

/// Records the folder trust of `keys` in the object of `~/.claude.json`, leaving every other
/// field and every other project as it is; returns whether anything changed. `Err` for a file
/// whose shape Ward does not understand: it is the owner's agent configuration and Ward
/// rewrites nothing it cannot read.
pub fn with_trusted_folders(root: &mut Value, keys: &[String]) -> Result<bool, &'static str> {
    let object = root.as_object_mut().ok_or("не объект JSON")?;
    let projects = object
        .entry("projects".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("projects не объект")?;
    let mut changed = false;
    for key in keys {
        match projects.get_mut(key.as_str()) {
            Some(Value::Object(entry)) => {
                if entry.get(TRUST_FIELD) != Some(&Value::Bool(true)) {
                    entry.insert(TRUST_FIELD.to_owned(), Value::Bool(true));
                    changed = true;
                }
            }
            // No entry, or one Claude Code itself could not read: Ward writes its own.
            _ => {
                projects.insert(key.clone(), new_project_entry());
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// Writes the folder trust of `dir` into `claude_json` and leaves the rest of the file alone,
/// so the agent starts in an unknown folder instead of asking about it. There is no command
/// line flag and no variable for this: `--dangerously-skip-permissions` is about tools, and the
/// trust question comes before it.
pub fn trust_folder(claude_json: &Path, dir: &Path) -> Result<(), String> {
    let (mut root, original) = profile::read_json_object(claude_json)?;
    let changed = with_trusted_folders(&mut root, &project_keys(dir))
        .map_err(|err| format!("{}: {err}", claude_json.display()))?;
    if !changed {
        return Ok(());
    }
    profile::write_json_if_changed(claude_json, &root, &original)?;
    Ok(())
}

/// Plan name from `oauthAccount.organizationType` (`claude_max` → `max`), else the display name.
fn plan_of(account: &Value) -> Option<String> {
    let from_type = account
        .get("organizationType")
        .and_then(Value::as_str)
        .map(|kind| kind.strip_prefix("claude_").unwrap_or(kind).to_lowercase())
        .filter(|kind| !kind.is_empty());
    from_type.or_else(|| {
        account
            .get("planDisplayName")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .filter(|name| !name.is_empty())
    })
}

/// Account from the text of `~/.claude.json` and whether `ANTHROPIC_API_KEY` is set.
pub fn parse_account(claude_json: Option<&str>, api_key_set: bool) -> Account {
    let oauth = claude_json
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.get("oauthAccount").cloned())
        .filter(|account| {
            account
                .get("accountUuid")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        });
    match oauth {
        Some(account) => Account {
            logged_in: true,
            plan: plan_of(&account),
        },
        None if api_key_set => Account {
            logged_in: true,
            plan: Some("api".to_owned()),
        },
        None => Account::default(),
    }
}

/// The two windows of the contract from the header chip's report.
pub fn usage_reports(
    usage: Option<&crate::ward::claude_usage::usage::Usage>,
    now: DateTime<Utc>,
) -> Vec<UsageReport> {
    let window =
        |name: &str, minutes: i64, data: Option<&crate::ward::claude_usage::usage::UsageWindow>| {
            UsageReport {
                provider: ID.to_owned(),
                window: name.to_owned(),
                window_minutes: Some(minutes),
                used_percent: data.and_then(|data| data.utilization),
                resets_at: data.and_then(|data| data.resets_at).map(rfc3339),
                observed_at: usage.map(|_| rfc3339(now)),
                source: if usage.is_some() {
                    "oauth_usage"
                } else {
                    "none"
                }
                .to_owned(),
            }
        };
    vec![
        window("5h", 5 * 60, usage.map(|usage| &usage.five_hour)),
        window("week", 7 * 24 * 60, usage.map(|usage| &usage.seven_day)),
    ]
}

/// The last limit refusal in transcript lines, if the transcript ends with one.
pub fn parse_limit_hit(tail: &str) -> Option<LimitHit> {
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = entry.get("type").and_then(Value::as_str);
        if kind != Some("assistant") {
            // Only the agent's last word counts; system and meta lines are skipped.
            if kind == Some("user") {
                return None;
            }
            continue;
        }
        let is_api_error = entry
            .get("isApiErrorMessage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !is_api_error {
            return None;
        }
        if entry.get("error").and_then(Value::as_str) == Some("rate_limit") {
            let resets_at = entry
                .get("quotaLimits")
                .and_then(|limits| limits.get("resetsAt"))
                .and_then(Value::as_i64)
                .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single());
            let has_quota = entry.get("quotaLimits").is_some_and(Value::is_object);
            if has_quota {
                return Some(LimitHit { resets_at });
            }
        }
        let text = entry
            .pointer("/message/content")
            .map(|content| content.to_string().to_lowercase())
            .unwrap_or_default();
        if text.contains("hit your limit") || text.contains("usage limit reached") {
            return Some(LimitHit { resets_at: None });
        }
        return None;
    }
    None
}

/// The tools Claude Code asks the owner something with. A call of one of these is a question
/// on the screen of the tab: the agent stands still until somebody answers, and the session has
/// to call the owner instead of being taken for finished.
pub const ASK_TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];

/// What the last turn of the transcript was; see the module header for the entries it reads.
pub fn parse_last_turn(tail: &str) -> LastTurn {
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                // An error in place of an answer is no answer: the turn is not over.
                if entry
                    .get("isApiErrorMessage")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return LastTurn::Working;
                }
                let Some(blocks) = entry.pointer("/message/content").and_then(Value::as_array)
                else {
                    return LastTurn::Working;
                };
                // The newest block of the message decides; thinking and meta blocks are skipped.
                for block in blocks.iter().rev() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            return if ASK_TOOLS.contains(&name) {
                                LastTurn::Asked
                            } else {
                                LastTurn::Working
                            };
                        }
                        Some("text") => {
                            let text = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !text.trim().is_empty() {
                                return LastTurn::Answered;
                            }
                        }
                        _ => {}
                    }
                }
                return LastTurn::Working;
            }
            // A `tool_result` or a message of the owner: the agent still owes the answer to it.
            Some("user") => return LastTurn::Working,
            // System, summary and file snapshot lines say nothing about the turn.
            _ => continue,
        }
    }
    LastTurn::Working
}

impl Provider for Claude {
    fn id(&self) -> &'static str {
        ID
    }

    fn binary(&self) -> &'static str {
        "claude"
    }

    fn command(&self, session_name: &str, model: Option<&str>) -> AgentCommand {
        let mut args = vec![
            "--remote-control".to_owned(),
            session_name.to_owned(),
            "--dangerously-skip-permissions".to_owned(),
        ];
        if let Some(model) = model {
            args.push("--model".to_owned());
            args.push(model.to_owned());
        }
        AgentCommand {
            program: "claude".to_owned(),
            args,
            remove_env_prefixes: vec!["CLAUDE"],
            set_env: vec![(
                "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE".to_owned(),
                "1".to_owned(),
            )],
        }
    }

    fn prepare_workspace(&self, homes: &Homes, cwd: &Path) -> Result<(), String> {
        trust_folder(&homes.claude_json, cwd)
    }

    fn find_session(
        &self,
        homes: &Homes,
        worktree: &Path,
        launched_at_ms: i64,
    ) -> Option<FoundSession> {
        let entries = std::fs::read_dir(homes.claude_dir.join("sessions")).ok()?;
        let files: Vec<SessionFile> = entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|json| parse_session_file(&json))
            .collect();
        let file = pick_session(&files, worktree, launched_at_ms)?;
        Some(FoundSession {
            agent_session_id: file.session_id.clone(),
            pid: Some(file.pid).filter(|pid| *pid > 0),
            remote_control_url: remote_control_url(file.bridge_session_id.as_deref()),
            transcript: None,
        })
    }

    fn transcript(
        &self,
        homes: &Homes,
        worktree: &Path,
        session: &FoundSession,
    ) -> Option<PathBuf> {
        let projects = homes.claude_dir.join("projects");
        let name = format!("{}.jsonl", session.agent_session_id);
        let expected = projects.join(project_slug(worktree)).join(&name);
        if expected.is_file() {
            return Some(expected);
        }
        // Very long paths get a hashed slug; look the id up in every project instead.
        std::fs::read_dir(&projects)
            .ok()?
            .flatten()
            .map(|entry| entry.path().join(&name))
            .find(|path| path.is_file())
    }

    fn account(&self, homes: &Homes) -> Account {
        let claude_json = std::fs::read_to_string(&homes.claude_json).ok();
        let api_key_set = std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|key| !key.is_empty());
        parse_account(claude_json.as_deref(), api_key_set)
    }

    fn usage(&self, _homes: &Homes, ward: &WardUsage, now: DateTime<Utc>) -> Vec<UsageReport> {
        usage_reports(ward.claude.as_ref(), now)
    }

    fn limit_hit(&self, tail: &str) -> Option<LimitHit> {
        parse_limit_hit(tail)
    }

    fn last_turn(&self, tail: &str) -> LastTurn {
        parse_last_turn(tail)
    }
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
