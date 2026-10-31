//! Codex CLI (`codex`).
//!
//! - Session: the `rollout-*.jsonl` under `$CODEX_HOME/sessions/YYYY/MM/DD` whose first line
//!   (`session_meta`) has the worktree as `cwd`; its `id` is the session id and the file is the
//!   transcript. Codex has no Remote Control: `remoteControlUrl` stays `null`.
//! - Account: `auth.json` (`auth_mode`, whether `tokens` or `OPENAI_API_KEY` are present; the
//!   values are skipped while parsing).
//! - Limits and plan: the last `token_count` event with `rate_limits` (`primary`, `secondary`,
//!   `plan_type`) in the newest rollouts.
//! - Limit refusal: after the last turn started, an `error` event about the usage limit or a
//!   `token_count` with `rate_limit_reached_type`.

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, TimeZone as _, Utc};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use super::{Account, AgentCommand, FoundSession, Homes, LimitHit, Provider, WardUsage, rfc3339};
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
                    .unwrap_or_default()
                    .to_lowercase();
                if message.contains("usage limit") {
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

/// Usage windows of the contract from the last rate limits.
pub fn usage_reports(limits: Option<&RateLimits>) -> Vec<UsageReport> {
    let report = |name: &str, window: Option<&Window>| UsageReport {
        provider: ID.to_owned(),
        window: name.to_owned(),
        window_minutes: window.and_then(|window| window.window_minutes),
        used_percent: window.and_then(|window| window.used_percent),
        resets_at: window.and_then(|window| window.resets_at).map(rfc3339),
        observed_at: limits.and_then(|limits| limits.observed_at).map(rfc3339),
        source: if limits.is_some() { "rollout" } else { "none" }.to_owned(),
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

    fn command(&self, _session_name: &str, model: Option<&str>) -> AgentCommand {
        let mut args = vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()];
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

    fn usage(&self, homes: &Homes, _ward: &WardUsage, _now: DateTime<Utc>) -> Vec<UsageReport> {
        usage_reports(Self::latest_limits(homes).as_ref())
    }

    fn limit_hit(&self, tail: &str) -> Option<LimitHit> {
        parse_limit_hit(tail)
    }
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
