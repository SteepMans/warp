//! The state of a running session on disk, so that a restart or an update of Ward does not
//! lose it.
//!
//! One file per session, `<root>/.agents/<first 8 characters of sessionId>/session.json`,
//! beside the files of the preparation phase: that directory is Ward's own, it is outside
//! every worktree and every vault, and one file per session means a write cut short costs at
//! most that one session. The write goes through a temporary file and a rename, so a kill in
//! the middle of it leaves either the old state or the new one — a crash of Ward is read back
//! the same way a clean exit is. The file lives as long as the session does and goes when it
//! ends, so a start of Ward finds exactly the sessions that were still going.
//!
//! Saved is only what cannot be found again: the assignment, the phase, the status, the agent
//! session with its process, the transcript being read and the position the service has
//! confirmed ([`super::journal::Journal::sent_position`]). Everything else is read off the
//! disk again (`prompt.md`, `runner.pid`, `exit-code`, `plan.json`, the worktree) or taken
//! from the service again.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::contract::{Assignment, SessionStatus};
use super::launch;

pub const STATE_FILE: &str = "session.json";

/// Bumped whenever the fields below change meaning; a file of another version is ignored, and
/// the session it belonged to is treated as one that did not survive.
pub const FORMAT_VERSION: u32 = 1;

/// Largest state file read back; a bigger one is not Ward's.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// How far a session had got when Ward went away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedPhase {
    /// The orchestrator of the preparation phase was running, or its plan was just taken.
    Prepare,
    /// The plan was ready and waited for the owner's `approve`.
    PlanReview,
    /// The worktree was being made: nothing of the agent had started yet.
    Worktree,
    /// The executor was running.
    Launched,
}

/// The agent session Ward had found for the launch running then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAgent {
    pub agent_session_id: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub remote_control_url: Option<String>,
}

/// Where the journal stood: the bytes of the transcript the service took and the `seq` of the
/// next chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedJournal {
    pub offset: u64,
    pub next_seq: u64,
}

/// The preparation phase of the session, when it had one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPrepare {
    pub dir: String,
    pub plan_path: String,
    pub vault_dir: String,
    pub limit_secs: u64,
}

/// A session as the next start of Ward reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSession {
    pub version: u32,
    pub session_id: String,
    /// Provider id (`claude`, `codex`).
    pub provider: String,
    pub phase: SavedPhase,
    pub status: SessionStatus,
    /// `reason` of the status, so a session blocked on a question of the owner comes back
    /// blocked for the same reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// Unix ms when the session was taken: `maxDurationMin` goes on counting from there.
    pub started_at_ms: i64,
    /// Unix ms of the launch running then; also what the agent's session is looked up by.
    pub launched_at_ms: i64,
    pub assignment: Assignment,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub worktree_note: Option<String>,
    #[serde(default)]
    pub runner_pid: Option<u32>,
    #[serde(default)]
    pub agent: Option<SavedAgent>,
    #[serde(default)]
    pub transcript: Option<String>,
    /// Whose transcript the journal was reading (`orchestrator`, `executor`).
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub journal: SavedJournal,
    #[serde(default)]
    pub prepare: Option<SavedPrepare>,
}

/// Ward's own directory of the session, `None` for a session id too odd for one.
pub fn dir(root: &Path, session_id: &str) -> Option<PathBuf> {
    launch::prepare_dir(root, session_id)
}

/// The state file of the session, `None` for a session id too odd for a directory name.
pub fn path(root: &Path, session_id: &str) -> Option<PathBuf> {
    Some(dir(root, session_id)?.join(STATE_FILE))
}

/// Writes the state through a temporary file, so a kill mid-write leaves no half a file.
pub fn write(path: &Path, state: &SavedSession) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let serialized = serde_json::to_string(state).map_err(io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serialized)?;
    std::fs::rename(&temp, path)
}

/// Reads one state file; `None` for a missing, oversized, unreadable or foreign one.
pub fn read(path: &Path) -> Option<SavedSession> {
    let size = std::fs::metadata(path).ok()?.len();
    if size == 0 || size > MAX_BYTES {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    let state: SavedSession = match serde_json::from_str(&raw) {
        Ok(state) => state,
        Err(err) => {
            log::warn!(
                "Ward agents: ignoring an unreadable session state {}: {err}",
                path.display()
            );
            return None;
        }
    };
    if state.version != FORMAT_VERSION || state.session_id.trim().is_empty() {
        return None;
    }
    Some(state)
}

/// Drops the state file of a session that is over.
pub fn remove(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => log::warn!(
            "Ward agents: could not remove the session state {}: {err}",
            path.display()
        ),
    }
}

/// Every session state under `root`, the one taken first in front.
pub fn scan(root: &Path) -> Vec<SavedSession> {
    let Ok(entries) = std::fs::read_dir(root.join(launch::AGENTS_DIR)) else {
        return Vec::new();
    };
    let mut states: Vec<SavedSession> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| read(&entry.path().join(STATE_FILE)))
        .collect();
    states.sort_by_key(|state| state.started_at_ms);
    states
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
