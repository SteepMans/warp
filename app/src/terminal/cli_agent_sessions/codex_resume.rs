//! Detects a Codex CLI process running inside a terminal so that the session can be resumed
//! when the pane is restored after a restart.
//!
//! Unlike Claude Code, Codex has no registry of live processes. It records every session in a
//! rollout file `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl` whose first line is a
//! `session_meta` record with the session id and `cwd`, and it appends to that file as the
//! conversation goes on. Which of them a running process holds is decided in
//! [`crate::ward::codex_session`]: the files the process holds open, the id it was resumed with,
//! and only then the rollouts of its directory that no other running Codex process holds.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::claude_resume::{ClaudeResumeInfo, quote_arg};
use crate::terminal::shell::ShellType;
use crate::ward::codex_session::{self, Launch, ProcessKey, Rollout, START_TIME_SLACK};
use crate::ward::codex_threads;

/// Flags carried over to `codex resume`, along with whether they take a value. Only flags that
/// `codex resume` accepts are listed.
const PRESERVED_FLAGS: &[(&str, bool)] = &[
    ("--model", true),
    ("-m", true),
    ("--config", true),
    ("-c", true),
    ("--profile", true),
    ("-p", true),
    ("--sandbox", true),
    ("-s", true),
    ("--ask-for-approval", true),
    ("-a", true),
    ("--add-dir", true),
    ("--enable", true),
    ("--disable", true),
    ("--local-provider", true),
    ("--oss", false),
    ("--search", false),
    ("--no-alt-screen", false),
    ("--approve-for-me", false),
    ("--dangerously-bypass-approvals-and-sandbox", false),
];

/// Flags of the original launch whose value has to be skipped together with the flag.
const DROPPED_FLAGS_WITH_VALUE: &[&str] = &[
    "--cd",
    "-C",
    "--image",
    "-i",
    "--remote",
    "--remote-auth-token-env",
];

/// Subcommands that start an interactive session in the terminal. Anything else (`exec`,
/// `login`, `mcp`, ...) is not resumable.
const INTERACTIVE_SUBCOMMANDS: &[&str] = &["resume", "fork"];

/// The parsed command line of a Codex process.
#[derive(Debug, PartialEq, Eq)]
struct CodexLaunch {
    preserved: Vec<String>,
    /// The session id passed to `codex resume <id>` (or `codex fork <id>`), if any.
    resumed_id: Option<String>,
    /// `resume` or `fork`; `None` for a plain `codex`.
    subcommand: Option<String>,
}

fn sessions_dir() -> Option<PathBuf> {
    codex_threads::sessions_dir()
}

