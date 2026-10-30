//! Settings → Updates in Ward: what the update state machine is doing, in words, and what the
//! user can do about it.
//!
//! The page reads [`AutoupdateState`]; this module holds the parts that do not need a view:
//! the status line for every [`AutoupdateStage`], the "Check now" decision and the "Last
//! checked" text. "Check now" always has a visible outcome: a running check or download is
//! shown instead of being silently dropped by the update queue, a check that finished a few
//! seconds ago is not repeated, anything else starts a manual check.

use channel_versions::VersionInfo;
use chrono::{DateTime, Local};
use warpui::{AppContext, SingletonEntity as _};

use super::changelog::{self, WardChangelog};
use crate::autoupdate::{AutoupdateStage, AutoupdateState};

/// A check that finished less than this long ago is not repeated by "Check now".
pub const RECENT_CHECK_SECONDS: i64 = 5;

/// How many release notes of the latest version the page lists.
pub const MAX_NOTES: usize = 5;

/// How the status line is colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    /// Something is running; the user only has to wait.
    Busy,
    /// Nothing to do.
    Idle,
    /// An update waits for the user.
    Ready,
    /// The last attempt failed.
    Error,
}

/// The button next to the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusAction {
    /// Relaunch into the downloaded update.
    InstallAndRestart,
    /// Relaunch into an update that is already installed.
    Restart,
    /// Check for updates again.
    Retry,
}

impl StatusAction {
    pub fn label(self) -> &'static str {
        match self {
            StatusAction::InstallAndRestart => "Install and restart",
            StatusAction::Restart => "Restart",
            StatusAction::Retry => "Retry",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    pub text: String,
    pub kind: StatusKind,
    pub action: Option<StatusAction>,
}

impl UpdateStatus {
    fn new(text: impl Into<String>, kind: StatusKind, action: Option<StatusAction>) -> Self {
        Self {
            text: text.into(),
            kind,
            action,
        }
    }
}

/// What the page knows about the last check, besides the stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckInfo<'a> {
    /// Whether any check has finished since launch.
    pub checked: bool,
    /// Why the last check or download failed.
    pub error: Option<&'a str>,
    /// The latest version from the last successful check.
    pub latest_version: Option<&'a str>,
    /// Whether update checks run yet (they start after launch finishes).
    pub polling_started: bool,
}

/// The status line for the current stage.
pub fn status(stage: &AutoupdateStage, info: CheckInfo<'_>) -> UpdateStatus {
    match stage {
        AutoupdateStage::NoUpdateAvailable => match info.error {
            Some(error) => UpdateStatus::new(
                format!("Error: {error}"),
                StatusKind::Error,
                Some(StatusAction::Retry),
            ),
            None if info.checked => UpdateStatus::new("Up to date", StatusKind::Idle, None),
            None if !info.polling_started => UpdateStatus::new(
                "Update checks start once Ward finishes launching",
                StatusKind::Idle,
                None,
            ),
            None => UpdateStatus::new("Not checked yet", StatusKind::Idle, None),
        },
        AutoupdateStage::CheckingForUpdate => {
            UpdateStatus::new("Checking for updates…", StatusKind::Busy, None)
        }
        AutoupdateStage::DownloadingUpdate => {
            let text = match info.latest_version {
                Some(version) => format!("Downloading {version}…"),
                None => "Downloading the update…".to_owned(),
            };
            UpdateStatus::new(text, StatusKind::Busy, None)
        }
        AutoupdateStage::UnableToUpdateToNewVersion { new_version } => UpdateStatus::new(
            format!(
                "{} is available, but Ward has no permission to install it",
                new_version.version
            ),
            StatusKind::Error,
            Some(StatusAction::Retry),
        ),
        AutoupdateStage::UpdateReady { new_version, .. } => UpdateStatus::new(
            format!("Ready to install: {}", new_version.version),
            StatusKind::Ready,
            Some(StatusAction::InstallAndRestart),
        ),
        AutoupdateStage::Updating { new_version, .. } => UpdateStatus::new(
            format!("Installing {} and restarting…", new_version.version),
            StatusKind::Busy,
            None,
        ),
        AutoupdateStage::UnableToLaunchNewVersion { new_version } => UpdateStatus::new(
            format!("Error: could not restart into {}", new_version.version),
            StatusKind::Error,
            Some(StatusAction::Retry),
        ),
        AutoupdateStage::UpdatedPendingRestart { new_version } => UpdateStatus::new(
            format!(
                "{} is installed, restart Ward to use it",
                new_version.version
            ),
            StatusKind::Ready,
            Some(StatusAction::Restart),
        ),
    }
}

/// What "Check now" does in the current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckNowDecision {
    /// A check, download or relaunch is running; its status is already on the page.
    InProgress,
    /// An update is installed and waits for a restart; another check cannot change anything.
    RestartPending,
    /// The last check finished a moment ago; its result is on the page.
    RecentlyChecked,
    /// Start a manual check.
    Start,
}

