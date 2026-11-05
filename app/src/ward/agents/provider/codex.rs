//! Codex CLI (`codex`).
//!
//! - Session: the `rollout-*.jsonl` under `$CODEX_HOME/sessions/YYYY/MM/DD` whose first line
//!   (`session_meta`) has the worktree as `cwd`; its `id` is the session id and the file is the
//!   transcript. Codex has no Remote Control: `remoteControlUrl` stays `null`.
//! - Account: `auth.json` (`auth_mode`, whether `tokens` or `OPENAI_API_KEY` are present; the
//!   values are skipped while parsing).
//! - Limits and plan: the last `token_count` event with `rate_limits` (`primary`, `secondary`,
//!   `plan_type`) in the newest rollouts. They are as fresh as the last Codex turn on this
//!   machine, which may be weeks ago: a window that has started over since is reported empty
//!   ([`usage_reports`]).
//! - Limit refusal: after the last turn started, an `error` event about the usage limit, the
//!   `error` of `task_complete` about it (Codex 0.147 and later) or a `token_count` with
//!   `rate_limit_reached_type`.
//! - Last turn: the newest event or item of the rollout that says something about it —
//!   `task_complete` and `agent_message` end a turn, a `function_call` waits for its output,
//!   `task_started` and `user_message` are a turn still going. A `task_complete` with an
//!   `error` (Codex 0.147 and later), or an `error` event right before a `task_complete`
//!   without the agent's last message (earlier ones), is a turn that ended on an error of the
//!   API: [`super::LastTurn::Failed`], not an answer. Codex runs with
//!   `--dangerously-bypass-approvals-and-sandbox`, so it asks the owner nothing and
//!   [`super::LastTurn::Asked`] never comes of a rollout.
//! - Chat: the same events read as replies ([`parse_said`]) — `user_message` is the owner,
//!   `agent_message` the agent, a `function_call` (also `custom_tool_call`, `local_shell_call`)
//!   a line of its own; `error`, and the `error` of `task_complete`, is Ward's line.

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, TimeDelta, TimeZone as _, Utc};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use super::{
    Account, AgentCommand, FoundSession, Homes, LastTurn, LimitHit, Provider, Said, Voice,
    WardUsage, rfc3339, tool_said,
};
use crate::ward::agents::contract::UsageReport;
use crate::ward::codex_usage::rollout::recent_rollouts;

pub const ID: &str = "codex";

/// Rollouts read for the limits, newest first.
const MAX_ROLLOUTS_FOR_USAGE: usize = 5;

/// The tail of a rollout read for the limits.
const USAGE_TAIL_BYTES: u64 = 512 * 1024;

pub struct Codex;

/// One window of `rate_limits`.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<DateTime<Utc>>,
}

/// `rate_limits` of one `token_count` event.
#[derive(Clone, Debug, PartialEq)]
pub struct RateLimits {
    pub primary: Option<Window>,
    pub secondary: Option<Window>,
    pub plan_type: Option<String>,
    pub reached_type: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
}

fn parse_window(value: Option<&Value>) -> Option<Window> {
    let value = value.filter(|value| value.is_object())?;
    Some(Window {
        used_percent: value
            .get("used_percent")
            .and_then(Value::as_f64)
            .filter(|percent| percent.is_finite()),
        window_minutes: value.get("window_minutes").and_then(Value::as_i64),
        resets_at: value
            .get("resets_at")
            .and_then(Value::as_i64)
            .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single()),
    })
}

fn non_empty_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The rate limits of a rollout line, if it is a `token_count` event that has them.
pub fn parse_rate_limits(line: &str) -> Option<RateLimits> {
    if !line.contains("\"rate_limits\"") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let payload = value.get("payload")?;
    if payload.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let limits = payload
        .get("rate_limits")
        .filter(|limits| limits.is_object())?;
    Some(RateLimits {
        primary: parse_window(limits.get("primary")),
        secondary: parse_window(limits.get("secondary")),
        plan_type: non_empty_str(limits.get("plan_type")),
        reached_type: non_empty_str(limits.get("rate_limit_reached_type")),
        observed_at: value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&Utc)),
    })
}

/// The last rate limits in rollout text.
pub fn latest_rate_limits(text: &str) -> Option<RateLimits> {
    text.lines().rev().find_map(parse_rate_limits)
}

fn event_type(value: &Value) -> Option<&str> {
    (value.get("type")?.as_str()? == "event_msg")
        .then(|| value.pointer("/payload/type")?.as_str())
        .flatten()
}

