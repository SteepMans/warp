//! Codex plan usage in the Ward header: `Codex 5h 10% · 7d 30%`.
//!
//! Codex CLI records the plan's rate limits in its session rollouts
//! (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, `token_count` events). Ward reads the last
//! report from the most recently written rollout: no network and no credentials. The data is as
//! fresh as the last Codex turn, so windows past their reset are hidden and the tooltip says
//! when Codex reported it. The user can turn the chip off on the Third party CLI agents
//! settings page.

pub mod model;
pub mod rollout;

use std::path::PathBuf;

use warp_core::user_preferences::GetUserPreferences as _;
use warpui::AppContext;

/// Private user preferences key of the settings toggle; the chip is on unless it is "false".
const ENABLED_PREFERENCE_KEY: &str = "WardCodexUsageChipEnabled";

/// `$CODEX_HOME/sessions`, by default `~/.codex/sessions`.
pub fn sessions_dir() -> Option<PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))?;
    Some(codex_home.join("sessions"))
}

/// Whether the user wants the Codex usage chip (Third party CLI agents page). On by default.
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
        log::warn!("Failed to save the Ward Codex usage setting: {err:?}");
    }
}
