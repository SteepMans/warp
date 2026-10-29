//! Ward (OSS build) workspace hooks. `view.rs` only calls into these methods so the upstream file
//! stays easy to merge.

use warpui::{AppContext, EntityId, ViewContext};

use super::Workspace;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModelEvent;
use crate::ward::agent_state;

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
}
