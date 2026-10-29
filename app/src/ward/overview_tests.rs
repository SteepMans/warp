use super::*;

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn columns_group_states() {
    assert_eq!(
        OverviewColumn::of(AgentState::WaitingPermission),
        OverviewColumn::NeedsYou
    );
    assert_eq!(
        OverviewColumn::of(AgentState::WaitingAnswer),
        OverviewColumn::NeedsYou
    );
    assert_eq!(OverviewColumn::of(AgentState::Thinking), OverviewColumn::Running);
    assert_eq!(OverviewColumn::of(AgentState::RunningTool), OverviewColumn::Running);
    assert_eq!(OverviewColumn::of(AgentState::Error), OverviewColumn::Errors);
    assert_eq!(OverviewColumn::of(AgentState::Idle), OverviewColumn::DoneIdle);
    assert_eq!(OverviewColumn::of(AgentState::DoneUnseen), OverviewColumn::DoneIdle);
}

#[test]
fn cards_sort_by_urgency_then_time() {
    let mut cards = vec![
        (AgentState::WaitingAnswer, 20 * MINUTE),
        (AgentState::WaitingPermission, MINUTE),
        (AgentState::WaitingAnswer, 40 * MINUTE),
        (AgentState::Done, 5 * MINUTE),
        (AgentState::DoneUnseen, 30 * MINUTE),
        (AgentState::Done, MINUTE),
    ];
    cards.sort_by(|a, b| card_order(*a, *b));
    assert_eq!(
        cards,
        vec![
            (AgentState::WaitingPermission, MINUTE),
            (AgentState::WaitingAnswer, 40 * MINUTE),
            (AgentState::WaitingAnswer, 20 * MINUTE),
            (AgentState::DoneUnseen, 30 * MINUTE),
            (AgentState::Done, MINUTE),
            (AgentState::Done, 5 * MINUTE),
        ]
    );
}

#[test]
fn tail_skips_frames_prompts_and_hints() {
    let screen = "\
Apply migration 0042 to stage or only locally?
migrations/0042_refund_status.sql

\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}
\u{2502} 1. only locally      \u{2502}
\u{2502} 2. stage too         \u{2502}
\u{2502} >                    \u{2502}
\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}
  ? for shortcuts
";
    assert_eq!(
        tail_lines(screen, 3),
        vec![
            "migrations/0042_refund_status.sql".to_owned(),
            "1. only locally".to_owned(),
            "2. stage too".to_owned(),
        ]
    );
    assert!(tail_lines("\n \n\u{2500}\u{2500}\n", 3).is_empty());
}

#[test]
fn open_state_is_per_window() {
    let window = WindowId::new();
    assert!(!is_open(window));
    assert!(set_open(window, true));
    assert!(is_open(window));
    assert!(!set_open(window, true));
    assert!(set_open(window, false));
    assert!(!is_open(window));
}
