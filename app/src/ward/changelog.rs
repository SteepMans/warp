//! "What's new in Ward": the changelog of a self-published OSS release.
//!
//! CI publishes `<releases_base_url>/stable/<version>/changelog.json` next to the installer (see
//! `script/windows/publish_s3.ps1`). Release notes are written by people: every user-visible
//! change adds a fragment `changes/<slug>.md` (format in `changes/README.md`), and CI collects
//! the fragments added or edited since the previous release into `notes`, uploading their
//! pictures next to the installer:
//!
//! ```json
//! {
//!   "version": "v0.2026.10.29.12.00.oss_00",
//!   "previous_version": "v0.2026.10.28.09.30.oss_00",
//!   "commit": "<sha>",
//!   "previous_commit": "<sha>",
//!   "date": "2026-10-29T12:00:00Z",
//!   "image_url": "https://.../images/x.png",
//!   "notes": [{ "section": "new", "title": "...", "text": "...", "image_url": "https://..." }]
//! }
//! ```
//!
//! A release without fragments falls back to `groups`: commit titles since the previous
//! release grouped by their `Module:` prefix (`[{ "module": "Tabs", "items": ["..."] }]`).
//!
//! The file is converted into Warp's [`Changelog`] so the existing changelog model and the
//! resource center's "What's new" section display it. That view renders markdown without
//! inline images and shows a single picture (`image_url`) above the first section, so the
//! first note picture is shown there and the other notes link to theirs. The first launch of a
//! version other than the last one whose changelog was shown opens it once; a failed fetch
//! (offline, no changelog for this build) is skipped silently and retried on the next launch.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use channel_versions::{Changelog, MarkdownSection};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use warp_core::channel::ChannelState;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::AppContext;

use crate::autoupdate::fork_manifest;

/// Private user preferences key holding the version whose changelog was shown last.
const LAST_SEEN_VERSION_KEY: &str = "WardChangelogLastSeenVersion";

const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Every change of the fork, linked from the changelog section.
pub const ALL_CHANGES_URL: &str = "https://github.com/SteepMans/warp/commits/master";

/// Section heading for commit titles without a `Module:` prefix when CI did not group them.
const UNGROUPED_MODULE: &str = "Other";

/// Headings of the "What's new" sections in Ward (see `ChangelogHeader`'s `Display`).
pub const NEW_SECTION_TITLE: &str = "Новое";
pub const IMPROVED_SECTION_TITLE: &str = "Улучшения";
pub const FIXED_SECTION_TITLE: &str = "Исправления";

/// Link text under a note whose picture is not the one shown above the sections.
const IMAGE_LINK_TEXT: &str = "Посмотреть картинку";

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
    /// Picture shown above the sections; CI sets it to the first note picture.
    #[serde(default)]
    pub image_url: Option<String>,
    /// Human-written release notes; when empty, `groups` are shown instead.
    #[serde(default)]
    pub notes: Vec<ChangelogNote>,
    #[serde(default)]
    pub groups: Vec<ChangelogGroup>,
}

/// One release note from a `changes/*.md` fragment.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ChangelogNote {
    /// `new`, `improved` or `fixed`; anything else counts as `improved`.
    #[serde(default)]
    pub section: String,
    #[serde(default)]
    pub title: String,
    /// Markdown written for the user.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub image_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoteSection {
    New,
    Improved,
    Fixed,
}

impl NoteSection {
    const ALL: [NoteSection; 3] = [NoteSection::New, NoteSection::Improved, NoteSection::Fixed];

    fn parse(section: &str) -> Self {
        match section.trim().to_ascii_lowercase().as_str() {
            "new" => NoteSection::New,
            "fixed" => NoteSection::Fixed,
            _ => NoteSection::Improved,
        }
    }

    fn title(self) -> &'static str {
        match self {
            NoteSection::New => NEW_SECTION_TITLE,
            NoteSection::Improved => IMPROVED_SECTION_TITLE,
            NoteSection::Fixed => FIXED_SECTION_TITLE,
        }
    }
}

