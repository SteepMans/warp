//! Ward workspace actions, carried by the single `WorkspaceAction::Ward` variant so the upstream
//! action enum only grows by one entry.

use warpui::EntityId;

#[derive(Debug, Clone)]
pub enum WardAction {
    /// Shows or hides the agent overview board in this window.
    ToggleOverview,
    /// Focuses the agent's terminal (any window) and closes the overview board.
    OpenAgent(EntityId),
    /// Marks a finished agent as seen.
    MarkAgentSeen(EntityId),
    /// Closes the tab that holds the agent's terminal.
    CloseAgentTab(EntityId),
}
