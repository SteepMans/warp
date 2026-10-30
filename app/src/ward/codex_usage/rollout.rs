//! Pure parts of the Codex usage chip: finding the newest Codex CLI rollout, pulling the last
//! `rate_limits` out of its `token_count` events, the chip text, its color level and the
//! tooltip. Parsing is lenient: unknown fields are ignored, a window without `used_percent` is
//! left out.
//!
//! A rollout line looks like
//! `{"timestamp":"2026-08-20T09:17:24.591Z","type":"event_msg","payload":{"type":"token_count",
//! "info":{..},"rate_limits":{"limit_id":"codex","primary":{"used_percent":94.0,
//! "window_minutes":10080,"resets_at":1787550558},"secondary":null,..}}}`. Older Codex versions
//! write `resets_in_seconds`, counted from the line's timestamp, instead of `resets_at`.

use std::cmp::Reverse;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::Value;

use crate::ward::claude_usage::usage::{UsageLevel, format_percent, format_reset};

/// The rate limit bucket of the Codex models; other buckets are a fallback.
const CODEX_LIMIT_ID: &str = "codex";

/// Day directories (`sessions/YYYY/MM/DD`) searched for the newest rollout. A session keeps
/// writing into the directory of the day it started, so a few weeks back are covered.
const MAX_DAY_DIRS: usize = 31;

/// Rollouts tried, newest first, until one has rate limits (a fresh session has none yet).
pub const MAX_ROLLOUTS_TRIED: usize = 5;

/// The tail read first; doubled until it has a rate limits line or reaches [`MAX_TAIL_BYTES`].
const INITIAL_TAIL_BYTES: u64 = 256 * 1024;

/// Tool output can make single lines huge; past this the rollout is skipped.
const MAX_TAIL_BYTES: u64 = 16 * 1024 * 1024;

/// One rate limit window of the Codex plan.
#[derive(Clone, Debug, PartialEq)]
pub struct RateWindow {
    /// Percent used, `0..` (may exceed 100 when over the limit).
    pub used_percent: f64,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<DateTime<Utc>>,
}

/// The last rate limits Codex reported, and when (the time of that Codex turn).
#[derive(Clone, Debug, PartialEq)]
pub struct CodexUsage {
    /// Shortest window first.
    pub windows: Vec<RateWindow>,
    pub updated_at: DateTime<Utc>,
}

fn parse_percent(value: &Value) -> Option<f64> {
    let percent = value.as_f64()?;
    percent.is_finite().then_some(percent.max(0.))
}

fn parse_window(value: Option<&Value>, line_time: DateTime<Utc>) -> Option<RateWindow> {
    let value = value.filter(|value| value.is_object())?;
    let used_percent = value.get("used_percent").and_then(parse_percent)?;
    let window_minutes = value
        .get("window_minutes")
        .and_then(Value::as_i64)
        .filter(|minutes| *minutes > 0);
    let resets_at = value
        .get("resets_at")
        .and_then(Value::as_i64)
        .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
        .or_else(|| {
            let seconds = value.get("resets_in_seconds")?.as_i64()?;
            line_time.checked_add_signed(Duration::seconds(seconds.max(0)))
        });
    Some(RateWindow {
        used_percent,
        window_minutes,
        resets_at,
    })
}

/// A `token_count` event with rate limits: its limit id and the usage it reports.
fn parse_line(line: &str) -> Option<(Option<String>, CodexUsage)> {
    // Cheap filter: most lines are messages and tool output.
    if !line.contains("\"rate_limits\"") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let payload = value.get("payload")?;
    if payload.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let updated_at = DateTime::parse_from_rfc3339(value.get("timestamp")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    let limits = payload
        .get("rate_limits")
        .filter(|limits| limits.is_object())?;
    let mut windows: Vec<RateWindow> = ["primary", "secondary"]
        .into_iter()
        .filter_map(|key| parse_window(limits.get(key), updated_at))
        .collect();
    if windows.is_empty() {
        return None;
    }
    windows.sort_by_key(|window| window.window_minutes.unwrap_or(i64::MAX));
    let limit_id = limits
        .get("limit_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((
        limit_id,
        CodexUsage {
            windows,
            updated_at,
        },
    ))
}

/// The last rate limits in a run of rollout lines: the Codex bucket if present, else the last
/// bucket of any kind. `skip_first_line` drops a line cut by reading only the file's tail.
pub fn latest_usage(text: &str, skip_first_line: bool) -> Option<CodexUsage> {
    let mut lines = text.lines();
    if skip_first_line {
        lines.next();
    }
    let mut fallback = None;
    for line in lines.rev() {
        let Some((limit_id, usage)) = parse_line(line) else {
            continue;
        };
        if limit_id.as_deref().is_none_or(|id| id == CODEX_LIMIT_ID) {
            return Some(usage);
        }
        if fallback.is_none() {
            fallback = Some(usage);
        }
    }
    fallback
}

