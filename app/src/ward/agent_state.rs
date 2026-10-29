//! Per-agent state shown in Ward's tab rows, derived from [`CLIAgentSessionStatus`] plus the
//! client-side facts the plugin protocol does not carry: when the status last changed, the last
//! tool the agent finished and whether the user has seen a completed run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use instant::Instant;
use warp_core::ui::color::blend::Blend;
use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::WarpTheme;
use warpui::color::ColorU;
use warpui::{AppContext, EntityId, SingletonEntity};

use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::event::{CLIAgentEvent, CLIAgentEventType};
use crate::terminal::cli_agent_sessions::{
    BlockedSource, CLIAgentSession, CLIAgentSessionStatus, CLIAgentSessionsModel,
};
use crate::ui_components::icons::Icon;

/// A completed or cancelled agent left alone for this long is shown as idle.
pub const IDLE_AFTER: Duration = Duration::from_secs(30 * 60);

/// Longest detail text kept for a row; the row clips it further to its width.
const MAX_DETAIL_CHARS: usize = 240;

/// What a CLI agent is doing, from the user's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentState {
    /// Blocked on a tool permission prompt.
    WaitingPermission,
    /// Blocked on a question or other input from the user.
    WaitingAnswer,
    /// The turn failed.
    Error,
    /// Working, and has already run at least one tool this turn.
    RunningTool,
    /// Working, no tool has finished yet this turn.
    Thinking,
    /// Finished while the user was looking elsewhere.
    DoneUnseen,
    /// Finished (or cancelled) and already seen.
    Done,
    /// Finished or cancelled more than [`IDLE_AFTER`] ago.
    Idle,
}

impl AgentState {
    /// Whether the agent is blocked on the user.
    pub fn is_waiting(self) -> bool {
        matches!(self, Self::WaitingPermission | Self::WaitingAnswer)
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::RunningTool | Self::Thinking)
    }

    /// Whether the state should pull the user's attention (highlighted row, toast, jump target).
    pub fn needs_attention(self) -> bool {
        self.is_waiting() || self == Self::Error
    }

    /// Jump priority for agents that need the user: permissions first, then questions, then
    /// errors. `None` for states that are not jump targets.
    pub fn jump_rank(self) -> Option<u8> {
        match self {
            Self::WaitingPermission => Some(0),
            Self::WaitingAnswer => Some(1),
            Self::Error => Some(2),
            _ => None,
        }
    }

    /// Glyph for the state. Each state has its own shape so it reads without color.
    pub fn icon(self) -> Icon {
        match self {
            Self::WaitingPermission => Icon::AlertTriangle,
            Self::WaitingAnswer => Icon::HelpCircle,
            Self::Error => Icon::StopFilled,
            Self::RunningTool => Icon::ClockLoader,
            Self::Thinking => Icon::DotsHorizontal,
            Self::DoneUnseen => Icon::CheckCircleBroken,
            Self::Done => Icon::Check,
            Self::Idle => Icon::ClockSnooze,
        }
    }

    /// State color from the active theme. Under Dracula: yellow question, orange permission,
    /// cyan tool, purple thinking, green done, red error, comment-grey idle.
    pub fn color(self, theme: &WarpTheme) -> ColorU {
        match self {
            Self::WaitingPermission => permission_color(theme),
            Self::WaitingAnswer => theme.ansi_fg_yellow(),
            Self::Error => theme.ansi_fg_red(),
            Self::RunningTool => theme.ansi_fg_cyan(),
            Self::Thinking => theme.ansi_fg_blue(),
            Self::DoneUnseen | Self::Done => theme.ansi_fg_green(),
            Self::Idle => theme.sub_text_color(theme.background()).into_solid(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::WaitingPermission => "Waiting for permission",
            Self::WaitingAnswer => "Waiting for answer",
            Self::Error => "Error",
            Self::RunningTool => "Running tool",
            Self::Thinking => "Thinking",
            Self::DoneUnseen => "Done",
            Self::Done => "Done",
            Self::Idle => "Idle",
        }
    }
}

/// The theme has no orange, so the permission color is mixed from its yellow and red.
pub fn permission_color(theme: &WarpTheme) -> ColorU {
    theme
        .ansi_fg_yellow()
        .blend(&coloru_with_opacity(theme.ansi_fg_red(), 50))
}

/// Coarse status used to detect status changes; ignores message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    InProgress,
    Success,
    Failed,
    Permission,
    Question,
    Cancelled,
}

