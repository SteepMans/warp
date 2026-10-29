use super::*;

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn counts_group_states_by_what_they_need() {
    let counts = AttentionCounts::from_states([
        AgentState::WaitingPermission,
        AgentState::WaitingAnswer,
        AgentState::Error,
        AgentState::RunningTool,
        AgentState::Thinking,
        AgentState::RunningTool,
        AgentState::DoneUnseen,
        AgentState::Done,
        AgentState::Idle,
    ]);
    assert_eq!(
        counts,
        AttentionCounts {
            waiting: 2,
            errors: 1,
            running: 3,
            done: 1,
        }
    );
    assert!(AttentionCounts::from_states([AgentState::Idle, AgentState::Done]).is_empty());
}

#[test]
fn jump_order_is_permission_question_error_then_longest_waiting() {
    let (a, b, c, d, e) = (
        EntityId::new(),
        EntityId::new(),
        EntityId::new(),
        EntityId::new(),
        EntityId::new(),
    );
    let order = jump_order(&[
        (a, AgentState::Error, MINUTE * 30),
        (b, AgentState::WaitingAnswer, MINUTE),
        (c, AgentState::RunningTool, MINUTE * 60),
        (d, AgentState::WaitingAnswer, MINUTE * 14),
        (e, AgentState::WaitingPermission, Duration::from_secs(5)),
    ]);
    assert_eq!(order, vec![e, d, b, a]);
}

#[test]
fn next_jump_target_starts_at_most_urgent_and_walks_the_list() {
    let (a, b, c, elsewhere) = (
        EntityId::new(),
        EntityId::new(),
        EntityId::new(),
        EntityId::new(),
    );
    let order = vec![a, b, c];
    assert_eq!(next_jump_target(&order, None), Some(a));
    assert_eq!(next_jump_target(&order, Some(elsewhere)), Some(a));
    assert_eq!(next_jump_target(&order, Some(a)), Some(b));
    assert_eq!(next_jump_target(&order, Some(c)), Some(a));
    assert_eq!(next_jump_target(&[], Some(a)), None);
}

#[test]
fn notifications_only_for_attention_states_and_throttled() {
    let now = Instant::now();
    assert!(notification_due(AgentState::WaitingAnswer, None, now));
    assert!(notification_due(AgentState::Error, None, now));
    assert!(!notification_due(AgentState::DoneUnseen, None, now));
    assert!(!notification_due(AgentState::RunningTool, None, now));
    assert!(!notification_due(
        AgentState::WaitingPermission,
        Some(now),
        now + MINUTE * 9
    ));
    assert!(notification_due(
        AgentState::WaitingPermission,
        Some(now),
        now + NOTIFICATION_INTERVAL
    ));
}

#[test]
fn claim_notification_throttles_per_agent() {
    let (a, b) = (EntityId::new(), EntityId::new());
    assert!(claim_notification(a, AgentState::WaitingAnswer));
    assert!(!claim_notification(a, AgentState::Error));
    assert!(claim_notification(b, AgentState::Error));
    assert!(!claim_notification(EntityId::new(), AgentState::DoneUnseen));
}
