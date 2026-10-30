use warpui::AppContext;

use crate::context_chips::display_chip::GitLineChanges;
use crate::context_chips::{ContextChipKind, git_line_changes_from_chips};
use crate::terminal::{TerminalModel, TerminalView};

/// The terminal model fields a vertical tabs row shows, read under a single lock of the model.
/// A row needs several of them on every render, and every lock competes with the PTY thread of
/// a terminal that is streaming output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRowModelState {
    /// See [`TerminalView::terminal_title_from_shell`].
    pub title_from_shell: String,
    /// See [`TerminalView::is_long_running_and_user_controlled`].
    pub is_long_running_and_user_controlled: bool,
    /// See [`TerminalView::last_completed_command_text`].
    pub last_completed_command_text: Option<String>,
    /// See [`TerminalModel::is_cloud_agent_conversation`].
    pub is_cloud_agent_conversation: bool,
}

fn title_from_shell(model: &TerminalModel) -> String {
    let fallback_title = model.shell_launch_state().display_name().to_owned();
    model
        .terminal_title()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(fallback_title)
}

pub(super) fn is_long_running_and_user_controlled(model: &TerminalModel) -> bool {
    let active_block = model.block_list().active_block();
    active_block.is_active_and_long_running()
        && !active_block.is_agent_driving_command()
        && !model.is_read_only()
}

fn last_completed_command_text(model: &TerminalModel) -> Option<String> {
    model.block_list().blocks().iter().rev().find_map(|block| {
        if block.finished()
            && !block.is_background()
            && !block.is_static()
            && !block.is_hidden()
            && !block.is_in_band_command_block()
            && (block.bootstrap_stage().is_done() || block.is_restored())
        {
            let cmd = block.command_to_string();
            if cmd.trim().is_empty() {
                None
            } else {
                Some(cmd)
            }
        } else {
            None
        }
    })
}

impl TerminalView {
    fn prompt_chip_value(&self, chip_kind: &ContextChipKind, ctx: &AppContext) -> Option<String> {
        self.current_prompt
            .as_ref(ctx)
            .latest_chip_value(chip_kind, ctx)
            .map(|v| v.to_string())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn display_working_directory(&self, ctx: &AppContext) -> Option<String> {
        let raw = self
            .prompt_chip_value(&ContextChipKind::WorkingDirectory, ctx)
            .or_else(|| self.pwd())?;
        let home_dir = self
            .active_block_session_id()
            .and_then(|session_id| self.sessions.as_ref(ctx).get(session_id))
            .and_then(|session| session.home_dir().map(str::to_owned));
        Some(warp_util::path::user_friendly_path(&raw, home_dir.as_deref()).to_string())
    }

    pub fn terminal_title_from_shell(&self) -> String {
        title_from_shell(&self.model.lock())
    }

    /// Everything of the terminal model a vertical tabs row needs, under one lock.
    pub fn row_model_state(&self) -> TerminalRowModelState {
        let model = self.model.lock();
        TerminalRowModelState {
            title_from_shell: title_from_shell(&model),
            is_long_running_and_user_controlled: is_long_running_and_user_controlled(&model),
            last_completed_command_text: last_completed_command_text(&model),
            is_cloud_agent_conversation: model.is_cloud_agent_conversation(),
        }
    }

    pub fn current_git_branch(&self, ctx: &AppContext) -> Option<String> {
        self.prompt_chip_value(&ContextChipKind::ShellGitBranch, ctx)
            .or_else(|| {
                self.git_status_metadata(ctx)
                    .map(|metadata| metadata.current_branch_name.clone())
                    .filter(|branch| !branch.trim().is_empty())
            })
    }

    pub fn last_completed_command_text(&self) -> Option<String> {
        last_completed_command_text(&self.model.lock())
    }

    pub fn terminal_title_text(&self) -> String {
        if !self.terminal_title.trim().is_empty() {
            return self.terminal_title.clone();
        }
        self.terminal_title_from_shell()
    }

    pub fn current_pull_request_url(&self, ctx: &AppContext) -> Option<String> {
        self.current_prompt
            .as_ref(ctx)
            .latest_chip_value(&ContextChipKind::GithubPullRequest, ctx)
            .map(|v| v.to_string())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn current_diff_line_changes(&self, ctx: &AppContext) -> Option<GitLineChanges> {
        // Prefer the externally-updated GitRepoStatusModel (local filesystem
        // watcher or remote daemon push receiver) over parsing the raw shell
        // chip output. This matches the preference order used by the prompt
        // chip display (display.rs) and agent footer (chips.rs).
        let from_model = self
            .git_status_metadata(ctx)
            .map(|metadata| GitLineChanges::from_diff_stats(&metadata.stats_against_head));

        from_model
            .or_else(|| {
                git_line_changes_from_chips(&self.current_prompt.as_ref(ctx).agent_view_chips(ctx))
            })
            .filter(|line_changes| {
                line_changes.files_changed > 0
                    || line_changes.lines_added > 0
                    || line_changes.lines_removed > 0
            })
    }
}