/// Reads the rollout's tail, growing it until a rate limits line shows up.
pub fn read_latest_usage(path: &Path) -> Option<CodexUsage> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut tail = INITIAL_TAIL_BYTES;
    loop {
        let start = len.saturating_sub(tail);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::with_capacity((len - start) as usize);
        file.by_ref()
            .take(len - start)
            .read_to_end(&mut bytes)
            .ok()?;
        let text = String::from_utf8_lossy(&bytes);
        if let Some(usage) = latest_usage(&text, start > 0) {
            return Some(usage);
        }
        if start == 0 || tail >= MAX_TAIL_BYTES {
            return None;
        }
        tail *= 2;
    }
}

fn sorted_subdirs_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

/// `rollout-*.jsonl` files of the newest day directories, most recently written first.
pub fn recent_rollouts(sessions_dir: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut day_dirs = Vec::new();
    'years: for year in sorted_subdirs_desc(sessions_dir) {
        for month in sorted_subdirs_desc(&year) {
            for day in sorted_subdirs_desc(&month) {
                day_dirs.push(day);
                if day_dirs.len() >= MAX_DAY_DIRS {
                    break 'years;
                }
            }
        }
    }
    let mut rollouts: Vec<(PathBuf, SystemTime, u64)> = day_dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("rollout-") && name.ends_with(".jsonl")
        })
        .filter_map(|entry| {
            let metadata = entry
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file())?;
            Some((entry.path(), metadata.modified().ok()?, metadata.len()))
        })
        .collect();
    rollouts.sort_by_key(|(_, modified, _)| Reverse(*modified));
    rollouts
}

impl CodexUsage {
    /// The windows still worth showing at `now`: not past their reset, and, without a reset
    /// time, reported less than one window ago. `None` when nothing is left.
    pub fn current(&self, now: DateTime<Utc>) -> Option<CodexUsage> {
        let windows: Vec<RateWindow> = self
            .windows
            .iter()
            .filter(|window| match (window.resets_at, window.window_minutes) {
                (Some(resets_at), _) => resets_at > now,
                (None, Some(minutes)) => self.updated_at + Duration::minutes(minutes) > now,
                (None, None) => false,
            })
            .cloned()
            .collect();
        (!windows.is_empty()).then(|| CodexUsage {
            windows,
            updated_at: self.updated_at,
        })
    }
}

/// `5h`, `7d`, `90m`: the window length in its largest whole unit.
pub fn window_label(minutes: Option<i64>) -> String {
    match minutes {
        Some(minutes) if minutes % (24 * 60) == 0 => format!("{}d", minutes / (24 * 60)),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
        None => "limit".to_owned(),
    }
}

/// `5-hour limit`, `7-day limit` for the tooltip.
fn window_name(minutes: Option<i64>) -> String {
    match minutes {
        Some(minutes) if minutes % (24 * 60) == 0 => format!("{}-day limit", minutes / (24 * 60)),
        Some(minutes) if minutes % 60 == 0 => format!("{}-hour limit", minutes / 60),
        Some(minutes) => format!("{minutes}-minute limit"),
        None => "Limit".to_owned(),
    }
}

/// `5h 10% · 7d 30%`.
pub fn chip_text(usage: &CodexUsage) -> Option<String> {
    let parts: Vec<String> = usage
        .windows
        .iter()
        .map(|window| {
            format!(
                "{} {}",
                window_label(window.window_minutes),
                format_percent(window.used_percent)
            )
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// The chip is as loud as its busiest window.
pub fn usage_level(usage: &CodexUsage) -> UsageLevel {
    usage
        .windows
        .iter()
        .map(|window| UsageLevel::of(window.used_percent))
        .max()
        .unwrap_or(UsageLevel::Normal)
}

/// `just now`, `12 min ago`, `3 h ago`, `2 d ago`.
pub fn format_age(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (now - at).num_minutes().max(0);
    if minutes < 1 {
        "just now".to_owned()
    } else if minutes < 60 {
        format!("{minutes} min ago")
    } else if minutes < 48 * 60 {
        format!("{} h ago", minutes / 60)
    } else {
        format!("{} d ago", minutes / (24 * 60))
    }
}

/// One line per window, then when Codex reported it:
/// `5-hour limit: 10% used, resets today 18:00` ... `Updated 12 min ago (last Codex turn)`.
pub fn tooltip_text<Tz: TimeZone>(usage: &CodexUsage, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut lines: Vec<String> = usage
        .windows
        .iter()
        .map(|window| {
            let mut line = format!(
                "{}: {} used",
                window_name(window.window_minutes),
                format_percent(window.used_percent)
            );
            if let Some(at) = window.resets_at {
                line.push_str(", resets ");
                line.push_str(&format_reset(at, now, tz));
            }
            line
        })
        .collect();
    lines.push(format!(
        "Updated {} (last Codex turn)",
        format_age(usage.updated_at, now)
    ));
    lines.join("\n")
}

#[cfg(test)]
#[path = "rollout_tests.rs"]
mod tests;