/// Looks for a Codex process in the process tree of the given shell and returns the command
/// that resumes its session.
///
/// It takes one cheap snapshot of the process table and only reads rollout files written since
/// the Codex process started, so it can run periodically for every pane. It still makes
/// syscalls, so call it off the main thread.
#[cfg(not(target_family = "wasm"))]
pub fn detect(shell_pid: u32, shell_type: Option<ShellType>) -> Option<ClaudeResumeInfo> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    /// Bounds the parent walk; Codex is normally a direct child or grandchild of the shell.
    const MAX_DEPTH: usize = 16;

    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true, /* remove_dead_processes */
        ProcessRefreshKind::nothing(),
    );

    let shell_pid = Pid::from_u32(shell_pid);
    let codex_pids: Vec<Pid> = system
        .processes()
        .iter()
        .filter(|(_, process)| is_codex_executable(&process.name().to_string_lossy()))
        .map(|(pid, _)| *pid)
        .collect();
    // The depth below the shell of each Codex process that runs inside this pane.
    let mut matches: Vec<(usize, Pid)> = Vec::new();
    for pid in &codex_pids {
        let mut current = *pid;
        for depth in 0..MAX_DEPTH {
            let Some(parent) = system.process(current).and_then(|process| process.parent()) else {
                break;
            };
            if parent == shell_pid {
                matches.push((depth, *pid));
                break;
            }
            current = parent;
        }
    }

    // Prefer the outermost Codex process: the CLI spawns helper processes of its own.
    matches.sort();
    let (_, pid) = matches.into_iter().next()?;
    // The command lines of all Codex processes: the sessions the others were resumed with are
    // not this pane's.
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&codex_pids),
        false, /* remove_dead_processes */
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false, /* remove_dead_processes */
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    let argv_of = |pid: Pid| -> Option<Vec<String>> {
        Some(
            system
                .process(pid)?
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        )
    };
    let process = system.process(pid)?;
    let launch = parse_launch(&argv_of(pid)?)?;
    let process_cwd = process.cwd().map(Path::to_path_buf);
    let key = ProcessKey {
        pid: pid.as_u32(),
        started: process.start_time(),
    };
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(key.started);

    let sessions_dir = sessions_dir()?;
    let others: Vec<(ProcessKey, Option<String>)> = codex_pids
        .iter()
        .filter(|other| **other != pid)
        .filter_map(|other| {
            let process = system.process(*other)?;
            let resumed = argv_of(*other)
                .and_then(|argv| parse_launch(&argv))
                .filter(|launch| launch.subcommand.as_deref() == Some("resume"))
                .and_then(|launch| launch.resumed_id);
            Some((
                ProcessKey {
                    pid: other.as_u32(),
                    started: process.start_time(),
                },
                resumed,
            ))
        })
        .collect();
    let session_launch = match launch.subcommand.as_deref() {
        Some("resume") => Launch::Resume(
            launch
                .resumed_id
                .as_deref()
                // A resumed child thread stands for its root session.
                .map(|id| codex_threads::root_session_id(&sessions_dir, id)),
        ),
        _ => Launch::New,
    };
    let mut open: Vec<String> = Vec::new();
    for id in codex_session::open_thread_ids(key.pid).unwrap_or_default() {
        let root = codex_threads::root_session_id(&sessions_dir, &id);
        if !open.contains(&root) {
            open.push(root);
        }
    }
    let rollouts = recent_rollouts(&sessions_dir, started);
    let session_id = codex_session::assign(
        key,
        &session_launch,
        &open,
        &rollouts,
        process_cwd.as_deref(),
        &others,
    )?;
    let cwd = rollouts
        .iter()
        .find(|rollout| rollout.id == session_id)
        .and_then(|rollout| rollout.cwd.clone())
        .or_else(|| process_cwd.as_ref().map(|cwd| cwd.display().to_string()));
    Some(ClaudeResumeInfo {
        command: build_resume_command(&launch.preserved, &session_id, shell_type),
        session_id,
        cwd,
    })
}

#[cfg(target_family = "wasm")]
pub fn detect(_shell_pid: u32, _shell_type: Option<ShellType>) -> Option<ClaudeResumeInfo> {
    None
}

fn is_codex_executable(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "codex" || name == "codex.exe"
}

/// Parses the arguments of a Codex process. Returns `None` for non-interactive invocations.
fn parse_launch(argv: &[String]) -> Option<CodexLaunch> {
    let mut preserved = Vec::new();
    let mut resumed_id = None;
    let mut subcommand: Option<&str> = None;
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let (flag, inline_value) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg.as_str(), None),
        };
        if !flag.starts_with('-') {
            match subcommand {
                // The first positional argument is either a subcommand or the initial prompt.
                None if INTERACTIVE_SUBCOMMANDS.contains(&flag) => subcommand = Some(flag),
                None if is_known_subcommand(flag) => return None,
                Some(_) if resumed_id.is_none() && looks_like_session_id(flag) => {
                    resumed_id = Some(flag.to_owned());
                }
                // Prompts are dropped so that resuming does not resubmit them.
                _ => {}
            }
            continue;
        }
        if DROPPED_FLAGS_WITH_VALUE.contains(&flag) {
            if inline_value.is_none() {
                iter.next();
            }
            continue;
        }
        let Some((_, takes_value)) = PRESERVED_FLAGS.iter().find(|(name, _)| *name == flag) else {
            continue;
        };
        preserved.push(flag.to_owned());
        if let Some(value) = inline_value {
            preserved.push(value.to_owned());
        } else if *takes_value {
            preserved.extend(iter.next().cloned());
        }
    }
    Some(CodexLaunch {
        preserved,
        resumed_id,
        subcommand: subcommand.map(str::to_owned),
    })
}

