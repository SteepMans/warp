//! Metrics state that outlives a run: the installation id, the cumulative counters and the
//! facts needed to classify the previous run (clean exit, panic, update).
//!
//! The state is a JSON file in the app's state directory. A panic hook cannot safely take the
//! state lock or rewrite that file, so panics go to a separate marker file holding the number
//! of panics of the current run; the next start folds it into the counters and deletes it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STATE_FILE_NAME: &str = "ward_metrics.json";
const PANIC_MARKER_FILE_NAME: &str = "ward_metrics_panic";

/// Where the state and the panic marker live.
#[derive(Clone, Debug)]
pub struct StatePaths {
    pub state: PathBuf,
    pub panic_marker: PathBuf,
}

impl StatePaths {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            state: dir.join(STATE_FILE_NAME),
            panic_marker: dir.join(PANIC_MARKER_FILE_NAME),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrashCounts {
    pub panic: u64,
    pub unclean_exit: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateCounts {
    pub success: u64,
    pub failure: u64,
}

/// Agent label of a restored pane; `None` is a plain terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoredAgent {
    Claude,
    Codex,
    Other,
    None,
}

impl RestoredAgent {
    pub const ALL: [RestoredAgent; 4] = [Self::Claude, Self::Codex, Self::Other, Self::None];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Other => "other",
            Self::None => "none",
        }
    }

    /// Classifies a restored pane by the command that resumes its CLI agent session.
    pub fn of_resume_command(command: Option<&str>) -> Self {
        let Some(command) = command else {
            return Self::None;
        };
        let program = command
            .trim_start_matches(['&', ' ', '\t'])
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_matches(['"', '\''])
            .replace('\\', "/");
        let program = program.rsplit('/').next().unwrap_or_default().to_lowercase();
        let program = program
            .strip_suffix(".exe")
            .or_else(|| program.strip_suffix(".cmd"))
            .or_else(|| program.strip_suffix(".ps1"))
            .unwrap_or(&program);
        match program {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RestoredCounts {
    pub claude: u64,
    pub codex: u64,
    pub other: u64,
    pub none: u64,
}

impl RestoredCounts {
    pub fn get(&self, agent: RestoredAgent) -> u64 {
        match agent {
            RestoredAgent::Claude => self.claude,
            RestoredAgent::Codex => self.codex,
            RestoredAgent::Other => self.other,
            RestoredAgent::None => self.none,
        }
    }

    pub fn increment(&mut self, agent: RestoredAgent) {
        let counter = match agent {
            RestoredAgent::Claude => &mut self.claude,
            RestoredAgent::Codex => &mut self.codex,
            RestoredAgent::Other => &mut self.other,
            RestoredAgent::None => &mut self.none,
        };
        *counter += 1;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsState {
    /// Random lowercase UUIDv4 identifying this installation.
    pub installation_id: String,
    pub launches: u64,
    pub crashes: CrashCounts,
    pub update_installs: UpdateCounts,
    pub restored_sessions: RestoredCounts,
    /// Version of the previous run.
    pub last_seen_version: Option<String>,
    /// Set while the app runs, cleared on a clean exit: still set at startup means the
    /// previous run ended without one.
    pub run_in_progress: bool,
}

/// How the previous run ended, as determined at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviousRun {
    FirstRun,
    Clean,
    Panicked,
    Unclean,
}

/// Facts about the environment at startup.
#[derive(Clone, Debug)]
pub struct StartupFacts<'a> {
    pub current_version: &'a str,
    /// Panics recorded in the marker file by previous runs.
    pub marker_panics: u64,
    /// The installer of an autoupdate left its log for this start.
    pub update_log_found: bool,
}

impl MetricsState {
    /// Accounts for the previous run and starts a new one. `new_id` creates the installation
    /// id when there is none yet.
    pub fn begin_run(
        &mut self,
        facts: &StartupFacts<'_>,
        new_id: impl FnOnce() -> String,
    ) -> PreviousRun {
        let first_run = self.installation_id.is_empty();
        if first_run {
            self.installation_id = new_id();
        }

        let previous_run = if facts.marker_panics > 0 {
            self.crashes.panic += facts.marker_panics;
            PreviousRun::Panicked
        } else if self.run_in_progress {
            self.crashes.unclean_exit += 1;
            PreviousRun::Unclean
        } else if first_run {
            PreviousRun::FirstRun
        } else {
            PreviousRun::Clean
        };

        match self.last_seen_version.as_deref() {
            Some(last) if last != facts.current_version => self.update_installs.success += 1,
            // The installer ran but the version did not change: the update did not install.
            Some(_) if facts.update_log_found => self.update_installs.failure += 1,
            _ => {}
        }
        self.last_seen_version = Some(facts.current_version.to_owned());

        self.launches += 1;
        self.run_in_progress = true;
        previous_run
    }

    /// Records a clean exit.
    pub fn end_run(&mut self) {
        self.run_in_progress = false;
    }
}

/// Loads the state; a missing or unreadable file starts from scratch.
pub fn load(path: &Path) -> MetricsState {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            log::warn!("Ignoring unreadable Ward metrics state: {err}");
            MetricsState::default()
        }),
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                log::warn!("Failed to read Ward metrics state: {err}");
            }
            MetricsState::default()
        }
    }
}

/// Saves the state through a temporary file so a crash mid-write keeps the previous state.
pub fn save(path: &Path, state: &MetricsState) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let serialized = serde_json::to_string(state).map_err(io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serialized)?;
    fs::rename(&temp, path)
}

/// Number of panics in the marker file, 0 when there is none.
pub fn read_panic_marker(path: &Path) -> u64 {
    match fs::read_to_string(path) {
        // An unparsable marker still means at least one panic.
        Ok(raw) => raw.trim().parse().unwrap_or(1).max(1),
        Err(_) => 0,
    }
}

pub fn write_panic_marker(path: &Path, panics: u64) -> io::Result<()> {
    fs::write(path, panics.to_string())
}

pub fn remove_panic_marker(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => log::warn!("Failed to remove the Ward panic marker: {err}"),
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
