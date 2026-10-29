//! Cross-window view of every CLI agent: the header attention counts, the jump-to-waiting-agent
//! order and the notification throttle.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use instant::Instant;
use warpui::{AppContext, EntityId, SingletonEntity, ViewHandle, WindowId};

use super::agent_state::{AgentSnapshot, AgentState, agent_snapshot};
use super::labels::{AgentLabels, agent_labels};
use crate::terminal::TerminalView;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::workspace::WorkspaceRegistry;

/// At most one desktop notification per agent in this window.
pub const NOTIFICATION_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Snapshots of all CLI agents in all windows, in window and tab order.
pub fn all_agents(app: &AppContext) -> Vec<AgentSnapshot> {
    if !crate::features::is_fork_oss() {
        return Vec::new();
    }
    WorkspaceRegistry::as_ref(app)
        .all_workspaces(app)
        .iter()
        .flat_map(|(_, workspace)| {
            workspace
                .as_ref(app)
                .tab_views()
                .flat_map(|pane_group| pane_group.as_ref(app).terminal_views(app))
                .collect::<Vec<_>>()
        })
        .filter_map(|terminal_view| agent_snapshot(terminal_view.id(), app))
        .collect()
}

/// One CLI agent with what the overview board, the agents palette and the footer show of it.
#[derive(Clone)]
pub struct AgentEntry {
    pub snapshot: AgentSnapshot,
    pub labels: AgentLabels,
    pub window_id: WindowId,
    pub terminal_view: ViewHandle<TerminalView>,
}

/// Every CLI agent of every window with its labels, in window and tab order. Windows whose
/// workspace is being updated right now are skipped (the framework hides a view while it
/// handles an action), so call this from render or from other views.
pub fn all_agent_entries(app: &AppContext) -> Vec<AgentEntry> {
    if !crate::features::is_fork_oss() {
        return Vec::new();
    }
    let sessions = CLIAgentSessionsModel::as_ref(app);
    let mut entries = Vec::new();
    for (window_id, workspace) in WorkspaceRegistry::as_ref(app).all_workspaces(app) {
        for pane_group in workspace.as_ref(app).tab_views() {
            for terminal_view in pane_group.as_ref(app).terminal_views(app) {
                let Some(snapshot) = agent_snapshot(terminal_view.id(), app) else {
                    continue;
                };
                let view = terminal_view.as_ref(app);
                let session = sessions.session(terminal_view.id());
                let directory = view
                    .current_local_repo_path()
                    .map(|path| path.to_string_lossy().into_owned())
                    .or_else(|| session.and_then(|session| session.session_context.cwd.clone()));
                let agent_name = session
                    .map(|session| session.agent.display_name())
                    .unwrap_or("Agent");
                let labels = agent_labels(
                    view.claude_resume_info().map(|info| info.command.as_str()),
                    directory.as_deref(),
                    snapshot.task.as_deref(),
                    agent_name,
                );
                entries.push(AgentEntry {
                    snapshot,
                    labels,
                    window_id,
                    terminal_view: terminal_view.clone(),
                });
            }
        }
    }
    entries
}

/// Agent counts by what they need from the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttentionCounts {
    pub waiting: usize,
    pub errors: usize,
    pub running: usize,
    /// Finished and not yet seen.
    pub done: usize,
}

impl AttentionCounts {
    pub fn from_states(states: impl IntoIterator<Item = AgentState>) -> Self {
        let mut counts = Self::default();
        for state in states {
            match state {
                state if state.is_waiting() => counts.waiting += 1,
                AgentState::Error => counts.errors += 1,
                state if state.is_running() => counts.running += 1,
                AgentState::DoneUnseen => counts.done += 1,
                _ => {}
            }
        }
        counts
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Agents that need the user, most urgent first: permissions, then questions, then errors;
/// within each, the one waiting longest first.
pub fn jump_order(agents: &[(EntityId, AgentState, Duration)]) -> Vec<EntityId> {
    let mut targets: Vec<_> = agents
        .iter()
        .filter_map(|(id, state, in_state)| state.jump_rank().map(|rank| (rank, *in_state, *id)))
        .collect();
    targets.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    targets.into_iter().map(|(_, _, id)| id).collect()
}

/// Next jump target after `current`: the most urgent agent, or the one after `current` when the
/// user is already on a target (so repeated presses walk the list).
pub fn next_jump_target(order: &[EntityId], current: Option<EntityId>) -> Option<EntityId> {
    let position = current.and_then(|current| order.iter().position(|id| *id == current));
    match position {
        Some(position) => order.get((position + 1) % order.len()).copied(),
        None => order.first().copied(),
    }
}

/// Status filter of the vertical tabs panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentStatusFilter {
    #[default]
    All,
    Waiting,
    Running,
    Errors,
}

impl AgentStatusFilter {
    pub const ALL: [Self; 4] = [Self::All, Self::Waiting, Self::Running, Self::Errors];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Waiting => "Waiting",
            Self::Running => "Running",
            Self::Errors => "Errors",
        }
    }

    pub fn matches(self, state: AgentState) -> bool {
        match self {
            Self::All => true,
            Self::Waiting => state.is_waiting(),
            Self::Running => state.is_running(),
            Self::Errors => state == AgentState::Error,
        }
    }

    /// How many of `states` the filter keeps.
    pub fn count(self, states: &[AgentState]) -> usize {
        states.iter().filter(|state| self.matches(**state)).count()
    }
}