fn is_known_subcommand(arg: &str) -> bool {
    matches!(
        arg,
        "exec"
            | "e"
            | "review"
            | "login"
            | "logout"
            | "mcp"
            | "mcp-server"
            | "app-server"
            | "completion"
            | "sandbox"
            | "debug"
            | "apply"
            | "a"
            | "cloud"
            | "features"
            | "help"
    )
}

fn looks_like_session_id(arg: &str) -> bool {
    codex_threads::looks_like_session_id(arg)
}

/// The rollouts written since `started`, a child thread (a sub-agent, a Guardian review) under
/// the root session it belongs to; one that names no parent is skipped.
fn recent_rollouts(sessions_dir: &Path, started: SystemTime) -> Vec<Rollout> {
    let threshold = started.checked_sub(START_TIME_SLACK).unwrap_or(started);
    // Rollouts live in day directories named after their creation time; only the days since
    // the process started (plus one for time zones) can hold a session it created. A session
    // resumed from an older day is known by its `resume <id>`.
    let Some(first_day) = threshold
        .checked_sub(Duration::from_secs(24 * 60 * 60))
        .map(day_key)
    else {
        return Vec::new();
    };

    let mut rollouts = Vec::new();
    for path in day_dirs_since(sessions_dir, &first_day) {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if modified < threshold {
                continue;
            }
            let Some(meta) = codex_threads::read_session_meta(&path) else {
                continue;
            };
            let cwd = meta
                .get("cwd")
                .and_then(|cwd| cwd.as_str())
                .map(str::to_owned);
            let (id, created) = match codex_threads::thread_origin(&meta) {
                codex_threads::ThreadOrigin::Root => (
                    codex_threads::thread_id(&meta),
                    Some(
                        created_at(&meta)
                            .or_else(|| metadata.created().ok())
                            .unwrap_or(modified),
                    ),
                ),
                codex_threads::ThreadOrigin::Child { parent } => (
                    parent.map(|parent| codex_threads::root_session_id(sessions_dir, &parent)),
                    None,
                ),
            };
            let Some(id) = id else {
                continue;
            };
            rollouts.push(Rollout {
                id,
                cwd,
                created,
                modified,
            });
        }
    }
    rollouts
}

/// The creation time `session_meta` records (`timestamp`).
fn created_at(meta: &serde_json::Value) -> Option<SystemTime> {
    let timestamp = meta.get("timestamp")?.as_str()?;
    let datetime = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    Some(datetime.with_timezone(&chrono::Utc).into())
}

/// `YYYY/MM/DD` of a timestamp in UTC, matching the layout of the sessions directory.
fn day_key(time: SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.format("%Y/%m/%d").to_string()
}

/// The `YYYY/MM/DD` directories under `root` that are not older than `first_day`.
fn day_dirs_since(root: &Path, first_day: &str) -> Vec<PathBuf> {
    let subdirs = |dir: &Path| -> Vec<(String, PathBuf)> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
            .collect()
    };
    let mut days = Vec::new();
    for (year, year_path) in subdirs(root) {
        if year.as_str() < &first_day[..4] {
            continue;
        }
        for (month, month_path) in subdirs(&year_path) {
            if format!("{year}/{month}").as_str() < &first_day[..7] {
                continue;
            }
            for (day, day_path) in subdirs(&month_path) {
                if format!("{year}/{month}/{day}").as_str() >= first_day {
                    days.push(day_path);
                }
            }
        }
    }
    days
}

/// Builds `codex resume <preserved flags> <session id>`.
fn build_resume_command(
    preserved: &[String],
    session_id: &str,
    shell_type: Option<ShellType>,
) -> String {
    let mut command = String::from("codex resume");
    for arg in preserved.iter().map(String::as_str).chain([session_id]) {
        command.push(' ');
        command.push_str(&quote_arg(arg, shell_type));
    }
    command
}

#[cfg(test)]
#[path = "codex_resume_tests.rs"]
mod tests;
