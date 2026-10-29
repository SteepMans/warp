use super::*;

fn secs(secs: u64) -> Option<Duration> {
    Some(Duration::from_secs(secs))
}

#[test]
fn statuses_map_to_outcomes() {
    assert_eq!(PushOutcome::from_status(200), PushOutcome::Accepted);
    assert_eq!(PushOutcome::from_status(202), PushOutcome::Accepted);
    assert_eq!(PushOutcome::from_status(401), PushOutcome::Rejected);
    assert_eq!(PushOutcome::from_status(403), PushOutcome::Rejected);
    assert_eq!(PushOutcome::from_status(429), PushOutcome::Retryable);
    assert_eq!(PushOutcome::from_status(500), PushOutcome::Retryable);
    assert_eq!(PushOutcome::from_status(503), PushOutcome::Retryable);
    assert_eq!(PushOutcome::from_status(400), PushOutcome::Unexpected);
    assert_eq!(PushOutcome::from_status(413), PushOutcome::Unexpected);
}

#[test]
fn success_keeps_the_regular_interval() {
    let mut backoff = Backoff::default();
    assert_eq!(backoff.next_delay(PushOutcome::Accepted), secs(60));
    assert_eq!(backoff.next_delay(PushOutcome::Accepted), secs(60));
}

#[test]
fn failures_double_the_interval_up_to_five_minutes() {
    let mut backoff = Backoff::default();
    assert_eq!(backoff.next_delay(PushOutcome::Retryable), secs(120));
    assert_eq!(backoff.next_delay(PushOutcome::Retryable), secs(240));
    assert_eq!(backoff.next_delay(PushOutcome::Retryable), secs(300));
    assert_eq!(backoff.next_delay(PushOutcome::Retryable), secs(300));
}

#[test]
fn success_resets_the_backoff() {
    let mut backoff = Backoff::default();
    backoff.next_delay(PushOutcome::Retryable);
    backoff.next_delay(PushOutcome::Retryable);
    assert_eq!(backoff.next_delay(PushOutcome::Accepted), secs(60));
    assert_eq!(backoff.next_delay(PushOutcome::Retryable), secs(120));
}

#[test]
fn unexpected_status_keeps_the_current_pace() {
    let mut backoff = Backoff::default();
    backoff.next_delay(PushOutcome::Retryable);
    assert_eq!(backoff.next_delay(PushOutcome::Unexpected), secs(120));
}

#[test]
fn rejected_token_stops_pushing() {
    let mut backoff = Backoff::default();
    assert_eq!(backoff.next_delay(PushOutcome::Rejected), None);
}
