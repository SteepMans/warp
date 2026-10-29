//! Detects a Claude Code process running inside a terminal so that the session can be resumed
//! when the pane is restored after a restart.
//!
//! Claude Code keeps a registry of its live interactive processes in
//! `~/.claude/sessions/<pid>.json`, where each file records the process's current `sessionId`
//! and `cwd`. The file is rewritten when the session id changes (e.g. after `/clear` or
//! `/resume`), so it is the authoritative mapping from a process to the conversation it is
//! running. We find candidate processes by walking the shell's process tree, which keeps the
//! lookup scoped to this pane and to this machine.

use std::path::PathBuf;

use crate::terminal::shell::ShellType;

/// Everything needed to resume a Claude Code session in a restored pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeResumeInfo {
    pub session_id: String,
    /// The directory Claude Code was running in. Sessions are scoped to a project directory, so
    /// the resume command must run from here.
    pub cwd: Option<String>,
    /// The shell command that resumes the session, including the original launch flags.
    pub command: String,
}

/// Flags that are carried over to the resume command, along with whether they take a value.
const PRESERVED_FLAGS: &[(&str, FlagValue)] = &[
    ("--remote-control", FlagValue::Optional),
    ("--remote-control-session-name-prefix", FlagValue::Required),
    ("--model", FlagValue::Required),
    ("--fallback-model", FlagValue::Required),
    ("--effort", FlagValue::Required),
    ("--agent", FlagValue::Required),
    ("--permission-mode", FlagValue::Required),
    ("--settings", FlagValue::Required),
    ("--mcp-config", FlagValue::Variadic),
    ("--add-dir", FlagValue::Variadic),
    ("--plugin-dir", FlagValue::Required),
    ("--dangerously-skip-permissions", FlagValue::None),
    ("--allow-dangerously-skip-permissions", FlagValue::None),
    ("--strict-mcp-config", FlagValue::None),
    ("--chrome", FlagValue::None),
    ("--no-chrome", FlagValue::None),
    ("--ide", FlagValue::None),
    ("--verbose", FlagValue::None),
    ("--brief", FlagValue::None),
];

/// Flags that describe how the original session was selected or are only meaningful for a
/// one-shot run. They are dropped (with their values) and replaced by `--resume <id>`.
const DROPPED_FLAGS_WITH_VALUE: &[&str] = &["--session-id", "--name", "-n"];
const DROPPED_FLAGS_WITH_OPTIONAL_VALUE: &[&str] = &["--resume", "-r"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlagValue {
    None,
    Optional,
    Required,
    Variadic,
}

/// The contents of `~/.claude/sessions/<pid>.json` that we rely on.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRegistryEntry {
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

fn sessions_registry_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("sessions"))
}

fn read_registry_entry(pid: u32) -> Option<SessionRegistryEntry> {
    let path = sessions_registry_dir()?.join(format!("{pid}.json"));
    let contents = std::fs::read_to_string(path).ok()?;
    let entry: SessionRegistryEntry = serde_json::from_str(&contents).ok()?;
    // Only interactive sessions can be resumed in a terminal; skip `-p` runs and SDK hosts.
    let is_interactive = entry
        .kind
        .as_deref()
        .is_none_or(|kind| kind == "interactive");
    (is_interactive && !entry.session_id.is_empty()).then_some(entry)
}