/// Whether the message of an error is Codex's refusal for the usage limit.
fn is_usage_limit(message: &str) -> bool {
    message.to_lowercase().contains("usage limit")
}

/// Whether the last turn in rollout text ended on the usage limit.
pub fn parse_limit_hit(tail: &str) -> Option<LimitHit> {
    let mut latest_limits: Option<RateLimits> = None;
    let mut hit = false;
    for line in tail.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match event_type(&value) {
            Some("task_started") | Some("user_message") => break,
            Some("error") => {
                let message = value
                    .pointer("/payload/message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if is_usage_limit(message) {
                    hit = true;
                }
            }
            Some("task_complete") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                let info = payload
                    .pointer("/error/codex_error_info")
                    .and_then(Value::as_str);
                if task_error(payload).is_some_and(is_usage_limit)
                    || info == Some("usage_limit_exceeded")
                {
                    hit = true;
                }
            }
            Some("token_count") if latest_limits.is_none() => {
                latest_limits = parse_rate_limits(line);
                if latest_limits
                    .as_ref()
                    .is_some_and(|limits| limits.reached_type.is_some())
                {
                    hit = true;
                }
            }
            _ => {}
        }
    }
    if !hit {
        return None;
    }
    let resets_at = latest_limits.and_then(|limits| {
        [limits.primary, limits.secondary]
            .into_iter()
            .flatten()
            .filter(|window| window.used_percent.is_some_and(|percent| percent >= 100.0))
            .filter_map(|window| window.resets_at)
            .max()
    });
    Some(LimitHit { resets_at })
}

/// The message of an error of the API as the owner reads it. Codex keeps the body of the
/// answer as it came, `{"type":"error","status":400,"error":{"message":…,"code":…}}`: the
/// message and the code are taken out of it, any other text stays as it is.
pub fn readable_error(message: &str) -> String {
    let message = message.trim();
    let Ok(body) = serde_json::from_str::<Value>(message) else {
        return message.to_owned();
    };
    let error = body
        .get("error")
        .filter(|error| error.is_object())
        .unwrap_or(&body);
    let Some(text) = non_empty_str(error.get("message")) else {
        return message.to_owned();
    };
    match non_empty_str(error.get("code")) {
        Some(code) => format!("{text} ({code})"),
        None => text,
    }
}

/// The raw message of the error `task_complete` carries when its turn ended on one; Codex
/// 0.147 and later put it there instead of an `error` event of its own.
fn task_error(payload: &Value) -> Option<&str> {
    match payload.get("error")? {
        Value::Null => None,
        Value::String(text) => Some(text.as_str()),
        error => Some(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
    }
}

/// What the last turn of the rollout was; see the module header for the events it reads.
pub fn parse_last_turn(tail: &str) -> LastTurn {
    // A `task_complete` without the agent's last message is looked past: an `error` event
    // right before it is the error the turn ended on (Codex before 0.147).
    let mut completed = false;
    for line in tail.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let payload = value.get("payload").unwrap_or(&Value::Null);
        match payload.get("type").and_then(Value::as_str) {
            Some("task_complete") if !completed => {
                if let Some(error) = task_error(payload) {
                    return LastTurn::Failed(readable_error(error));
                }
                if non_empty_str(payload.get("last_agent_message")).is_some() {
                    return LastTurn::Answered;
                }
                completed = true;
            }
            Some("error") if completed => {
                let message = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return LastTurn::Failed(readable_error(message));
            }
            Some("task_complete" | "agent_message") => return LastTurn::Answered,
            Some(
                "task_started"
                | "user_message"
                | "function_call"
                | "function_call_output"
                | "custom_tool_call"
                | "local_shell_call",
            ) => {
                return if completed {
                    LastTurn::Answered
                } else {
                    LastTurn::Working
                };
            }
            // Reasoning and token counts say nothing about the end of the turn, and neither does
            // an error the turn went on after.
            _ => continue,
        }
    }
    if completed {
        LastTurn::Answered
    } else {
        LastTurn::Working
    }
}

