//! Claude subscription usage in the Ward header: `5h 2% · 7d 60%`.
//!
//! `GET https://api.anthropic.com/api/oauth/usage` with the OAuth token Claude Code keeps in
//! `~/.claude/.credentials.json` (`claudeAiOauth.accessToken`). The file is read again on every
//! poll because Claude Code refreshes the token in it. Polls run once on start and every
//! [`usage::POLL_INTERVAL`], backing off on failures. Without the file, without a token or on
//! 401/403 the chip is hidden. The token is never logged or shown. The user can turn the chip
//! off on the Third party CLI agents settings page.

pub mod model;
pub mod usage;

use std::path::PathBuf;

use warp_core::user_preferences::GetUserPreferences as _;
use warpui::AppContext;

/// Private user preferences key of the settings toggle; the chip is on unless it is "false".
const ENABLED_PREFERENCE_KEY: &str = "WardClaudeUsageChipEnabled";

/// `~/.claude/.credentials.json`.
pub fn credentials_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join(".credentials.json"))
}

/// Whether the user wants the usage chip (Third party CLI agents page). On by default.
pub fn is_enabled_by_user(app: &AppContext) -> bool {
    crate::features::is_fork_oss()
        && app
            .private_user_preferences()
            .read_value(ENABLED_PREFERENCE_KEY)
            .ok()
            .flatten()
            .is_none_or(|value| value != "false")
}

pub fn set_enabled_by_user(enabled: bool, app: &AppContext) {
    if let Err(err) = app
        .private_user_preferences()
        .write_value(ENABLED_PREFERENCE_KEY, enabled.to_string())
    {
        log::warn!("Failed to save the Ward Claude usage setting: {err:?}");
    }
}