/// Looks for a Claude Code process in the process tree of the given shell and returns the
/// command that resumes its session.
///
/// This starts from the (few) processes listed in Claude Code's session registry and walks up
/// their parent chains in a single snapshot of the process table, so it stays cheap enough to
/// run periodically for every pane. It still makes syscalls, so call it off the main thread.
#[cfg(not(target_family = "wasm"))]
pub fn detect(shell_pid: u32, shell_type: Option<ShellType>) -> Option<ClaudeResumeInfo> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    /// Bounds the parent walk; Claude Code is normally a direct child or grandchild of the shell.
    const MAX_DEPTH: usize = 16;

    let registry_pids: Vec<u32> = std::fs::read_dir(sessions_registry_dir()?)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            path.file_stem()?.to_str()?.parse().ok()
        })
        .filter(|pid| *pid != shell_pid)
        .collect();
    if registry_pids.is_empty() {
        return None;
    }

    // A single snapshot of the process table with no per-process details is cheap; it is only
    // used for the parent links.
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true, /* remove_dead_processes */
        ProcessRefreshKind::nothing(),
    );

    let shell_pid = Pid::from_u32(shell_pid);
    // The depth below the shell of each registered process that runs inside this pane.
    let mut matches: Vec<(usize, Pid)> = Vec::new();
    for pid in registry_pids.into_iter().map(Pid::from_u32) {
        let mut current = pid;
        for depth in 0..MAX_DEPTH {
            let Some(parent) = system.process(current).and_then(|process| process.parent()) else {
                break;
            };
            if parent == shell_pid {
                matches.push((depth, pid));
                break;
            }
            current = parent;
        }
    }

    // Prefer the outermost Claude Code process if several are nested.
    matches.sort();
    matches.into_iter().find_map(|(_, pid)| {
        let entry = read_registry_entry(pid.as_u32())?;
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false, /* remove_dead_processes */
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        let argv: Vec<String> = system
            .process(pid)?
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        // Also guards against a stale registry file whose pid was reused by another process.
        let args = claude_args_from_argv(&argv)?;
        Some(ClaudeResumeInfo {
            command: build_resume_command(args, &entry.session_id, shell_type),
            session_id: entry.session_id,
            cwd: entry.cwd,
        })
    })
}

#[cfg(target_family = "wasm")]
pub fn detect(_shell_pid: u32, _shell_type: Option<ShellType>) -> Option<ClaudeResumeInfo> {
    None
}

/// Returns the arguments passed to Claude Code itself, skipping the executable and, for
/// script-based installs, the interpreter (`node cli.js ...`).
fn claude_args_from_argv(argv: &[String]) -> Option<&[String]> {
    let index = argv.iter().position(|arg| {
        let file_name = arg.rsplit(['/', '\\']).next().unwrap_or(arg);
        let file_name = file_name.to_ascii_lowercase();
        file_name.starts_with("claude") || file_name == "cli.js" || file_name == "cli.mjs"
    })?;
    Some(&argv[index + 1..])
}

/// Builds `claude <preserved flags> --resume <session id>` from the original arguments.
///
/// Positional arguments (the initial prompt) are dropped so that resuming does not resubmit it.
fn build_resume_command(
    args: &[String],
    session_id: &str,
    shell_type: Option<ShellType>,
) -> String {
    let mut preserved: Vec<String> = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let (flag, inline_value) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg.as_str(), None),
        };
        let next_is_value = |iter: &mut std::iter::Peekable<std::slice::Iter<'_, String>>| {
            iter.peek().is_some_and(|next| !next.starts_with('-'))
        };

        if DROPPED_FLAGS_WITH_VALUE.contains(&flag) {
            if inline_value.is_none() {
                iter.next();
            }
            continue;
        }
        if DROPPED_FLAGS_WITH_OPTIONAL_VALUE.contains(&flag) {
            if inline_value.is_none() && next_is_value(&mut iter) {
                iter.next();
            }
            continue;
        }

        let Some((_, value_kind)) = PRESERVED_FLAGS.iter().find(|(name, _)| *name == flag) else {
            // Unknown flags and positional arguments are dropped. A dropped flag's value (if
            // any) is then seen as a positional argument and dropped as well.
            continue;
        };

        preserved.push(flag.to_owned());
        if let Some(value) = inline_value {
            preserved.push(value.to_owned());
            continue;
        }
        match value_kind {
            FlagValue::None => {}
            FlagValue::Optional => {
                if next_is_value(&mut iter) {
                    preserved.extend(iter.next().cloned());
                }
            }
            FlagValue::Required => preserved.extend(iter.next().cloned()),
            FlagValue::Variadic => {
                while next_is_value(&mut iter) {
                    preserved.extend(iter.next().cloned());
                }
            }
        }
    }

    let mut command = String::from("claude");
    for arg in preserved
        .iter()
        .map(String::as_str)
        .chain(["--resume", session_id])
    {
        command.push(' ');
        command.push_str(&quote_arg(arg, shell_type));
    }
    command
}

pub(super) fn quote_arg(arg: &str, shell_type: Option<ShellType>) -> String {
    let is_plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='));
    if is_plain {
        return arg.to_owned();
    }
    match shell_type {
        Some(ShellType::PowerShell) => format!("'{}'", arg.replace('\'', "''")),
        _ => format!("'{}'", arg.replace('\'', r"'\''")),
    }
}

#[cfg(test)]
#[path = "claude_resume_tests.rs"]
mod tests;
