use super::*;

#[test]
fn never_seen_agent_does_not_end_anything() {
    let mut liveness = AgentProcessLiveness::default();
    for _ in 0..5 {
        assert_eq!(liveness.observe(None), None);
    }
}

#[test]
fn ends_after_consecutive_missed_polls() {
    let mut liveness = AgentProcessLiveness::default();
    assert_eq!(liveness.observe(Some("a")), None);
    assert_eq!(liveness.observe(Some("a")), None);
    assert_eq!(liveness.observe(None), None);
    assert_eq!(liveness.observe(None), Some(vec!["a".to_owned()]));
}

#[test]
fn fires_once_per_exit() {
    let mut liveness = AgentProcessLiveness::default();
    liveness.observe(Some("a"));
    liveness.observe(None);
    assert!(liveness.observe(None).is_some());
    assert_eq!(liveness.observe(None), None);
    assert_eq!(liveness.observe(None), None);
}

#[test]
fn single_miss_is_forgiven() {
    let mut liveness = AgentProcessLiveness::default();
    liveness.observe(Some("a"));
    assert_eq!(liveness.observe(None), None);
    assert_eq!(liveness.observe(Some("a")), None);
    assert_eq!(liveness.observe(None), None);
    assert_eq!(liveness.observe(None), Some(vec!["a".to_owned()]));
}

#[test]
fn keeps_ids_from_clear() {
    let mut liveness = AgentProcessLiveness::default();
    liveness.observe(Some("before-clear"));
    liveness.observe(Some("after-clear"));
    liveness.observe(Some("after-clear"));
    liveness.observe(None);
    assert_eq!(
        liveness.observe(None),
        Some(vec!["before-clear".to_owned(), "after-clear".to_owned()])
    );
}

#[test]
fn new_agent_after_exit_starts_fresh() {
    let mut liveness = AgentProcessLiveness::default();
    liveness.observe(Some("a"));
    liveness.observe(None);
    liveness.observe(None);
    liveness.observe(Some("b"));
    liveness.observe(None);
    assert_eq!(liveness.observe(None), Some(vec!["b".to_owned()]));
}

#[test]
fn matches_only_exited_sessions() {
    let exited = vec!["a".to_owned(), "b".to_owned()];
    assert!(session_has_exited(Some("a"), &exited));
    assert!(session_has_exited(Some("b"), &exited));
    // A new agent started in the same pane before the poll noticed it.
    assert!(!session_has_exited(Some("c"), &exited));
    // Sessions without a plugin id end with their command block instead.
    assert!(!session_has_exited(None, &exited));
    assert!(!session_has_exited(Some("a"), &[]));
}