impl CheckNowDecision {
    /// The note shown under the status after clicking "Check now", if the status alone does
    /// not explain what happened.
    pub fn note(self, polling_started: bool) -> Option<&'static str> {
        match self {
            CheckNowDecision::InProgress => Some("Already in progress, see the status above"),
            CheckNowDecision::RestartPending => {
                Some("An update is already installed, restart to use it")
            }
            CheckNowDecision::RecentlyChecked => Some("Checked a few seconds ago"),
            CheckNowDecision::Start if !polling_started => {
                Some("The check will run once Ward finishes launching")
            }
            CheckNowDecision::Start => None,
        }
    }
}

pub fn decide_check_now(
    stage: &AutoupdateStage,
    last_check_finished_at: Option<DateTime<Local>>,
    now: DateTime<Local>,
) -> CheckNowDecision {
    match stage {
        AutoupdateStage::CheckingForUpdate
        | AutoupdateStage::DownloadingUpdate
        | AutoupdateStage::Updating { .. } => CheckNowDecision::InProgress,
        AutoupdateStage::UpdatedPendingRestart { .. } => CheckNowDecision::RestartPending,
        AutoupdateStage::NoUpdateAvailable
        | AutoupdateStage::UnableToUpdateToNewVersion { .. }
        | AutoupdateStage::UpdateReady { .. }
        | AutoupdateStage::UnableToLaunchNewVersion { .. } => {
            let recent = last_check_finished_at.is_some_and(|finished| {
                (now - finished).num_seconds() < RECENT_CHECK_SECONDS && finished <= now
            });
            if recent {
                CheckNowDecision::RecentlyChecked
            } else {
                CheckNowDecision::Start
            }
        }
    }
}

/// Runs "Check now": starts a manual check when [`decide_check_now`] says so.
pub fn check_now(app: &mut AppContext) -> CheckNowDecision {
    let handle = AutoupdateState::handle(app);
    handle.update(app, |state, ctx| {
        let decision =
            decide_check_now(state.stage(), state.last_check_finished_at(), Local::now());
        if decision == CheckNowDecision::Start {
            state.manually_check_for_update(ctx);
        }
        decision
    })
}

/// "Last checked 3 min ago" and friends.
pub fn last_checked_text(last: Option<DateTime<Local>>, now: DateTime<Local>) -> String {
    let Some(last) = last else {
        return "Not checked since launch".to_owned();
    };
    let seconds = (now - last).num_seconds().max(0);
    match seconds {
        0..60 => "Last checked just now".to_owned(),
        60..3600 => format!("Last checked {} min ago", seconds / 60),
        3600..86400 => format!("Last checked {} h ago", seconds / 3600),
        _ => format!("Last checked on {}", last.format("%Y-%m-%d %H:%M")),
    }
}

/// The version whose notes the page shows: the latest known one, or the running one before
/// the first check.
pub fn notes_version<'a>(
    latest: Option<&'a VersionInfo>,
    running: Option<&'a str>,
) -> Option<&'a str> {
    latest.map(|version| version.version.as_str()).or(running)
}

/// One line of the short "What's new" on the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteLine {
    pub section: &'static str,
    pub title: String,
    pub text: String,
}

/// The first [`MAX_NOTES`] notes of a release and how many were left out. A release without
/// notes lists its commit groups instead, one line per module.
pub fn short_notes(changelog: &WardChangelog) -> (Vec<NoteLine>, usize) {
    let lines: Vec<NoteLine> = if changelog
        .notes
        .iter()
        .any(|note| !note.title.trim().is_empty() || !note.text.trim().is_empty())
    {
        changelog
            .notes
            .iter()
            .filter(|note| !note.title.trim().is_empty() || !note.text.trim().is_empty())
            .map(|note| NoteLine {
                section: changelog::section_title(&note.section),
                title: note.title.trim().to_owned(),
                text: note.text.trim().to_owned(),
            })
            .collect()
    } else {
        changelog
            .groups
            .iter()
            .filter(|group| group.items.iter().any(|item| !item.trim().is_empty()))
            .map(|group| NoteLine {
                section: changelog::NEW_SECTION_TITLE,
                title: group.module.trim().to_owned(),
                text: group
                    .items
                    .iter()
                    .map(|item| item.trim())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>()
                    .join("; "),
            })
            .collect()
    };
    let hidden = lines.len().saturating_sub(MAX_NOTES);
    (lines.into_iter().take(MAX_NOTES).collect(), hidden)
}

/// "Released 2026-10-29 12:00 · commit abc1234" from the changelog, if it has either.
pub fn release_details(changelog: &WardChangelog) -> Option<String> {
    let date = changelog
        .date
        .as_deref()
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
        .map(|date| {
            format!(
                "Released {}",
                date.with_timezone(&Local).format("%Y-%m-%d %H:%M")
            )
        });
    let commit = changelog
        .commit
        .as_deref()
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
        .map(|commit| format!("commit {}", commit.chars().take(7).collect::<String>()));
    match (date, commit) {
        (Some(date), Some(commit)) => Some(format!("{date} · {commit}")),
        (Some(text), None) | (None, Some(text)) => Some(text),
        (None, None) => None,
    }
}

#[cfg(test)]
#[path = "updates_tests.rs"]
mod tests;
