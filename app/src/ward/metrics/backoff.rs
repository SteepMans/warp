//! When to push next, given how the last push went.

use std::time::Duration;

/// Regular push interval.
pub const PUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Longest interval after repeated failures.
pub const MAX_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Result of one push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// 2xx.
    Accepted,
    /// Network error, timeout, 429 or 5xx: worth retrying later, more slowly.
    Retryable,
    /// 401 or 403: the token is wrong, retrying cannot help.
    Rejected,
    /// Any other status: keep the current pace.
    Unexpected,
}

impl PushOutcome {
    pub fn from_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Accepted,
            401 | 403 => Self::Rejected,
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
            interval: PUSH_INTERVAL,
        }
    }
}

impl Backoff {
    /// Updates the interval after a push and returns the delay until the next one, or `None`
    /// when pushing should stop until the app restarts.
    pub fn next_delay(&mut self, outcome: PushOutcome) -> Option<Duration> {
        match outcome {
            PushOutcome::Accepted => self.interval = PUSH_INTERVAL,
            PushOutcome::Retryable => self.interval = (self.interval * 2).min(MAX_INTERVAL),
            PushOutcome::Unexpected => {}
            PushOutcome::Rejected => return None,
        }
        Some(self.interval)
    }
}

#[cfg(test)]
#[path = "backoff_tests.rs"]
mod tests;
