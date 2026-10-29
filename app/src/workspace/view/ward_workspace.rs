//! Ward (OSS build) workspace hooks. `view.rs` only calls into these methods so the upstream file
//! stays easy to merge.

use std::collections::HashSet;

use warpui::{AppContext, EntityId, SingletonEntity, ViewContext};

use super::Workspace;
use crate::features::FeatureFlag;
use crate::tab::{SelectedTabColor, TabData};
use crate::terminal::cli_agent_sessions::{CLIAgentSessionsModel, CLIAgentSessionsModelEvent};
use crate::ward::agent_state;
use crate::ward::project_groups::{self, TabObservation};
use crate::workspace::WorkspaceRegistry;
use crate::workspace::tab_group::TabGroup;

/// Learns project groups from, and auto-places tabs in, every window. Runs whenever the app
/// state is saved, which covers tab and group changes, working directory changes and newly
/// detected agent sessions.
pub(crate) fn ward_sync_project_groups_in_all_windows(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || !FeatureFlag::GroupedTabs.is_enabled() {
        return;
    }
    let workspaces = WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx);
    let mut alive = HashSet::new();
    for (_, workspace) in &workspaces {
        workspace.update(ctx, |workspace, ctx| {
            alive.extend(workspace.tabs.iter().map(|tab| tab.pane_group.id()));
            workspace.ward_sync_project_groups(ctx);
        });
    }
    project_groups::retain_tabs(|id| alive.contains(&id));
}

impl Workspace {
    /// Terminal views of the active tab, in layout order.
    fn ward_active_tab_terminal_ids(&self, app: &AppContext) -> Vec<EntityId> {
        self.tabs
            .get(self.active_tab_index)
            .map(|tab| {
                tab.pane_group
                    .as_ref(app)
                    .visible_terminal_views(app)
                    .iter()
                    .map(|view| view.id())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A completed agent counts as seen once its tab is the active tab.
    pub(super) fn ward_mark_active_tab_seen(&self, ctx: &mut ViewContext<Self>) {
        if !crate::features::is_fork_oss() {
            return;
        }
        let mut changed = false;
        for terminal_view_id in self.ward_active_tab_terminal_ids(ctx) {
            changed |= agent_state::mark_seen(terminal_view_id);
        }
        if changed {
            ctx.notify();
        }
    }

    pub(super) fn ward_handle_cli_agent_sessions_event(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        if !crate::features::is_fork_oss() {
            return;
        }
        if let CLIAgentSessionsModelEvent::StatusChanged {
            terminal_view_id, ..
        } = event
            && self
                .ward_active_tab_terminal_ids(ctx)
                .contains(terminal_view_id)
        {
            agent_state::mark_seen(*terminal_view_id);
        }
    }

    /// Project keys of a tab, from its agent pane if it has one, else its first terminal.
    /// Outside a git repo only agent panes fall back to the working directory: a plain shell
    /// wandering through directories would otherwise teach the map every folder it visits.
    fn ward_tab_project_keys(tab: &TabData, app: &AppContext) -> Vec<String> {
        let terminal_views = tab.pane_group.as_ref(app).visible_terminal_views(app);
        let sessions = CLIAgentSessionsModel::as_ref(app);
        let agent_view = terminal_views
            .iter()
            .find(|view| sessions.session(view.id()).is_some());
        let Some(terminal_view) = agent_view.or_else(|| terminal_views.first()) else {
            return Vec::new();
        };
        let terminal_view = terminal_view.as_ref(app);
        let remote_control_project = terminal_view
            .claude_resume_info()
            .and_then(|info| project_groups::remote_control_project(&info.command));
        let directory = terminal_view
            .current_local_repo_path()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| {
                agent_view
                    .is_some()
                    .then(|| terminal_view.pwd_if_local(app))
                    .flatten()
            });
        project_groups::project_keys(remote_control_project.as_deref(), directory.as_deref())
    }

    fn ward_sync_project_groups(&mut self, ctx: &mut ViewContext<Self>) {
        let observations: Vec<(EntityId, TabObservation)> = self
            .tabs
            .iter()
            .map(|tab| {
                let group_name = tab
                    .group_id
                    .and_then(|group_id| self.tab_groups.get(&group_id))
                    .and_then(|group| group.name.clone());
                (
                    tab.pane_group.id(),
                    TabObservation {
                        keys: Self::ward_tab_project_keys(tab, ctx),
                        group_id: tab.group_id,
                        group_name,
                    },
                )
            })
            .collect();
        let moves: Vec<(EntityId, String)> = observations
            .into_iter()
            .filter_map(|(pane_group_id, observation)| {
                project_groups::observe_tab(pane_group_id, observation, ctx)
                    .map(|group| (pane_group_id, group))
            })
            .collect();
        for (pane_group_id, group) in moves {
            self.ward_move_tab_to_named_group(pane_group_id, &group, ctx);
        }
    }

    /// Moves a tab to the end of the group with the given name, creating the group when no
    /// group has that name any more. Unlike the menu actions, this never moves focus.
    fn ward_move_tab_to_named_group(
        &mut self,
        pane_group_id: EntityId,
        group_name: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(tab_index) = self
            .tabs
            .iter()
            .position(|tab| tab.pane_group.id() == pane_group_id)
        else {
            return;
        };
        let previous_group_id = self.tabs[tab_index].group_id;
        let existing_group_id = self
            .tab_groups
            .iter()
            .find(|(_, group)| {
                group
                    .name
                    .as_deref()
                    .is_some_and(|name| project_groups::same_group_name(name, group_name))
            })
            .map(|(group_id, _)| *group_id);
        if existing_group_id.is_some() && existing_group_id == previous_group_id {
            return;
        }

        let (group_id, target_index) = match existing_group_id {
            Some(group_id) => (
                group_id,
                self.index_after_group(group_id).unwrap_or(self.tabs.len()),
            ),
            None => {
                let mut group = TabGroup::new();
                group.name = Some(group_name.to_owned());
                group.color =
                    SelectedTabColor::Color(project_groups::auto_group_color(group_name));
                let group_id = group.id;
                self.tab_groups.insert(group_id, group);
                // A new single-member group stays where the tab is, past its old group so that
                // group stays contiguous, and out of the pinned region.
                let natural_index = previous_group_id
                    .and_then(|group_id| self.index_after_group(group_id))
                    .unwrap_or(tab_index);
                (
                    group_id,
                    self.clamp_to_unpinned_region(&self.tabs, natural_index),
                )
            }
        };

        self.tabs[tab_index].group_id = Some(group_id);
        self.tabs[tab_index].pinned = false;
        self.move_tab_to_index(tab_index, target_index, ctx);
        if self
            .tabs
            .get(self.active_tab_index)
            .is_some_and(|tab| tab.pane_group.id() == pane_group_id)
        {
            self.expand_tab_group(group_id, ctx);
        }
        if let Some(previous_group_id) = previous_group_id {
            self.prune_empty_tab_group(previous_group_id, ctx);
        }
        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }
}
