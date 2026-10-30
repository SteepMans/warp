//! When to poll the announcements feed next, given how the last poll went.

use std::time::Duration;

/// Regular poll interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Longest interval after repeated failures.
pub const MAX_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Result of one poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// 2xx with a feed in it.
    Fetched,
    /// Network error, timeout, 429 or 5xx: worth retrying later, more slowly.
    Retryable,
    /// Any other status, or a body that is not a feed: keep the current pace.
    Unexpected,
}

impl PollOutcome {
    pub fn from_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Fetched,
            429 | 500..=599 => Self::Retryable,
            _ => Self::Unexpected,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    interval: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            interval: POLL_INTERVAL,
        }
    }
}

impl Backoff {
    /// Updates the interval after a poll and returns the delay until the next one.
    pub fn next_delay(&mut self, outcome: PollOutcome) -> Duration {
        match outcome {
            PollOutcome::Fetched => self.interval = POLL_INTERVAL,
            PollOutcome::Retryable => self.interval = (self.interval * 2).min(MAX_INTERVAL),
            PollOutcome::Unexpected => {}
        }
        self.interval
    }
}

#[cfg(test)]
#[path = "backoff_tests.rs"]
mod tests;
