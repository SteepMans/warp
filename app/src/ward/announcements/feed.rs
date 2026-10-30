//! Pure parts of the announcements feed: parsing `GET /api/v1/ward/announcements` and choosing
//! which announcements this build should show. Parsing is lenient: unknown fields are ignored,
//! a malformed item is skipped without dropping the rest of the list, and a bare top-level
//! array is accepted as well as `{"announcements": [...]}`.

use std::cmp::Ordering;
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::shown::ShownIds;

/// Larger responses are not an announcements feed.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Items past this many in one response are ignored.
pub const MAX_ITEMS: usize = 100;

/// New announcements shown per poll; the rest wait for the next poll. Matches the number of
/// toasts a window shows at once.
pub const MAX_SHOWN_PER_POLL: usize = 3;

const MAX_ID_CHARS: usize = 128;
const MAX_TITLE_CHARS: usize = 200;
const MAX_TEXT_CHARS: usize = 2000;
const MAX_URL_CHARS: usize = 2048;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    #[default]
    Info,
    Warning,
    Critical,
}

impl Level {
    /// Unknown or missing levels are shown as information.
    fn parse(value: Option<&Value>) -> Self {
        match value
            .and_then(Value::as_str)
            .map(|level| level.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("warning") => Self::Warning,
            Some("critical") => Self::Critical,
            _ => Self::Info,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    pub id: String,
    pub title: String,
    /// Plain text or simple markdown, shown as is; may be empty.
    pub body: String,
    pub level: Level,
    pub created_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub min_version: Option<String>,
    pub max_version: Option<String>,
    /// Only `http` and `https` links are kept.
    pub url: Option<String>,
}

impl Announcement {
    /// Whether the announcement is still current and meant for a build of `version`.
    pub fn is_visible(&self, now: DateTime<Utc>, version: &[u64]) -> bool {
        self.expires_at.is_none_or(|expires_at| now < expires_at)
            && version_in_bounds(
                version,
                self.min_version.as_deref(),
                self.max_version.as_deref(),
            )
    }
}

/// The feed's announcements, or `None` when the body is not a feed at all.
pub fn parse_feed(json: &str) -> Option<Vec<Announcement>> {
    if json.len() > MAX_BODY_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(json).ok()?;
    let items = match &value {
        Value::Array(items) => items,
        Value::Object(object) => object.get("announcements")?.as_array()?,
        _ => return None,
    };
    Some(
        items
            .iter()
            .take(MAX_ITEMS)
            .filter_map(parse_announcement)
            .collect(),
    )
}

fn parse_announcement(value: &Value) -> Option<Announcement> {
    let object = value.as_object()?;
    let id = non_empty_str(object.get("id"))?;
    if id.chars().count() > MAX_ID_CHARS {
        return None;
    }
    let title = non_empty_str(object.get("title"))?;
    let body = match object.get("body") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(body)) => body.trim().to_owned(),
        Some(_) => return None,
    };
    Some(Announcement {
        id: id.to_owned(),
        title: truncate(title, MAX_TITLE_CHARS),
        body: truncate(&body, MAX_TEXT_CHARS),
        level: Level::parse(object.get("level")),
        created_at: optional_time(object.get("created_at")).unwrap_or_default(),
        // An expiry that cannot be read could hide a withdrawn announcement; skip the item.
        expires_at: optional_time(object.get("expires_at"))?,
        min_version: non_empty_str(object.get("min_version")).map(str::to_owned),
        max_version: non_empty_str(object.get("max_version")).map(str::to_owned),
        url: non_empty_str(object.get("url")).and_then(web_url),
    })
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// `Some(None)` for a missing or null time, `None` for one that cannot be parsed.
fn optional_time(value: Option<&Value>) -> Option<Option<DateTime<Utc>>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) if text.trim().is_empty() => Some(None),
        Some(Value::String(text)) => DateTime::parse_from_rfc3339(text.trim())
            .ok()
            .map(|time| Some(time.with_timezone(&Utc))),
        Some(_) => None,
    }
}

fn web_url(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let is_web = lower.starts_with("https://") || lower.starts_with("http://");
    (is_web && url.len() <= MAX_URL_CHARS && !url.chars().any(char::is_whitespace))
        .then(|| url.to_owned())
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// The runs of digits of a version as numbers: `v0.2026.10.29.12.00.oss_00` is
/// `[0, 2026, 10, 29, 12, 0, 0]`. Empty for versions without digits (dev builds).
pub fn version_numbers(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .map(|run| run.parse().unwrap_or(u64::MAX))
        .collect()
}

/// Compares `version` with `bound` over the bound's length only, so a bound of
/// `0.2026.10.29` stands for every build of that day. A missing part of `version` counts as 0.
fn compare_prefix(version: &[u64], bound: &[u64]) -> Ordering {
    let padded = version.iter().copied().chain(std::iter::repeat(0));
    padded
        .zip(bound.iter().copied())
        .map(|(part, bound_part)| part.cmp(&bound_part))
        .find(|ordering| ordering.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// Both bounds are inclusive. A bound without digits matches nothing, and a build without a
/// version only sees announcements without bounds.
fn version_in_bounds(version: &[u64], min: Option<&str>, max: Option<&str>) -> bool {
    if min.is_none() && max.is_none() {
        return true;
    }
    if version.is_empty() {
        return false;
    }
    let satisfies = |bound: Option<&str>, allowed: fn(Ordering) -> bool| {
        bound.is_none_or(|bound| {
            let bound = version_numbers(bound);
            !bound.is_empty() && allowed(compare_prefix(version, &bound))
        })
    };
    satisfies(min, Ordering::is_ge) && satisfies(max, Ordering::is_le)
}

/// The visible announcements that were not shown yet, oldest first, at most
/// [`MAX_SHOWN_PER_POLL`].
pub fn select_new(
    announcements: Vec<Announcement>,
    shown: &ShownIds,
    now: DateTime<Utc>,
    version: &[u64],
) -> Vec<Announcement> {
    let mut ids = HashSet::new();
    let mut new: Vec<_> = announcements
        .into_iter()
        .filter(|announcement| {
            !shown.contains(&announcement.id)
                && announcement.is_visible(now, version)
                && ids.insert(announcement.id.clone())
        })
        .collect();
    // Undated announcements go last; the sort is stable, so the feed order breaks ties.
    new.sort_by_key(|announcement| announcement.created_at.unwrap_or(DateTime::<Utc>::MAX_UTC));
    new.truncate(MAX_SHOWN_PER_POLL);
    new
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
