use std::time::Duration;

use super::*;

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn blocked_sources_map_to_waiting_states() {
    let permission = CLIAgentSessionStatus::Blocked {
        message: None,
        source: BlockedSource::PermissionRequest,
    };
    let question = CLIAgentSessionStatus::Blocked {
        message: None,
        source: BlockedSource::QuestionAsked,
    };
    let needs_input = CLIAgentSessionStatus::Blocked {
        message: None,
        source: BlockedSource::NeedsInput,
    };
    assert_eq!(
        derive_state(StatusKind::of(&permission), false, false, MINUTE),
        AgentState::WaitingPermission
    );
    assert_eq!(
        derive_state(StatusKind::of(&question), false, false, MINUTE),
        AgentState::WaitingAnswer
    );
    assert_eq!(
        derive_state(StatusKind::of(&needs_input), false, false, MINUTE),
        AgentState::WaitingAnswer
    );
}

#[test]
fn in_progress_is_thinking_until_a_tool_finishes() {
    assert_eq!(
        derive_state(StatusKind::InProgress, false, false, MINUTE),
        AgentState::Thinking
    );
    assert_eq!(
        derive_state(StatusKind::InProgress, true, false, MINUTE),
        AgentState::RunningTool
    );
}

#[test]
fn success_is_unseen_then_done_then_idle() {
    assert_eq!(
        derive_state(StatusKind::Success, false, true, MINUTE),
        AgentState::DoneUnseen
    );
    assert_eq!(
        derive_state(StatusKind::Success, false, false, MINUTE),
        AgentState::Done
    );
    assert_eq!(
        derive_state(StatusKind::Success, false, true, IDLE_AFTER),
        AgentState::Idle
    );
    assert_eq!(
        derive_state(StatusKind::Cancelled, false, false, IDLE_AFTER + MINUTE),
        AgentState::Idle
    );
    assert_eq!(
        derive_state(StatusKind::Cancelled, false, false, MINUTE),
        AgentState::Done
    );
}

#[test]
fn failure_is_error_regardless_of_age() {
    assert_eq!(
        derive_state(StatusKind::Failed, true, false, IDLE_AFTER * 4),
        AgentState::Error
    );
}

#[test]
fn tracked_agent_restarts_clock_only_on_kind_change() {
    let start = Instant::now();
    let mut agent = TrackedAgent::new(StatusKind::InProgress, start);
    agent.observe(StatusKind::InProgress, start + MINUTE);
    assert_eq!(agent.changed_at, start);

    agent.observe(StatusKind::Success, start + MINUTE);
    assert_eq!(agent.changed_at, start + MINUTE);
    assert!(agent.unseen);

    agent.observe(StatusKind::InProgress, start + MINUTE * 2);
    assert!(!agent.unseen);
}

#[test]
fn jump_ranks_order_permission_question_error() {
    assert!(AgentState::WaitingPermission.jump_rank() < AgentState::WaitingAnswer.jump_rank());
    assert!(AgentState::WaitingAnswer.jump_rank() < AgentState::Error.jump_rank());
    assert_eq!(AgentState::RunningTool.jump_rank(), None);
    assert_eq!(AgentState::DoneUnseen.jump_rank(), None);
}

#[test]
fn tool_line_joins_name_and_collapsed_preview() {
    assert_eq!(
        tool_line("Bash", Some("dotnet  test\n  Hassle.Sync.Tests")),
        "Bash dotnet test Hassle.Sync.Tests"
    );
    assert_eq!(tool_line("Read", Some("  ")), "Read");
    assert_eq!(tool_line("Read", None), "Read");
}

#[test]
fn error_line_prefers_type_and_first_message_line() {
    assert_eq!(
        error_line(Some("overloaded"), Some("\nAPI Error 529\nretry failed")),
        Some("overloaded: API Error 529".to_owned())
    );
    assert_eq!(error_line(None, Some("boom")), Some("boom".to_owned()));
    assert_eq!(
        error_line(Some("rate_limit"), None),
        Some("rate_limit".to_owned())
    );
}

#[test]
fn format_in_state_is_coarse() {
    assert_eq!(format_in_state(Duration::from_secs(5)), "<1m");
    assert_eq!(format_in_state(MINUTE * 14), "14m");
    assert_eq!(format_in_state(MINUTE * 125), "2h");
}

#[test]
fn clip_limits_long_text() {
    let long = "a".repeat(MAX_DETAIL_CHARS * 2);
    let clipped = clip(&long);
    assert_eq!(clipped.chars().count(), MAX_DETAIL_CHARS);
    assert!(clipped.ends_with('\u{2026}'));
}

#[test]
fn snapshot_texts_are_built_once_per_session_change() {
    let mut agent = TrackedAgent::new(StatusKind::InProgress, Instant::now());
    let mut built = 0;
    for _ in 0..50 {
        let (detail, task) = agent.texts(AgentState::Thinking, |_| {
            built += 1;
            (Some("first".to_owned()), Some("task".to_owned()))
        });
        assert_eq!(detail.as_deref(), Some("first"));
        assert_eq!(task.as_deref(), Some("task"));
    }
    assert_eq!(built, 1);

    // A plugin event or status update invalidates the texts.
    agent.touch();
    let (detail, _) = agent.texts(AgentState::Thinking, |_| {
        built += 1;
        (Some("second".to_owned()), None)
    });
    assert_eq!(detail.as_deref(), Some("second"));
    assert_eq!(built, 2);
}

#[test]
fn snapshot_texts_follow_the_derived_state() {
    let start = Instant::now();
    let mut agent = TrackedAgent::new(StatusKind::Success, start);
    let mut built = Vec::new();
    agent.texts(AgentState::Done, |_| {
        built.push(AgentState::Done);
        (Some("All tests pass".to_owned()), None)
    });
    agent.texts(AgentState::Done, |_| {
        built.push(AgentState::Done);
        (None, None)
    });
    // Time alone moves a finished agent to idle: the text is rebuilt for the new state.
    let (detail, _) = agent.texts(AgentState::Idle, |_| {
        built.push(AgentState::Idle);
        (Some("Idle".to_owned()), None)
    });
    assert_eq!(detail.as_deref(), Some("Idle"));
    assert_eq!(built, vec![AgentState::Done, AgentState::Idle]);
}

#[test]
fn status_kind_change_invalidates_snapshot_texts() {
    let start = Instant::now();
    let mut agent = TrackedAgent::new(StatusKind::Success, start);
    agent.texts(AgentState::Done, |_| (Some("Done".to_owned()), None));
    // Success and Cancelled both show as Done, with different texts.
    agent.observe(StatusKind::Cancelled, start + MINUTE);
    let (detail, _) = agent.texts(AgentState::Done, |_| (Some("Cancelled".to_owned()), None));
    assert_eq!(detail.as_deref(), Some("Cancelled"));
}