#[derive(Default)]
struct JumpState {
    /// Where the first jump of a series started, for jumping back.
    return_to: Option<EntityId>,
}

thread_local! {
    static STATUS_FILTER: RefCell<AgentStatusFilter> = RefCell::new(AgentStatusFilter::All);
    static JUMP: RefCell<JumpState> = RefCell::new(JumpState::default());
    static LAST_NOTIFIED: RefCell<HashMap<EntityId, Instant>> = RefCell::new(HashMap::new());
}

pub fn status_filter() -> AgentStatusFilter {
    STATUS_FILTER.with(|filter| *filter.borrow())
}

pub fn set_status_filter(filter: AgentStatusFilter) {
    STATUS_FILTER.with(|current| *current.borrow_mut() = filter);
}

/// Picks the next agent to jump to from the focused terminal and remembers where the series
/// started.
pub fn begin_jump(current: Option<EntityId>, app: &AppContext) -> Option<EntityId> {
    let agents: Vec<_> = all_agents(app)
        .iter()
        .map(|agent| (agent.terminal_view_id, agent.state, agent.in_state))
        .collect();
    let order = jump_order(&agents);
    let target = next_jump_target(&order, current)?;
    let jumping_from_target = current.is_some_and(|current| order.contains(&current));
    JUMP.with(|jump| {
        let mut jump = jump.borrow_mut();
        if !jumping_from_target || jump.return_to.is_none() {
            jump.return_to = current.filter(|current| *current != target);
        }
    });
    Some(target)
}

/// Where to jump back to, consuming it.
pub fn take_jump_back_target() -> Option<EntityId> {
    JUMP.with(|jump| jump.borrow_mut().return_to.take())
}

/// Whether a desktop notification for this agent is due: only for waiting and error states,
/// and at most once per [`NOTIFICATION_INTERVAL`].
pub fn notification_due(state: AgentState, last: Option<Instant>, now: Instant) -> bool {
    state.needs_attention()
        && last.is_none_or(|last| now.saturating_duration_since(last) >= NOTIFICATION_INTERVAL)
}

/// Records and throttles a notification for the agent. Returns whether to send it.
pub fn claim_notification(terminal_view_id: EntityId, state: AgentState) -> bool {
    let now = Instant::now();
    LAST_NOTIFIED.with(|last_notified| {
        let mut last_notified = last_notified.borrow_mut();
        let due = notification_due(state, last_notified.get(&terminal_view_id).copied(), now);
        if due {
            last_notified.insert(terminal_view_id, now);
        }
        due
    })
}

/// Whether to send a desktop notification for the agent's new status: it needs the user, the
/// user cannot see it (other tab or window unfocused) and it was not notified recently.
pub fn should_notify_agent(terminal_view_id: EntityId, app: &AppContext) -> bool {
    let Some(snapshot) = agent_snapshot(terminal_view_id, app) else {
        return false;
    };
    snapshot.state.needs_attention()
        && !is_terminal_visible(terminal_view_id, app)
        && claim_notification(terminal_view_id, snapshot.state)
}

/// Whether the terminal is in the active tab of the focused window.
pub fn is_terminal_visible(terminal_view_id: EntityId, app: &AppContext) -> bool {
    let Some(window_id) = app.windows().active_window() else {
        return false;
    };
    WorkspaceRegistry::as_ref(app)
        .get(window_id, app)
        .is_some_and(|workspace| {
            let workspace = workspace.as_ref(app);
            workspace
                .get_pane_group_view(workspace.active_tab_index())
                .is_some_and(|pane_group| {
                    pane_group
                        .as_ref(app)
                        .visible_terminal_views(app)
                        .iter()
                        .any(|view| view.id() == terminal_view_id)
                })
        })
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
