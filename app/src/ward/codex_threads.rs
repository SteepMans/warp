//! Root sessions and child threads of Codex.
//!
//! Codex writes every thread to its own rollout `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`,
//! and not every thread is a session the user can resume: a sub-agent the session spawns, the
//! Guardian thread that reviews a command before it runs and other helper threads have rollouts
//! too, and `codex resume <child id>` refuses them ("Cannot resume a Guardian review thread.
//! Resume its parent session instead: codex resume <parent id>"). A tab must therefore remember
//! the root session only.
//!
//! The first line of a rollout (`session_meta`) tells them apart. A child thread has any of:
//! - `parent_thread_id`, or `source.subagent.thread_spawn.parent_thread_id`;
//! - a `session_id` other than its own `id` (the session it belongs to);
//! - a `source` that is an object (`{"subagent": ...}`): Codex writes the sources a user starts
//!   (`cli`, `vscode`, `exec`, `mcp`) as plain strings;
//! - a `thread_source` other than `user` (`subagent`, `guardian`, ...).
//!
//! A root session has none of them; a fork (`forked_from_id`) is a root session of its own.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How far up a chain of child threads is followed.
const MAX_DEPTH: usize = 8;

/// Whether a rollout's thread is a session to resume or a child of one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadOrigin {
    Root,
    /// A child thread; `parent` is the thread it belongs to, when the rollout names it.
    Child {
        parent: Option<String>,
    },
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The id of the thread a `session_meta` payload describes.
pub fn thread_id(payload: &Value) -> Option<String> {
    non_empty(payload.get("id")).or_else(|| non_empty(payload.get("session_id")))
}

/// Reads the origin of a thread off its `session_meta` payload.
pub fn thread_origin(payload: &Value) -> ThreadOrigin {
    let id = thread_id(payload);
    let session_id = non_empty(payload.get("session_id"));
    let parent = non_empty(payload.get("parent_thread_id"))
        .or_else(|| {
            non_empty(
                payload
                    .pointer("/source/subagent/thread_spawn/parent_thread_id")
                    .or_else(|| payload.pointer("/source/subagent/parent_thread_id")),
            )
        })
        .or_else(|| {
            session_id
                .clone()
                .filter(|session| Some(session) != id.as_ref())
        })
        .filter(|parent| Some(parent) != id.as_ref());
    let source_is_child = payload.get("source").is_some_and(|source| {
        source.is_object()
            || source
                .as_str()
                .is_some_and(|source| source.to_ascii_lowercase().contains("guardian"))
    });
    let thread_source_is_child = non_empty(payload.get("thread_source"))
        .is_some_and(|thread_source| thread_source != "user");
    if parent.is_some() || source_is_child || thread_source_is_child {
        ThreadOrigin::Child { parent }
    } else {
        ThreadOrigin::Root
    }
}

/// The `session_meta` payload of a rollout's first line.
pub fn parse_session_meta(first_line: &str) -> Option<Value> {
    let mut line: Value = serde_json::from_str(first_line.trim()).ok()?;
    if line.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let payload = line.get_mut("payload")?.take();
    thread_id(&payload).is_some().then_some(payload)
}

pub fn read_session_meta(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut first_line = String::new();
    std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .ok()?;
    parse_session_meta(&first_line)
}

/// Whether a command line argument is a Codex thread id (a UUID).
pub fn looks_like_session_id(arg: &str) -> bool {
    arg.len() == 36
        && arg.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && arg.chars().filter(|c| *c == '-').count() == 4
}

/// The rollout of a thread: `rollout-<time>-<id>.jsonl` in a day directory.
pub fn find_rollout(sessions_dir: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        // Newest days first: the thread asked about is usually recent.
        dirs.sort();
        dirs.reverse();
        dirs
    };
    for year in subdirs(sessions_dir) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                let found = std::fs::read_dir(&day)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| name.ends_with(&suffix))
                    });
                if let Some(entry) = found {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

/// The root session a thread belongs to: follows the parents named in the rollouts. A thread
/// whose rollout is missing or names no parent is taken as it is.
pub fn root_session_id(sessions_dir: &Path, id: &str) -> String {
    let mut current = id.to_owned();
    let mut seen = vec![current.clone()];
    for _ in 0..MAX_DEPTH {
        let Some(payload) =
            find_rollout(sessions_dir, &current).and_then(|path| read_session_meta(&path))
        else {
            break;
        };
        let ThreadOrigin::Child {
            parent: Some(parent),
        } = thread_origin(&payload)
        else {
            break;
        };
        if seen.contains(&parent) {
            break;
        }
        seen.push(parent.clone());
        current = parent;
    }
    current
}

fn unquote(token: &str) -> &str {
    token.trim_matches(|c| c == '\'' || c == '"')
}

/// Whether a command runs `codex resume`.
fn is_codex_resume(command: &str) -> bool {
    let mut tokens = command.split_whitespace().map(unquote);
    let Some(program) = tokens.next() else {
        return false;
    };
    let program = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    (program == "codex" || program == "codex.exe") && tokens.next() == Some("resume")
}

/// The thread id a `codex resume ... <id>` command resumes.
pub fn resumed_id(command: &str) -> Option<&str> {
    if !is_codex_resume(command) {
        return None;
    }
    command
        .split_whitespace()
        .map(unquote)
        .rfind(|token| looks_like_session_id(token))
}

/// The command with the thread id `old` replaced by `new`.
pub fn with_session_id(command: &str, old: &str, new: &str) -> String {
    match command.rfind(old) {
        Some(at) => format!("{}{new}{}", &command[..at], &command[at + old.len()..]),
        None => format!("codex resume {new}"),
    }
}

/// A saved `codex resume <child id>` turned into the resume of the root session, when the id
/// is a child thread. `None`: nothing to change.
pub fn resume_root_command(sessions_dir: &Path, command: &str) -> Option<String> {
    let id = resumed_id(command)?;
    let root = root_session_id(sessions_dir, id);
    (root != id).then(|| with_session_id(command, id, &root))
}

/// [`resume_root_command`] with Codex's own sessions directory.
pub fn resume_root_command_here(command: &str) -> Option<String> {
    let changed = resume_root_command(&sessions_dir()?, command)?;
    log::info!("Ward: resuming the root Codex session instead of a child thread");
    Some(changed)
}

/// The parent session Codex names when it refuses to resume a child thread:
/// "Resume its parent session instead: codex resume <id>". The terminal may wrap the line
/// anywhere, so whitespace is ignored.
pub fn parent_from_resume_error(output: &str) -> Option<String> {
    const MARKER: &str = "Resumeitsparentsessioninstead:";
    let compact: String = output.chars().filter(|c| !c.is_whitespace()).collect();
    let after = &compact[compact.find(MARKER)? + MARKER.len()..];
    let after = after.strip_prefix("codexresume")?;
    let id: String = after
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
        .take(36)
        .collect();
    looks_like_session_id(&id).then_some(id)
}

/// The command to run after `command` (a `codex resume`) failed with `output` because it named
/// a child thread: the same command with the parent session. `None` when the failure was
/// something else.
pub fn retry_after_failed_resume(command: &str, output: &str) -> Option<String> {
    let old = resumed_id(command)?;
    let parent = parent_from_resume_error(output)?;
    (parent != old).then(|| with_session_id(command, old, &parent))
}

/// Codex's sessions directory: `$CODEX_HOME/sessions`, else `~/.codex/sessions`.
pub fn sessions_dir() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .map(|home| home.join("sessions"))
}

#[cfg(test)]
#[path = "codex_threads_tests.rs"]
mod tests;
