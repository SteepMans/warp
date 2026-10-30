//! Pure parts of the Claude subscription usage chip: reading the OAuth token out of Claude Code's
//! credentials file, parsing `GET /api/oauth/usage`, the chip text, its color level, the tooltip
//! and the polling backoff. Parsing is lenient: unknown fields are ignored, missing or null
//! values leave that window out.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

/// Regular poll interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Longest interval after repeated failures.
pub const MAX_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Percent from which the chip turns to the warning color.
pub const WARNING_PERCENT: f64 = 50.;

/// Percent above which the chip turns to the error color.
pub const CRITICAL_PERCENT: f64 = 80.;

/// `claudeAiOauth.accessToken` of `~/.claude/.credentials.json`. The token is never logged.
pub fn parse_access_token(credentials_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(credentials_json).ok()?;
    let token = value.get("claudeAiOauth")?.get("accessToken")?.as_str()?.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

/// One rate limit window: how much of it is used and when it starts over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageWindow {
    /// Percent used, `0..=100` (may exceed 100 when over the limit).
    pub utilization: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub five_hour: UsageWindow,
    pub seven_day: UsageWindow,
}

fn parse_percent(value: &Value) -> Option<f64> {
    let percent = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }?;
    percent.is_finite().then_some(percent.max(0.))
}

/// RFC 3339 (`2026-10-30T15:00:00.123+00:00`), or unix seconds as a number.
fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => DateTime::parse_from_rfc3339(text.trim())
            .ok()
            .map(|time| time.with_timezone(&Utc)),
        Value::Number(number) => number
            .as_i64()
            .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single()),
        _ => None,
    }
}

fn parse_window(value: Option<&Value>) -> UsageWindow {
    let Some(value) = value.filter(|value| value.is_object()) else {
        return UsageWindow::default();
    };
    UsageWindow {
        utilization: value.get("utilization").and_then(parse_percent),
        resets_at: value.get("resets_at").and_then(parse_time),
    }
}

/// The usage response, or `None` when it has neither window's utilization.
pub fn parse_usage(json: &str) -> Option<Usage> {
    let value: Value = serde_json::from_str(json).ok()?;
    let usage = Usage {
        five_hour: parse_window(value.get("five_hour")),
        seven_day: parse_window(value.get("seven_day")),
    };
    (usage.five_hour.utilization.is_some() || usage.seven_day.utilization.is_some())
        .then_some(usage)
}

/// Whole percent, rounded: `2%`.
pub fn format_percent(percent: f64) -> String {
    format!("{}%", percent.round() as i64)
}

/// `5h 2% · 7d 60%`; a window without utilization is left out.
pub fn chip_text(usage: &Usage) -> Option<String> {
    let parts: Vec<String> = [("5h", &usage.five_hour), ("7d", &usage.seven_day)]
        .into_iter()
        .filter_map(|(label, window)| {
            window
                .utilization
                .map(|percent| format!("{label} {}", format_percent(percent)))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageLevel {
    /// Below 50%.
    Normal,
    /// 50% to 80%.
    Warning,
    /// Above 80%.
    Critical,
}

impl UsageLevel {
    pub fn of(percent: f64) -> Self {
        if percent > CRITICAL_PERCENT {
            Self::Critical
        } else if percent >= WARNING_PERCENT {
            Self::Warning
        } else {
            Self::Normal
        }
    }
}

/// The chip is as loud as its busiest window.
pub fn usage_level(usage: &Usage) -> UsageLevel {
    [&usage.five_hour, &usage.seven_day]
        .into_iter()
        .filter_map(|window| window.utilization)
        .map(UsageLevel::of)
        .max()
        .unwrap_or(UsageLevel::Normal)
}

/// `today 18:00` for a reset later today (in `tz`), else `Mon 03 Nov 10:00`.
pub fn format_reset<Tz: TimeZone>(at: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let local = at.with_timezone(tz);
    if local.date_naive() == now.with_timezone(tz).date_naive() {
        format!("today {}", local.format("%H:%M"))
    } else {
        local.format("%a %d %b %H:%M").to_string()
    }
}

/// Two lines, one per window: `5-hour limit: 2% used, resets today 18:00`.
pub fn tooltip_text<Tz: TimeZone>(usage: &Usage, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    [
        ("5-hour limit", &usage.five_hour),
        ("7-day limit", &usage.seven_day),
    ]
    .into_iter()
    .filter_map(|(label, window)| {
        let percent = window.utilization?;
        let mut line = format!("{label}: {} used", format_percent(percent));
        if let Some(at) = window.resets_at {
            line.push_str(", resets ");
            line.push_str(&format_reset(at, now, tz));
        }
        Some(line)
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Result of one poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// 2xx with a usable body.
    Fetched,
    /// No credentials file or token, or 401/403: the chip hides and polling keeps the regular
    /// pace, since Claude Code refreshes the token in the file by itself.
    Unauthorized,
    /// Network error, timeout, 429, 5xx or an unexpected body: retried later, more slowly.
    Failed,
}

impl PollOutcome {
    pub fn from_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Fetched,
            401 | 403 => Self::Unauthorized,
            _ => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    interval: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            interval: POLL_INTERVAL,
        }
    }
}

impl Backoff {
    /// Updates the interval after a poll and returns the delay until the next one.
    pub fn next_delay(&mut self, outcome: PollOutcome) -> Duration {
        match outcome {
            PollOutcome::Fetched | PollOutcome::Unauthorized => self.interval = POLL_INTERVAL,
            PollOutcome::Failed => self.interval = (self.interval * 2).min(MAX_INTERVAL),
        }
        self.interval
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