impl StatusKind {
    pub fn of(status: &CLIAgentSessionStatus) -> Self {
        match status {
            CLIAgentSessionStatus::InProgress => Self::InProgress,
            CLIAgentSessionStatus::Success => Self::Success,
            CLIAgentSessionStatus::Failed { .. } => Self::Failed,
            CLIAgentSessionStatus::Blocked {
                source: BlockedSource::PermissionRequest,
                ..
            } => Self::Permission,
            CLIAgentSessionStatus::Blocked {
                source: BlockedSource::QuestionAsked | BlockedSource::NeedsInput,
                ..
            } => Self::Question,
            CLIAgentSessionStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// Pure state derivation.
pub fn derive_state(
    kind: StatusKind,
    has_tool: bool,
    unseen: bool,
    in_state: Duration,
) -> AgentState {
    match kind {
        StatusKind::Permission => AgentState::WaitingPermission,
        StatusKind::Question => AgentState::WaitingAnswer,
        StatusKind::Failed => AgentState::Error,
        StatusKind::InProgress if has_tool => AgentState::RunningTool,
        StatusKind::InProgress => AgentState::Thinking,
        StatusKind::Success | StatusKind::Cancelled if in_state >= IDLE_AFTER => AgentState::Idle,
        StatusKind::Success if unseen => AgentState::DoneUnseen,
        StatusKind::Success | StatusKind::Cancelled => AgentState::Done,
    }
}

/// A tool the agent ran most recently in the current turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolActivity {
    pub name: String,
    pub input_preview: Option<String>,
}

#[derive(Clone, Debug)]
struct TrackedAgent {
    kind: StatusKind,
    changed_at: Instant,
    last_tool: Option<ToolActivity>,
    unseen: bool,
}

impl TrackedAgent {
    fn new(kind: StatusKind, now: Instant) -> Self {
        Self {
            kind,
            changed_at: now,
            last_tool: None,
            unseen: false,
        }
    }

    /// Applies an observed status; restarts the clock only when the coarse status changed.
    fn observe(&mut self, kind: StatusKind, now: Instant) {
        if self.kind == kind {
            return;
        }
        self.kind = kind;
        self.changed_at = now;
        self.unseen = kind == StatusKind::Success;
    }
}

thread_local! {
    // UI state lives on the main thread; tests get their own copy per test thread.
    static TRACKED: RefCell<HashMap<EntityId, TrackedAgent>> = RefCell::new(HashMap::new());
}

fn with_tracked<R>(
    terminal_view_id: EntityId,
    kind: StatusKind,
    f: impl FnOnce(&mut TrackedAgent) -> R,
) -> R {
    TRACKED.with(|tracked| {
        let mut tracked = tracked.borrow_mut();
        let entry = tracked
            .entry(terminal_view_id)
            .or_insert_with(|| TrackedAgent::new(kind, Instant::now()));
        f(entry)
    })
}

/// Records the session status after `CLIAgentSessionsModel` changed it.
pub fn observe_status(terminal_view_id: EntityId, status: &CLIAgentSessionStatus) {
    if !crate::features::is_fork_oss() {
        return;
    }
    let kind = StatusKind::of(status);
    with_tracked(terminal_view_id, kind, |agent| agent.observe(kind, Instant::now()));
}

/// Records a plugin event and the status the session ended up in after it.
pub fn observe_event(
    terminal_view_id: EntityId,
    event: &CLIAgentEvent,
    status: &CLIAgentSessionStatus,
) {
    if !crate::features::is_fork_oss() {
        return;
    }
    let kind = StatusKind::of(status);
    with_tracked(terminal_view_id, kind, |agent| {
        match &event.event {
            // A new turn starts without tools.
            CLIAgentEventType::PromptSubmit => agent.last_tool = None,
            CLIAgentEventType::ToolComplete => {
                if let Some(name) = event
                    .payload
                    .tool_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    agent.last_tool = Some(ToolActivity {
                        name: name.to_owned(),
                        input_preview: event.payload.tool_input_preview.clone(),
                    });
                }
            }
            _ => {}
        }
        agent.observe(kind, Instant::now());
    });
}

/// Forgets a session that ended.
pub fn forget(terminal_view_id: EntityId) {
    TRACKED.with(|tracked| tracked.borrow_mut().remove(&terminal_view_id));
}

/// Marks a completed run as seen. Returns whether anything changed.
pub fn mark_seen(terminal_view_id: EntityId) -> bool {
    TRACKED.with(|tracked| {
        tracked
            .borrow_mut()
            .get_mut(&terminal_view_id)
            .is_some_and(|agent| std::mem::replace(&mut agent.unseen, false))
    })
}

/// Everything a row, the attention strip or a jump needs to know about one agent.
#[derive(Clone, Debug)]
pub struct AgentSnapshot {
    pub terminal_view_id: EntityId,
    pub agent: CLIAgent,
    pub state: AgentState,
    pub in_state: Duration,
    /// Question text, tool and argument, error message or completion summary.
    pub detail: Option<String>,
    /// The user's latest prompt (the agent's current task).
    pub task: Option<String>,
}

/// Snapshot of the CLI agent in the given terminal. `None` outside the OSS build, for plain
/// terminals and for sessions without rich (plugin) status.
pub fn agent_snapshot(terminal_view_id: EntityId, app: &AppContext) -> Option<AgentSnapshot> {
    if !crate::features::is_fork_oss() {
        return None;
    }
    let session = CLIAgentSessionsModel::as_ref(app)
        .session(terminal_view_id)
        .filter(|session| session.supports_rich_status())
        .filter(|session| !matches!(session.agent, CLIAgent::Unknown))?;
    let kind = StatusKind::of(&session.status);
    let (changed_at, last_tool, unseen) = with_tracked(terminal_view_id, kind, |agent| {
        // Sessions whose status changed before tracking started are adopted here.
        agent.observe(kind, Instant::now());
        (agent.changed_at, agent.last_tool.clone(), agent.unseen)
    });
    let in_state = Instant::now().saturating_duration_since(changed_at);
    let state = derive_state(kind, last_tool.is_some(), unseen, in_state);
    Some(AgentSnapshot {
        terminal_view_id,
        agent: session.agent,
        state,
        in_state,
        detail: detail_text(state, session, last_tool.as_ref()),
        task: session.session_context.latest_user_prompt().map(|t| clip(&t)),
    })
}

fn detail_text(
    state: AgentState,
    session: &CLIAgentSession,
    last_tool: Option<&ToolActivity>,
) -> Option<String> {
    let context = &session.session_context;
    let text = match (state, &session.status) {
        (AgentState::WaitingPermission, _) => match context.tool_name.as_deref() {
            Some(tool) => Some(format!(
                "Allow: {}",
                tool_line(tool, context.tool_input_preview.as_deref())
            )),
            None => blocked_message(&session.status),
        },
        (AgentState::WaitingAnswer, _) => blocked_message(&session.status),
        (AgentState::Error, CLIAgentSessionStatus::Failed { error_type, message }) => {
            error_line(error_type.as_deref(), message.as_deref())
        }
        (AgentState::RunningTool, _) => {
            last_tool.map(|tool| tool_line(&tool.name, tool.input_preview.as_deref()))
        }
        (AgentState::Thinking, _) => Some("Thinking\u{2026}".to_owned()),
        (AgentState::DoneUnseen | AgentState::Done, CLIAgentSessionStatus::Cancelled) => {
            Some("Cancelled".to_owned())
        }
        (AgentState::DoneUnseen | AgentState::Done, _) => context
            .response
            .as_deref()
            .and_then(first_line)
            .or_else(|| Some("Done".to_owned())),
        (AgentState::Idle, _) => Some("Idle".to_owned()),
        _ => None,
    };
    text.map(|text| clip(&text)).filter(|text| !text.is_empty())
}

fn blocked_message(status: &CLIAgentSessionStatus) -> Option<String> {
    match status {
        CLIAgentSessionStatus::Blocked { message, .. } => message.clone(),
        _ => None,
    }
}

/// `"Bash dotnet test"`: the tool name followed by its argument preview.
pub fn tool_line(tool: &str, input_preview: Option<&str>) -> String {
    match input_preview.map(collapse_whitespace).filter(|p| !p.is_empty()) {
        Some(preview) => format!("{tool} {preview}"),
        None => tool.to_owned(),
    }
}

pub fn error_line(error_type: Option<&str>, message: Option<&str>) -> Option<String> {
    let message = message.and_then(first_line);
    match (error_type.filter(|t| !t.is_empty()), message) {
        (Some(kind), Some(message)) => Some(format!("{kind}: {message}")),
        (Some(kind), None) => Some(kind.to_owned()),
        (None, Some(message)) => Some(message),
        (None, None) => Some("The agent stopped with an error".to_owned()),
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(collapse_whitespace)
        .find(|line| !line.is_empty())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str) -> String {
    let text = collapse_whitespace(text);
    if text.chars().count() <= MAX_DETAIL_CHARS {
        return text;
    }
    let mut clipped: String = text.chars().take(MAX_DETAIL_CHARS - 1).collect();
    clipped.push('\u{2026}');
    clipped
}

/// Time in the current state, coarse enough not to look frozen between repaints:
/// `<1m`, `14m`, `2h`.
pub fn format_in_state(in_state: Duration) -> String {
    let secs = in_state.as_secs();
    match secs {
        0..=59 => "<1m".to_owned(),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

#[cfg(test)]
#[path = "agent_state_tests.rs"]
mod tests;
