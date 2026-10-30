//! Ends a CLI agent session when its process is gone, for agents that never ran as a command
//! block.
//!
//! Warp ends a CLI agent session when the command block that ran the agent finishes. An agent
//! started from a shell profile (`~/.bashrc` and similar) runs before the shell bootstrap
//! completes, so there is no such block: after the agent exits, its session would stay in its
//! last state forever. The pane already polls its shell's process tree for a running Claude Code
//! or Codex process (see `poll_claude_resume` in the terminal view); this tracker turns that poll
//! into an end-of-session signal.

/// Polls in a row without the agent process before its session is ended. One miss is not
/// enough: the process registry can be read mid-rewrite.
pub const MISSED_POLLS_TO_END: u32 = 2;

/// Tracks whether the agent process seen in a pane's process tree is still there.
#[derive(Debug, Default)]
pub struct AgentProcessLiveness {
    /// Session ids of the agent processes seen since the agent last went missing. Kept as a list
    /// because `/clear` gives the running process a new session id while the plugin can still
    /// report the old one.
    seen_session_ids: Vec<String>,
    missed_polls: u32,
}

impl AgentProcessLiveness {
    /// Records one poll of the process tree: the session id of the running agent, or `None` when
    /// no agent process was found.
    ///
    /// Returns the session ids whose process has been gone for [`MISSED_POLLS_TO_END`] polls,
    /// once. A session should be ended only if its id is among them, so a new agent started in
    /// the same pane in the meantime is left alone.
    pub fn observe(&mut self, running_session_id: Option<&str>) -> Option<Vec<String>> {
        match running_session_id {
            Some(session_id) => {
                self.missed_polls = 0;
                if !self.seen_session_ids.iter().any(|id| id == session_id) {
                    self.seen_session_ids.push(session_id.to_owned());
                }
                None
            }
            None if self.seen_session_ids.is_empty() => None,
            None => {
                self.missed_polls += 1;
                if self.missed_polls < MISSED_POLLS_TO_END {
                    return None;
                }
                self.missed_polls = 0;
                Some(std::mem::take(&mut self.seen_session_ids))
            }
        }
    }
}

/// Whether a session that reports `session_id` belongs to one of the exited processes.
pub fn session_has_exited(session_id: Option<&str>, exited_session_ids: &[String]) -> bool {
    session_id.is_some_and(|session_id| exited_session_ids.iter().any(|id| id == session_id))
}

#[cfg(test)]
#[path = "agent_liveness_tests.rs"]
mod tests;
