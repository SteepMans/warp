//! Prometheus text exposition (version 0.0.4) of Ward's metrics, as pushed to the
//! Pushgateway. The job and instance labels come from the push URL, never from the body.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::state::{MetricsState, RestoredAgent};
use crate::ward::agent_state::{AgentState, StatusKind};

/// The Pushgateway rejects larger bodies.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Agent label of a live CLI agent session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentLabel {
    Claude,
    Codex,
    Other,
}

impl AgentLabel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Other => "other",
        }
    }
}

/// Coarse state of a live CLI agent session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SessionState {
    Working,
    Waiting,
    Idle,
}

impl SessionState {
    /// Running or thinking is working, blocked on the user is waiting, anything finished
    /// (done, idle, error) is idle.
    pub fn of_agent_state(state: AgentState) -> Self {
        match state {
            AgentState::RunningTool | AgentState::Thinking => Self::Working,
            AgentState::WaitingPermission | AgentState::WaitingAnswer => Self::Waiting,
            AgentState::Error | AgentState::DoneUnseen | AgentState::Done | AgentState::Idle => {
                Self::Idle
            }
        }
    }

    /// The same mapping for sessions that only report the coarse status.
    pub fn of_status_kind(kind: StatusKind) -> Self {
        match kind {
            StatusKind::InProgress => Self::Working,
            StatusKind::Permission | StatusKind::Question => Self::Waiting,
            StatusKind::Success | StatusKind::Failed | StatusKind::Cancelled => Self::Idle,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
        }
    }
}

/// Everything one push reports.
#[derive(Clone, Debug)]
pub struct MetricsSnapshot<'a> {
    pub version: &'a str,
    /// `stable`, `preview` or `dev`.
    pub channel: &'static str,
    /// `x86_64` or `aarch64`.
    pub arch: &'static str,
    pub running: bool,
    pub process_start_time_seconds: u64,
    pub state: &'a MetricsState,
    /// Panics of the current run, not yet part of `state`.
    pub panics_this_run: u64,
    /// Live agent sessions; `None` omits the series entirely (the final push on exit).
    pub agent_sessions: Option<&'a BTreeMap<(AgentLabel, SessionState), u64>>,
}

/// Renders the push body.
pub fn render(snapshot: &MetricsSnapshot<'_>) -> String {
    let state = snapshot.state;
    let mut body = String::with_capacity(2048);

    family(&mut body, "ward_build_info", "gauge");
    sample(
        &mut body,
        "ward_build_info",
        &[
            ("version", snapshot.version),
            ("channel", snapshot.channel),
            ("os", "windows"),
            ("arch", snapshot.arch),
        ],
        1,
    );

    family(&mut body, "ward_app_running", "gauge");
    sample(
        &mut body,
        "ward_app_running",
        &[],
        u64::from(snapshot.running),
    );

    family(&mut body, "ward_process_start_time_seconds", "gauge");
    sample(
        &mut body,
        "ward_process_start_time_seconds",
        &[],
        snapshot.process_start_time_seconds,
    );

    family(&mut body, "ward_app_launches_total", "counter");
    sample(&mut body, "ward_app_launches_total", &[], state.launches);

    family(&mut body, "ward_crashes_total", "counter");
    sample(
        &mut body,
        "ward_crashes_total",
        &[("kind", "panic")],
        state.crashes.panic + snapshot.panics_this_run,
    );
    sample(
        &mut body,
        "ward_crashes_total",
        &[("kind", "unclean_exit")],
        state.crashes.unclean_exit,
    );

    family(&mut body, "ward_update_installs_total", "counter");
    sample(
        &mut body,
        "ward_update_installs_total",
        &[("result", "success")],
        state.update_installs.success,
    );
    sample(
        &mut body,
        "ward_update_installs_total",
        &[("result", "failure")],
        state.update_installs.failure,
    );

    family(&mut body, "ward_restored_sessions_total", "counter");
    for agent in RestoredAgent::ALL {
        sample(
            &mut body,
            "ward_restored_sessions_total",
            &[("agent", agent.label())],
            state.restored_sessions.get(agent),
        );
    }

    if let Some(sessions) = snapshot.agent_sessions {
        family(&mut body, "ward_agent_sessions", "gauge");
        for ((agent, session_state), count) in sessions {
            if *count == 0 {
                continue;
            }
            sample(
                &mut body,
                "ward_agent_sessions",
                &[("agent", agent.label()), ("state", session_state.label())],
                *count,
            );
        }
    }

    body
}

fn family(body: &mut String, name: &str, kind: &str) {
    let _ = writeln!(body, "# TYPE {name} {kind}");
}

fn sample(body: &mut String, name: &str, labels: &[(&str, &str)], value: u64) {
    body.push_str(name);
    if !labels.is_empty() {
        body.push('{');
        for (index, (label, value)) in labels.iter().enumerate() {
            if index > 0 {
                body.push(',');
            }
            let _ = write!(body, "{label}=\"{}\"", escape_label_value(value));
        }
        body.push('}');
    }
    let _ = writeln!(body, " {value}");
}

fn escape_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
#[path = "body_tests.rs"]
mod tests;
