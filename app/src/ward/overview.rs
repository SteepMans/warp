//! The agent overview board: every CLI agent of every window in four columns by what it needs
//! from the user, toggled per window with Ctrl+Alt+W or the header button.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::time::Duration;

use warpui::WindowId;

use super::agent_state::AgentState;
use crate::terminal::TerminalView;
use crate::terminal::model::index::Point;

/// Terminal rows scanned for the output tail of a card. Only the end of the active block is
/// read, so long dialogs cost the same as short ones.
pub const TAIL_SCAN_ROWS: usize = 40;

/// Output lines shown on a card.
pub const TAIL_LINES: usize = 3;

/// Longest tail line kept; the card clips it further to its width.
const MAX_TAIL_LINE_CHARS: usize = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverviewColumn {
    NeedsYou,
    Running,
    Errors,
    DoneIdle,
}

impl OverviewColumn {
    pub const ALL: [Self; 4] = [Self::NeedsYou, Self::Running, Self::Errors, Self::DoneIdle];

    pub fn of(state: AgentState) -> Self {
        match state {
            AgentState::WaitingPermission | AgentState::WaitingAnswer => Self::NeedsYou,
            AgentState::RunningTool | AgentState::Thinking => Self::Running,
            AgentState::Error => Self::Errors,
            AgentState::DoneUnseen | AgentState::Done | AgentState::Idle => Self::DoneIdle,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Running => "Running",
            Self::Errors => "Errors",
            Self::DoneIdle => "Done & idle",
        }
    }
}

/// Order of cards inside a column: permissions before questions, unseen results before seen
/// ones before idle agents; then whoever has been in the state longest (the user owes them
/// the most), except finished agents, where the freshest result comes first.
pub fn card_order(a: (AgentState, Duration), b: (AgentState, Duration)) -> Ordering {
    fn rank(state: AgentState) -> u8 {
        match state {
            AgentState::WaitingPermission => 0,
            AgentState::WaitingAnswer => 1,
            AgentState::Error => 2,
            AgentState::RunningTool => 3,
            AgentState::Thinking => 4,
            AgentState::DoneUnseen => 5,
            AgentState::Done => 6,
            AgentState::Idle => 7,
        }
    }
    let finished = |state: AgentState| OverviewColumn::of(state) == OverviewColumn::DoneIdle;
    rank(a.0).cmp(&rank(b.0)).then_with(|| {
        if finished(a.0) {
            a.1.cmp(&b.1)
        } else {
            b.1.cmp(&a.1)
        }
    })
}

/// Whether a line is only frame drawing (box drawing, block elements) or blank.
fn is_decoration(line: &str) -> bool {
    line.chars()
        .all(|c| c.is_whitespace() || ('\u{2500}'..='\u{259f}').contains(&c))
}

/// Strips the side borders a TUI draws around a line.
fn strip_frame(line: &str) -> &str {
    line.trim()
        .trim_matches(|c: char| c.is_whitespace() || ('\u{2500}'..='\u{259f}').contains(&c))
}

/// The last `max` meaningful lines of terminal text: no blank or frame-only lines, no bare input
/// prompt and no key-hint lines of agent TUIs.
pub fn tail_lines(text: &str, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .filter(|line| !is_decoration(line))
        .map(strip_frame)
        .filter(|line| !line.is_empty() && *line != ">" && !line.contains("? for shortcuts"))
        .take(max)
        .map(|line| super::labels::clip_chars(line, MAX_TAIL_LINE_CHARS))
        .collect();
    lines.reverse();
    lines
}

/// The last lines of the agent's terminal output, read from the end of the grid only (the
/// running block, or the alternate screen of a full-screen TUI).
pub fn terminal_tail(terminal_view: &TerminalView) -> Vec<String> {
    let text = {
        let model = terminal_view.model.lock();
        if model.is_alt_screen_active() {
            model.alt_screen().output_to_string()
        } else {
            let grid = model.block_list().active_block().output_grid();
            let rows = grid.len();
            if rows == 0 {
                return Vec::new();
            }
            grid.point_to_end_as_string(Point::new(rows.saturating_sub(TAIL_SCAN_ROWS), 0))
        }
    };
    tail_lines(&text, TAIL_LINES)
}

thread_local! {
    static OPEN_WINDOWS: RefCell<HashSet<WindowId>> = RefCell::new(HashSet::new());
}

pub fn is_open(window_id: WindowId) -> bool {
    OPEN_WINDOWS.with(|open| open.borrow().contains(&window_id))
}

/// Opens or closes the board in a window. Returns whether anything changed.
pub fn set_open(window_id: WindowId, open: bool) -> bool {
    OPEN_WINDOWS.with(|windows| {
        let mut windows = windows.borrow_mut();
        if open {
            windows.insert(window_id)
        } else {
            windows.remove(&window_id)
        }
    })
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;