/// What one event of a rollout says in the chat: the owner's message, the agent's message, the
/// tool it called. Reasoning, token counts, task markers and the output of a tool say nothing
/// to a reader and give nothing; an error is Ward's line, not an answer of the agent.
pub fn parse_said(entry: &Value) -> Vec<Said> {
    let payload = match entry.get("payload") {
        Some(payload) => payload,
        None => return Vec::new(),
    };
    let text = |field: &str| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match payload.get("type").and_then(Value::as_str) {
        Some("user_message") => one(Said::new(Voice::Owner, text("message"))),
        Some("agent_message") => one(Said::new(Voice::Agent, text("message"))),
        Some("function_call" | "custom_tool_call" | "local_shell_call") => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("инструмент");
            // `arguments` is JSON in a string; what cannot be parsed is shown as it is.
            let arguments = payload.get("arguments").and_then(Value::as_str);
            let parsed = arguments.and_then(|text| serde_json::from_str::<Value>(text).ok());
            match parsed {
                Some(input) => one(tool_said(name, Some(&input))),
                None => {
                    let raw = arguments.map(|text| Value::String(text.to_owned()));
                    one(tool_said(name, raw.as_ref()))
                }
            }
        }
        Some("error") => one(Said::new(
            Voice::Ward,
            format!("Ошибка агента: {}", readable_error(&text("message"))),
        )),
        // Codex 0.147 and later write the error a turn ended on into its `task_complete`.
        Some("task_complete") => match task_error(payload) {
            Some(error) => one(Said::new(
                Voice::Ward,
                format!("Ошибка агента: {}", readable_error(error)),
            )),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// One line of the chat, or none when it came out empty.
fn one(said: Said) -> Vec<Said> {
    if said.text.is_empty() {
        Vec::new()
    } else {
        vec![said]
    }
}

/// `auth.json`; token values are skipped, only their presence is kept.
#[derive(Default, Deserialize)]
struct AuthFile {
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    tokens: Option<IgnoredAny>,
    #[serde(default, rename = "OPENAI_API_KEY")]
    api_key: Option<IgnoredAny>,
}

/// Account from the text of `auth.json` and the plan of the last rate limits.
pub fn parse_account(auth_json: Option<&str>, plan_type: Option<String>) -> Account {
    let Some(auth) = auth_json.and_then(|text| serde_json::from_str::<AuthFile>(text).ok()) else {
        return Account::default();
    };
    let logged_in = auth.tokens.is_some() || auth.api_key.is_some();
    let plan = if auth.auth_mode.as_deref() == Some("apikey") {
        Some("api".to_owned())
    } else {
        plan_type
    };
    Account {
        logged_in,
        plan: plan.filter(|_| logged_in),
    }
}

/// Whether the window has started over by `now`: its reset time has passed or, without one, a
/// whole window has gone by since Codex reported it (`observed_at`).
fn started_over(window: &Window, observed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match (window.resets_at, window.window_minutes, observed_at) {
        (Some(resets_at), _, _) => resets_at <= now,
        (None, Some(minutes), Some(observed_at)) => TimeDelta::try_minutes(minutes)
            .and_then(|length| observed_at.checked_add_signed(length))
            .is_some_and(|end| end <= now),
        _ => false,
    }
}

/// Usage windows of the contract from the last rate limits. A window that has started over by
/// `now` (see `started_over`) is reported empty, as it was at its reset, with the next reset
/// unknown: the percent of the rollout belongs to the window before. Codex may not have run
/// here for weeks, and an old 94% must neither show nor keep Codex from taking assignments
/// until its next turn reports the new window.
pub fn usage_reports(limits: Option<&RateLimits>, now: DateTime<Utc>) -> Vec<UsageReport> {
    let observed_at = limits.and_then(|limits| limits.observed_at);
    let report = |name: &str, window: Option<&Window>| {
        let (used_percent, resets_at, observed_at) = match window {
            Some(window) if started_over(window, observed_at, now) => {
                (Some(0.0), None, window.resets_at)
            }
            Some(window) => (window.used_percent, window.resets_at, observed_at),
            None => (None, None, observed_at),
        };
        UsageReport {
            provider: ID.to_owned(),
            window: name.to_owned(),
            window_minutes: window.and_then(|window| window.window_minutes),
            used_percent,
            resets_at: resets_at.map(rfc3339),
            observed_at: observed_at.map(rfc3339),
            source: if limits.is_some() { "rollout" } else { "none" }.to_owned(),
        }
    };
    let mut reports = vec![report(
        "primary",
        limits.and_then(|limits| limits.primary.as_ref()),
    )];
    if let Some(secondary) = limits.and_then(|limits| limits.secondary.as_ref()) {
        reports.push(report("secondary", Some(secondary)));
    }
    reports
}

/// `session_meta` of a rollout's first line: the session id and `cwd`.
pub fn parse_session_meta(first_line: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(first_line.trim()).ok()?;
    if value.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let payload = value.get("payload")?;
    let id =
        non_empty_str(payload.get("id")).or_else(|| non_empty_str(payload.get("session_id")))?;
    let cwd = non_empty_str(payload.get("cwd"))?;
    Some((id, cwd))
}

fn read_first_line(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buffer = vec![0u8; 64 * 1024];
    let read = file.read(&mut buffer).ok()?;
    let text = String::from_utf8_lossy(&buffer[..read]);
    text.lines().next().map(str::to_owned)
}

fn read_tail(path: &Path, bytes: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).ok()?;
    let text = String::from_utf8_lossy(&buffer).into_owned();
    // A tail that starts mid-line drops the partial line.
    Some(if start > 0 {
        text.split_once('\n')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_default()
    } else {
        text
    })
}

impl Codex {
    fn sessions_dir(homes: &Homes) -> PathBuf {
        homes.codex_dir.join("sessions")
    }

    fn latest_limits(homes: &Homes) -> Option<RateLimits> {
        recent_rollouts(&Self::sessions_dir(homes))
            .into_iter()
            .take(MAX_ROLLOUTS_FOR_USAGE)
            .find_map(|(path, _, _)| latest_rate_limits(&read_tail(&path, USAGE_TAIL_BYTES)?))
    }
}

impl Provider for Codex {
    fn id(&self) -> &'static str {
        ID
    }

    fn binary(&self) -> &'static str {
        "codex"
    }

    fn command(
        &self,
        _session_name: &str,
        model: Option<&str>,
        resume: Option<&str>,
    ) -> AgentCommand {
        // `resume` is a subcommand of Codex and goes before the flags.
        let mut args = match resume {
            Some(id) => vec!["resume".to_owned(), id.to_owned()],
            None => Vec::new(),
        };
        args.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
        if let Some(model) = model {
            args.push("-m".to_owned());
            args.push(model.to_owned());
        }
        AgentCommand {
            program: "codex".to_owned(),
            args,
            // Ward may run inside an agent session; its markers must not leak into the agent.
            remove_env_prefixes: vec!["CLAUDE", "CODEX_SANDBOX", "CODEX_THREAD"],
            set_env: Vec::new(),
        }
    }

    fn prepare_workspace(&self, _homes: &Homes, _cwd: &Path) -> Result<(), String> {
        // Nothing to write: `--dangerously-bypass-approvals-and-sandbox` skips every
        // confirmation Codex would ask, the one about a directory it does not know included.
        Ok(())
    }

    fn find_session(
        &self,
        homes: &Homes,
        worktree: &Path,
        launched_at_ms: i64,
    ) -> Option<FoundSession> {
        let launched_at = UNIX_EPOCH + Duration::from_millis(launched_at_ms.max(0) as u64);
        recent_rollouts(&Self::sessions_dir(homes))
            .into_iter()
            .filter(|(_, modified, _)| *modified + Duration::from_secs(5) >= launched_at)
            .find_map(|(path, _, _)| {
                let (id, cwd) = parse_session_meta(&read_first_line(&path)?)?;
                super::same_path(&cwd, worktree).then_some(FoundSession {
                    agent_session_id: id,
                    pid: None,
                    remote_control_url: None,
                    transcript: Some(path),
                })
            })
    }

    fn transcript(
        &self,
        _homes: &Homes,
        _worktree: &Path,
        session: &FoundSession,
    ) -> Option<PathBuf> {
        session.transcript.clone()
    }

    fn account(&self, homes: &Homes) -> Account {
        let auth = std::fs::read_to_string(homes.codex_dir.join("auth.json")).ok();
        let plan = Self::latest_limits(homes).and_then(|limits| limits.plan_type);
        parse_account(auth.as_deref(), plan)
    }

    fn usage(&self, homes: &Homes, _ward: &WardUsage, now: DateTime<Utc>) -> Vec<UsageReport> {
        usage_reports(Self::latest_limits(homes).as_ref(), now)
    }

    fn limit_hit(&self, tail: &str) -> Option<LimitHit> {
        parse_limit_hit(tail)
    }

    fn last_turn(&self, tail: &str) -> LastTurn {
        parse_last_turn(tail)
    }

    fn said(&self, entry: &Value) -> Vec<Said> {
        parse_said(entry)
    }
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
