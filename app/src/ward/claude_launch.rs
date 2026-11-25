//! Environment of Claude Code tabs Ward starts itself: the "+" menu presets, agents started from
//! Chimney cards and restored Claude sessions (`claude ... --resume <id>`).
//!
//! With the "Claude Code fullscreen renderer" setting on, these tabs get
//! `CLAUDE_CODE_NO_FLICKER=1`. The variable goes into the pane's shell environment when the
//! pane is created, so it works the same in pwsh, cmd and bash, and the command text stays as
//! it was (no shell-specific prefix, nothing changes in the saved resume command).

use std::collections::HashMap;
use std::ffi::OsString;

use warp_core::user_preferences::GetUserPreferences as _;
use warpui::AppContext;

pub const NO_FLICKER_ENV_VAR: &str = "CLAUDE_CODE_NO_FLICKER";

/// Private user preferences key of the settings toggle; off unless it is "true".
const NO_FLICKER_PREFERENCE_KEY: &str = "WardClaudeNoFlicker";

/// Whether the user turned the Claude Code fullscreen renderer on. Off by default.
pub fn is_no_flicker_enabled(app: &AppContext) -> bool {
    crate::features::is_fork_oss()
        && app
            .private_user_preferences()
            .read_value(NO_FLICKER_PREFERENCE_KEY)
            .ok()
            .flatten()
            .is_some_and(|value| value == "true")
}

pub fn set_no_flicker_enabled(enabled: bool, app: &AppContext) {
    if let Err(err) = app
        .private_user_preferences()
        .write_value(NO_FLICKER_PREFERENCE_KEY, enabled.to_string())
    {
        log::warn!("Failed to save the Ward Claude Code renderer setting: {err:?}");
    }
}

/// Whether a command Ward runs in a new tab starts Claude Code: its first word is `claude`
/// (also `claude.exe`, `claude.cmd`, a path to one, or quoted).
pub fn is_claude_command(command: &str) -> bool {
    let command = command.trim_start();
    let program = match command.chars().next() {
        Some(quote @ ('\'' | '"')) => command[1..].split(quote).next().unwrap_or_default(),
        _ => command.split_whitespace().next().unwrap_or_default(),
    };
    let file_name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let file_name = file_name.to_ascii_lowercase();
    let stem = file_name
        .strip_suffix(".exe")
        .or_else(|| file_name.strip_suffix(".cmd"))
        .or_else(|| file_name.strip_suffix(".ps1"))
        .unwrap_or(&file_name);
    stem == "claude"
}

/// Extra environment of the pane that will run `command`.
pub fn launch_env_vars(command: &str, no_flicker: bool) -> HashMap<OsString, OsString> {
    let mut env_vars = HashMap::new();
    if no_flicker && is_claude_command(command) {
        env_vars.insert(OsString::from(NO_FLICKER_ENV_VAR), OsString::from("1"));
    }
    env_vars
}

/// [`launch_env_vars`] with the user's setting.
pub fn env_vars_for_command(command: &str, app: &AppContext) -> HashMap<OsString, OsString> {
    launch_env_vars(command, is_no_flicker_enabled(app))
}

/// Set in the shell of a tab Ward reopens with its own CLI agent session, to that session's id.
///
/// The shell profile runs before Ward types the resume command. A profile hook that starts an
/// agent in the tab's directory (a pending launch left for "the next tab opened in this repo")
/// would otherwise fire in every reopened tab of that directory at once, and all of them would
/// show the same conversation instead of their own. The hook skips a tab that has this variable.
pub const RESUME_SESSION_ENV_VAR: &str = "WARD_RESUME_SESSION";

/// The session id a saved resume command reopens: `claude ... --resume <id>` or
/// `codex resume ... <id>`.
pub fn resumed_session_id(command: &str) -> Option<&str> {
    if is_claude_command(command) {
        crate::terminal::cli_agent_sessions::claude_resume::session_id_from_resume_command(command)
    } else {
        crate::ward::codex_threads::resumed_id(command)
    }
}

/// Extra environment of a reopened tab that runs the resume command `command`: the launch
/// environment plus [`RESUME_SESSION_ENV_VAR`] with the tab's own session id.
pub fn resume_env_vars(command: &str, no_flicker: bool) -> HashMap<OsString, OsString> {
    let mut env_vars = launch_env_vars(command, no_flicker);
    if let Some(session_id) = resumed_session_id(command) {
        env_vars.insert(
            OsString::from(RESUME_SESSION_ENV_VAR),
            OsString::from(session_id),
        );
    }
    env_vars
}

/// [`resume_env_vars`] with the user's setting.
pub fn env_vars_for_resume(command: &str, app: &AppContext) -> HashMap<OsString, OsString> {
    resume_env_vars(command, is_no_flicker_enabled(app))
}

#[cfg(test)]
#[path = "claude_launch_tests.rs"]
mod tests;
