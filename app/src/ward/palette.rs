//! The `@` agents palette: CLI agents in urgency order (permissions, questions, errors, then
//! running and finished ones) with fuzzy search over project, task and question.

use std::time::Duration;

use fuzzy_match::match_indices_case_insensitive;
use warpui::{AppContext, EntityId};

use super::agent_state::AgentState;
use super::overview::card_order;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteAgent {
    pub terminal_view_id: EntityId,
    pub state: AgentState,
    pub in_state: Duration,
    pub project: Option<String>,
    pub task: String,
    /// Question, permission, tool or error line.
    pub detail: Option<String>,
}

/// Best fuzzy score of the query over the agent's fields, `None` when nothing matches. An empty
/// query matches everything.
pub fn match_score(agent: &PaletteAgent, query: &str) -> Option<i64> {
    let query = query.trim();
    if query.is_empty() {
        return Some(0);
    }
    [
        agent.project.as_deref(),
        Some(agent.task.as_str()),
        agent.detail.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|field| match_indices_case_insensitive(field, query))
    .map(|result| result.score)
    .max()
}

/// Char indices of the query's fuzzy match in each field, for highlighting like the other
/// palette modes. Empty for an empty query and for fields that do not match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MatchedIndices {
    pub project: Vec<usize>,
    pub task: Vec<usize>,
    pub detail: Vec<usize>,
}

pub fn matched_indices(agent: &PaletteAgent, query: &str) -> MatchedIndices {
    let query = query.trim();
    let indices = |field: Option<&str>| {
        if query.is_empty() {
            return Vec::new();
        }
        field
            .and_then(|field| match_indices_case_insensitive(field, query))
            .map(|result| result.matched_indices)
            .unwrap_or_default()
    };
    MatchedIndices {
        project: indices(agent.project.as_deref()),
        task: indices(Some(agent.task.as_str())),
        detail: indices(agent.detail.as_deref()),
    }
}

/// The agents matching `query`, most urgent first; equally urgent agents keep the order of their
/// time in state (longest waiting first).
pub fn filter_and_sort(agents: Vec<PaletteAgent>, query: &str) -> Vec<PaletteAgent> {
    let mut matching: Vec<PaletteAgent> = agents
        .into_iter()
        .filter(|agent| match_score(agent, query).is_some())
        .collect();
    matching.sort_by(|a, b| card_order((a.state, a.in_state), (b.state, b.in_state)));
    matching
}

/// Every CLI agent the palette can jump to.
pub fn palette_agents(app: &AppContext) -> Vec<PaletteAgent> {
    super::attention::all_agent_entries(app)
        .into_iter()
        .map(|entry| PaletteAgent {
            terminal_view_id: entry.terminal_view.id(),
            state: entry.snapshot.state,
            in_state: entry.snapshot.in_state,
            project: entry.labels.project,
            task: entry.labels.task,
            detail: entry.snapshot.detail,
        })
        .collect()
}

/// Section label shown above the first agent of each urgency group.
pub fn section_label(state: AgentState) -> &'static str {
    match state {
        AgentState::WaitingPermission | AgentState::WaitingAnswer => "Waiting for you",
        AgentState::Error => "Errors",
        AgentState::RunningTool | AgentState::Thinking => "Running",
        AgentState::DoneUnseen | AgentState::Done | AgentState::Idle => "Done & idle",
    }
}

#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
