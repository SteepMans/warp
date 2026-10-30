use super::*;

fn minutes(minutes: u64) -> Duration {
    Duration::from_secs(minutes * 60)
}

#[test]
fn statuses_map_to_outcomes() {
    assert_eq!(PollOutcome::from_status(200), PollOutcome::Fetched);
    assert_eq!(PollOutcome::from_status(204), PollOutcome::Fetched);
    assert_eq!(PollOutcome::from_status(429), PollOutcome::Retryable);
    assert_eq!(PollOutcome::from_status(500), PollOutcome::Retryable);
    assert_eq!(PollOutcome::from_status(503), PollOutcome::Retryable);
    assert_eq!(PollOutcome::from_status(404), PollOutcome::Unexpected);
    assert_eq!(PollOutcome::from_status(403), PollOutcome::Unexpected);
}

#[test]
fn success_keeps_the_regular_interval() {
    let mut backoff = Backoff::default();
    assert_eq!(backoff.next_delay(PollOutcome::Fetched), minutes(15));
    assert_eq!(backoff.next_delay(PollOutcome::Fetched), minutes(15));
}

#[test]
fn failures_double_the_interval_up_to_an_hour() {
    let mut backoff = Backoff::default();
    assert_eq!(backoff.next_delay(PollOutcome::Retryable), minutes(30));
    assert_eq!(backoff.next_delay(PollOutcome::Retryable), minutes(60));
    assert_eq!(backoff.next_delay(PollOutcome::Retryable), minutes(60));
}

#[test]
fn success_resets_the_backoff() {
    let mut backoff = Backoff::default();
    backoff.next_delay(PollOutcome::Retryable);
    backoff.next_delay(PollOutcome::Retryable);
    assert_eq!(backoff.next_delay(PollOutcome::Fetched), minutes(15));
    assert_eq!(backoff.next_delay(PollOutcome::Retryable), minutes(30));
}

#[test]
fn unexpected_outcome_keeps_the_current_pace() {
    let mut backoff = Backoff::default();
    backoff.next_delay(PollOutcome::Retryable);
    assert_eq!(backoff.next_delay(PollOutcome::Unexpected), minutes(30));
}
