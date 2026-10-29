//! Detects a Codex CLI process running inside a terminal so that the session can be resumed
//! when the pane is restored after a restart.
//!
//! Unlike Claude Code, Codex has no registry of live processes. It records every session in a
//! rollout file `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl` whose first line is a
//! `session_meta` record with the session id and `cwd`, and it appends to that file as the
//! conversation goes on. The session of a running process is therefore the most recently
//! written rollout for the process's directory that was touched after the process started.
//! Two Codex processes in the same directory are indistinguishable this way; the most recently
//! active session wins.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::claude_resume::{ClaudeResumeInfo, quote_arg};
use crate::terminal::shell::ShellType;

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
const DROPPED_FLAGS_WITH_VALUE: &[&str] = &["--cd", "-C", "--image", "-i", "--remote", "--remote-auth-token-env"];

/// Subcommands that start an interactive session in the terminal. Anything else (`exec`,
/// `login`, `mcp`, ...) is not resumable.
const INTERACTIVE_SUBCOMMANDS: &[&str] = &["resume", "fork"];

/// Slack for clock and file system timestamp granularity when comparing a rollout's
/// modification time with the process start time.
const START_TIME_SLACK: Duration = Duration::from_secs(5);

#[derive(serde::Deserialize)]
struct RolloutLine {
    #[serde(rename = "type")]
    kind: String,
    payload: SessionMeta,
}

#[derive(serde::Deserialize)]
struct SessionMeta {
    id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    thread_source: Option<String>,
}

/// The parsed command line of a Codex process.
#[derive(Debug, PartialEq, Eq)]
struct CodexLaunch {
    preserved: Vec<String>,
    /// The session id passed to `codex resume <id>`, if any.
    resumed_id: Option<String>,
}

fn sessions_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join("sessions"))
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
    // The depth below the shell of each Codex process that runs inside this pane.
    let mut matches: Vec<(usize, Pid)> = Vec::new();
    for (pid, process) in system.processes() {
        if !is_codex_executable(&process.name().to_string_lossy()) {
            continue;
        }
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
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false, /* remove_dead_processes */
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    let process = system.process(pid)?;
    let argv: Vec<String> = process
        .cmd()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let launch = parse_launch(&argv)?;
    let process_cwd = process.cwd().map(Path::to_path_buf);
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(process.start_time());

    let (session_id, cwd) = latest_session(&sessions_dir()?, started, process_cwd.as_deref())
        .or_else(|| {
            launch
                .resumed_id
                .clone()
                .map(|id| (id, process_cwd.as_ref().map(|cwd| cwd.display().to_string())))
        })?;
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
    })
}

fn is_known_subcommand(arg: &str) -> bool {
    matches!(
        arg,
        "exec" | "e" | "review" | "login" | "logout" | "mcp" | "mcp-server" | "app-server"
            | "completion" | "sandbox" | "debug" | "apply" | "a" | "cloud" | "features" | "help"
    )
}

fn looks_like_session_id(arg: &str) -> bool {
    arg.len() == 36 && arg.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Finds the most recently written top-level session in `cwd` among rollouts touched since
/// `started`. Returns the session id and the session's directory.
fn latest_session(
    sessions_dir: &Path,
    started: SystemTime,
    cwd: Option<&Path>,
) -> Option<(String, Option<String>)> {
    let threshold = started.checked_sub(START_TIME_SLACK).unwrap_or(started);
    // Rollouts live in day directories named after their creation time; only the days since
    // the process started (plus one for time zones) can hold a session it created. A session
    // resumed from an older day is covered by the `resume <id>` fallback in `detect`.
    let first_day = day_key(threshold.checked_sub(Duration::from_secs(24 * 60 * 60))?);

    let mut best: Option<(SystemTime, String, Option<String>)> = None;
    for path in day_dirs_since(sessions_dir, &first_day) {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let Some(modified) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            if modified < threshold || best.as_ref().is_some_and(|(time, ..)| *time >= modified) {
                continue;
            }
            let Some(meta) = read_session_meta(&path) else {
                continue;
            };
            if meta.thread_source.as_deref() == Some("subagent") {
                continue;
            }
            if let (Some(cwd), Some(session_cwd)) = (cwd, meta.cwd.as_deref()) {
                if !same_dir(cwd, Path::new(session_cwd)) {
                    continue;
                }
            }
            best = Some((modified, meta.id, meta.cwd));
        }
    }
    best.map(|(_, id, cwd)| (id, cwd))
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

fn read_session_meta(path: &Path) -> Option<SessionMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mut first_line = String::new();
    std::io::BufReader::new(file).read_line(&mut first_line).ok()?;
    let line: RolloutLine = serde_json::from_str(&first_line).ok()?;
    (line.kind == "session_meta" && !line.payload.id.is_empty()).then_some(line.payload)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .replace('\\', "/")
            .to_lowercase()
    };
    normalize(a) == normalize(b)
}

/// Builds `codex resume <preserved flags> <session id>`.
fn build_resume_command(preserved: &[String], session_id: &str, shell_type: Option<ShellType>) -> String {
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
