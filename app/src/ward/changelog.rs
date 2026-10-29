//! "What's new in Ward": the changelog of a self-published OSS release.
//!
//! CI publishes `<releases_base_url>/stable/<version>/changelog.json` next to the installer (see
//! `script/windows/publish_s3.ps1`) with the commit titles since the previous release, grouped
//! by their `Module:` prefix:
//!
//! ```json
//! {
//!   "version": "v0.2026.10.29.12.00.oss_00",
//!   "previous_version": "v0.2026.10.28.09.30.oss_00",
//!   "commit": "<sha>",
//!   "previous_commit": "<sha>",
//!   "date": "2026-10-29T12:00:00Z",
//!   "groups": [{ "module": "Tabs", "items": ["...", "..."] }]
//! }
//! ```
//!
//! The file is converted into Warp's [`Changelog`] so the existing changelog model and the
//! resource center's "What's new" section display it. The first launch of a version other than
//! the last one whose changelog was shown opens it once; a failed fetch (offline, no changelog
//! for this build) is skipped silently and retried on the next launch.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use channel_versions::{Changelog, MarkdownSection};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use warp_core::channel::ChannelState;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::AppContext;

use crate::autoupdate::fork_manifest;
use crate::changelog_model::ChangelogHeader;

/// Private user preferences key holding the version whose changelog was shown last.
const LAST_SEEN_VERSION_KEY: &str = "WardChangelogLastSeenVersion";

const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Every change of the fork, linked from the changelog section.
pub const ALL_CHANGES_URL: &str = "https://github.com/SteepMans/warp/commits/master";

/// Section heading for commit titles without a `Module:` prefix when CI did not group them.
const UNGROUPED_MODULE: &str = "Other";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct WardChangelog {
    pub version: String,
    #[serde(default)]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub previous_commit: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub groups: Vec<ChangelogGroup>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ChangelogGroup {
    #[serde(default)]
    pub module: String,
    #[serde(default)]
    pub items: Vec<String>,
}

/// Whether this build shows Ward's changelog: the OSS build with a releases URL.
pub fn is_enabled() -> bool {
    crate::features::is_fork_oss() && fork_manifest::stable_dir_url().is_some()
}

/// URL of the changelog published for the given version.
pub fn changelog_url(version: &str) -> Option<String> {
    fork_manifest::stable_dir_url().map(|stable_dir| format!("{stable_dir}/{version}/changelog.json"))
}

/// Fetches the changelog of the running version and converts it for the changelog model.
pub async fn fetch_current(client: &http_client::Client) -> Result<Changelog> {
    let Some(version) = ChannelState::app_version() else {
        bail!("This build has no version");
    };
    let Some(url) = changelog_url(version) else {
        bail!("No releases URL configured for this build");
    };
    let changelog: WardChangelog = client
        .get(&url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("Failed to parse changelog.json")?;
    log::info!("Received Ward changelog for {version}");
    Ok(to_warp_changelog(&changelog, version))
}

/// Converts the published changelog into Warp's changelog shape: everything goes into the
/// "New features" section, the only one the "What's new" view renders with a heading of its
/// own, as one bold line per module followed by its items.
pub fn to_warp_changelog(changelog: &WardChangelog, running_version: &str) -> Changelog {
    let date = changelog
        .date
        .as_deref()
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
        .unwrap_or_else(|| Utc::now().fixed_offset());
    Changelog {
        date,
        sections: Vec::new(),
        markdown_sections: vec![MarkdownSection {
            title: ChangelogHeader::NewFeatures.to_string(),
            markdown: render_markdown(changelog, running_version),
        }],
        image_url: None,
        oz_updates: Vec::new(),
        tui_updates: Vec::new(),
    }
}

/// Markdown body of the changelog.
pub fn render_markdown(changelog: &WardChangelog, running_version: &str) -> String {
    let version = if changelog.version.trim().is_empty() {
        running_version
    } else {
        changelog.version.trim()
    };
    let mut markdown = format!(
        "**What's new in {} {}**\n\n",
        crate::features::product_name(),
        escape_markdown(version)
    );
    let groups: Vec<(&str, Vec<&str>)> = changelog
        .groups
        .iter()
        .map(|group| {
            let module = group.module.trim();
            let items = group
                .items
                .iter()
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            (if module.is_empty() { UNGROUPED_MODULE } else { module }, items)
        })
        .filter(|(_, items)| !items.is_empty())
        .collect();
    if groups.is_empty() {
        markdown.push_str("* No notable changes this release\n");
        return markdown;
    }
    for (index, (module, items)) in groups.iter().enumerate() {
        if index > 0 {
            markdown.push('\n');
        }
        markdown.push_str(&format!("**{}**\n", escape_markdown(module)));
        for item in items {
            markdown.push_str(&format!("* {}\n", escape_markdown(item)));
        }
    }
    markdown
}

/// Escapes characters that would otherwise turn commit titles into emphasis, code or links.
fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Whether the changelog of `version` was already shown, i.e. it is the last seen version.
pub fn has_been_shown(version: &str, app: &AppContext) -> bool {
    app.private_user_preferences()
        .read_value(LAST_SEEN_VERSION_KEY)
        .ok()
        .flatten()
        .is_some_and(|last_seen| last_seen == version)
}

/// Remembers `version` as the last version whose changelog was shown.
pub fn mark_shown(version: &str, app: &AppContext) {
    if let Err(err) = app
        .private_user_preferences()
        .write_value(LAST_SEEN_VERSION_KEY, version.to_owned())
    {
        log::warn!("Failed to save the last seen Ward changelog version: {err:?}");
    }
}

#[cfg(test)]
#[path = "changelog_tests.rs"]
mod tests;