/// Heading of the "What's new" section a note with this `section` goes to.
pub fn section_title(section: &str) -> &'static str {
    NoteSection::parse(section).title()
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
    let changelog = fetch_for_version(client, version).await?;
    log::info!("Received Ward changelog for {version}");
    Ok(to_warp_changelog(&changelog, version))
}

/// Fetches the changelog published for the given version as is.
pub async fn fetch_for_version(
    client: &http_client::Client,
    version: &str,
) -> Result<WardChangelog> {
    let Some(url) = changelog_url(version) else {
        bail!("No releases URL configured for this build");
    };
    client
        .get(&url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("Failed to parse changelog.json")
}

/// Converts the published changelog into Warp's changelog shape.
///
/// Notes become the sections «Новое», «Улучшения» and «Исправления» (only non-empty ones), each
/// note a bold title followed by its text. Without notes everything goes into «Новое», one bold
/// line per module followed by its items.
pub fn to_warp_changelog(changelog: &WardChangelog, running_version: &str) -> Changelog {
    let date = changelog
        .date
        .as_deref()
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
        .unwrap_or_else(|| Utc::now().fixed_offset());
    let notes: Vec<&ChangelogNote> = changelog
        .notes
        .iter()
        .filter(|note| !note.title.trim().is_empty() || !note.text.trim().is_empty())
        .collect();
    let (markdown_sections, image_url) = if notes.is_empty() {
        let section = MarkdownSection {
            title: NEW_SECTION_TITLE.to_owned(),
            markdown: render_markdown(changelog, running_version),
        };
        (vec![section], None)
    } else {
        let image_url = safe_image_url(changelog.image_url.as_deref()).or_else(|| {
            notes
                .iter()
                .find_map(|note| safe_image_url(note.image_url.as_deref()))
        });
        (render_note_sections(&notes, image_url), image_url)
    };
    Changelog {
        date,
        sections: Vec::new(),
        markdown_sections,
        image_url: image_url.map(ToOwned::to_owned),
        oz_updates: Vec::new(),
        tui_updates: Vec::new(),
    }
}

/// A picture URL that is safe to load and to put into a markdown link.
fn safe_image_url(url: Option<&str>) -> Option<&str> {
    let url = url?.trim();
    let is_http = url.starts_with("https://") || url.starts_with("http://");
    let is_plain = !url
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>'));
    (is_http && is_plain).then_some(url)
}

/// One markdown section per non-empty note section, notes in their published order. The
/// picture already shown above the sections (`shown_image`) is not linked again.
fn render_note_sections(
    notes: &[&ChangelogNote],
    shown_image: Option<&str>,
) -> Vec<MarkdownSection> {
    NoteSection::ALL
        .iter()
        .filter_map(|&section| {
            let markdown = notes
                .iter()
                .filter(|note| NoteSection::parse(&note.section) == section)
                .map(|note| render_note(note, shown_image))
                .collect::<Vec<_>>()
                .join("\n");
            (!markdown.is_empty()).then(|| MarkdownSection {
                title: section.title().to_owned(),
                markdown,
            })
        })
        .collect()
}

/// A note as markdown: the bold title, the text and a link to the note's picture unless it is
/// the one already shown. The text is the author's markdown, the title is plain text.
fn render_note(note: &ChangelogNote, shown_image: Option<&str>) -> String {
    let mut markdown = String::new();
    let title = note.title.trim();
    if !title.is_empty() {
        markdown.push_str(&format!("**{}**\n", escape_markdown(title)));
    }
    let text = note.text.trim();
    if !text.is_empty() {
        markdown.push_str(text);
        markdown.push('\n');
    }
    if let Some(url) = safe_image_url(note.image_url.as_deref())
        && Some(url) != shown_image
    {
        markdown.push_str(&format!("[{IMAGE_LINK_TEXT}]({url})\n"));
    }
    markdown
}

/// Markdown body of a changelog without notes: commit titles grouped by module.
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
