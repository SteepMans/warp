//! Where a session lives on disk and how its tab starts the agent.
//!
//! A clone is `<root>/<owner>/<repo>`, the session's worktree `<clone>/.worktrees/<first 8
//! characters of sessionId>`. Ward's files of the session are in `<worktree>/.agents/`: the
//! prompt, the runner script, the runner's pid and, once the agent exits, its exit code.
//!
//! The tab runs the runner (pwsh on Windows, sh elsewhere), not the agent directly: the runner
//! clears the inherited agent variables, starts the agent with the prompt from the file as its
//! first message and writes the agent's exit code to `exit-code`, which Ward watches.

use std::path::{Path, PathBuf};

use super::contract::TaskRef;
use super::provider::AgentCommand;

pub const AGENTS_DIR: &str = ".agents";
pub const WORKTREES_DIR: &str = ".worktrees";
pub const PROMPT_FILE: &str = "prompt.md";
pub const EXIT_CODE_FILE: &str = "exit-code";
pub const RUNNER_PID_FILE: &str = "runner.pid";

/// Characters of the session id in the worktree directory name.
const WORKTREE_ID_CHARS: usize = 8;

/// `owner/repo` of a repository URL.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RepoId {
    pub owner: String,
    pub name: String,
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The last two path segments of `https://host/owner/repo(.git)`, `git@host:owner/repo.git`,
/// `host/owner/repo` or a local path; `None` when they are not plain names.
pub fn parse_repo(url: &str) -> Option<RepoId> {
    let url = url.trim().trim_end_matches('/');
    let path = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url.rsplit_once(':').map_or(url, |(host, path)| {
            // `git@host:owner/repo`, but not a Windows drive (`C:\...`).
            if host.len() == 1 { url } else { path }
        }),
    };
    let mut segments = path
        .rsplit(['/', '\\'])
        .filter(|segment| !segment.is_empty());
    let name = segments.next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    let owner = segments.next()?;
    (is_safe_segment(owner) && is_safe_segment(name)).then(|| RepoId {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// Whether `repo` passes `allowedRepos` (comma separated `owner/repo`, empty: all).
pub fn is_repo_allowed(repo: &RepoId, allowed_repos: &str) -> bool {
    let allowed: Vec<&str> = allowed_repos
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    allowed.is_empty()
        || allowed
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(&repo.to_string()))
}

pub fn clone_dir(root: &Path, repo: &RepoId) -> PathBuf {
    root.join(&repo.owner).join(&repo.name)
}

/// The first characters of a session id, when they make a directory name; `None` for a
/// session id too odd for one.
pub fn short_session_id(session_id: &str) -> Option<String> {
    let short: String = session_id.chars().take(WORKTREE_ID_CHARS).collect();
    (!short.is_empty() && short.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .then_some(short)
}

/// `None` for a session id too odd to be a directory name.
pub fn worktree_dir(clone: &Path, session_id: &str) -> Option<PathBuf> {
    Some(
        clone
            .join(WORKTREES_DIR)
            .join(short_session_id(session_id)?),
    )
}

/// Where the files of the preparation phase live, before the worktree exists and while the
/// repository may still be unknown: `<root>/.agents/<first 8 characters of sessionId>`. The
/// orchestrator works in the vault, so nothing of Ward is ever written inside it.
pub fn prepare_dir(root: &Path, session_id: &str) -> Option<PathBuf> {
    Some(root.join(AGENTS_DIR).join(short_session_id(session_id)?))
}

pub fn agents_dir(worktree: &Path) -> PathBuf {
    worktree.join(AGENTS_DIR)
}

/// Remote Control name: `<taskNumber> <title>`, the task id when there is no number.
pub fn session_name(task_ref: &TaskRef, title: &str) -> String {
    let number = task_ref.task_number.unwrap_or(task_ref.task_id);
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        number.to_string()
    } else {
        format!("{number} {title}")
    }
}

/// Lines Ward keeps in the clone's `.git/info/exclude`.
pub const EXCLUDE_ENTRIES: [&str; 2] = ["/.agents/", "/.worktrees/"];

/// The exclude file with Ward's entries added, or `None` when they are all there.
pub fn exclude_with_entries(existing: &str) -> Option<String> {
    let missing: Vec<&str> = EXCLUDE_ENTRIES
        .into_iter()
        .filter(|entry| !existing.lines().any(|line| line.trim() == *entry))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut text = existing.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for entry in missing {
        text.push_str(entry);
        text.push('\n');
    }
    Some(text)
}

/// Which script runs the agent in the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerKind {
    /// PowerShell 7: passes a multi-line prompt as one argument (Windows PowerShell 5.1 does
    /// not).
    Pwsh,
    /// POSIX sh, also Git Bash on Windows without pwsh.
    Sh,
}

impl RunnerKind {
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Pwsh => "run.ps1",
            Self::Sh => "run.sh",
        }
    }
}

/// `'...'` for PowerShell.
pub fn pwsh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// `'...'` for sh.
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The runner script of one agent: `files` holds the prompt, the runner's pid and the exit
/// code, `cwd` is the directory the agent starts in. For an executor both are the worktree and
/// its `.agents`; for the orchestrator of the preparation phase the files are under the
/// repository root and the directory is the vault, so nothing of Ward is written inside it.
pub fn runner_script(kind: RunnerKind, files: &Path, cwd: &Path, command: &AgentCommand) -> String {
    let file = |name: &str| forward_slashes(&files.join(name));
    let worktree = forward_slashes(cwd);
    match kind {
        RunnerKind::Pwsh => {
            let q = |value: &str| pwsh_quote(value);
            let mut lines = vec![
                "$ErrorActionPreference = 'Continue'".to_owned(),
                format!(
                    "Set-Content -LiteralPath {} -Value $PID -NoNewline",
                    q(&file(RUNNER_PID_FILE))
                ),
            ];
            for prefix in &command.remove_env_prefixes {
                lines.push(format!(
                    "Get-ChildItem Env: | Where-Object {{ $_.Name.StartsWith({}, [StringComparison]::OrdinalIgnoreCase) }} | ForEach-Object {{ Remove-Item -LiteralPath ('Env:' + $_.Name) }}",
                    q(prefix)
                ));
            }
            for (name, value) in &command.set_env {
                lines.push(format!(
                    "Set-Item -LiteralPath {} -Value {}",
                    q(&format!("Env:{name}")),
                    q(value)
                ));
            }
            lines.push(format!("Set-Location -LiteralPath {}", q(&worktree)));
            lines.push(format!(
                "$prompt = Get-Content -Raw -Encoding UTF8 -LiteralPath {}",
                q(&file(PROMPT_FILE))
            ));
            let args: Vec<String> = command.args.iter().map(|arg| q(arg)).collect();
            lines.push("$LASTEXITCODE = $null".to_owned());
            lines.push(format!(
                "try {{ & {} {} -- $prompt }} catch {{ Write-Host $_; $LASTEXITCODE = 127 }}",
                q(&command.program),
                args.join(" ")
            ));
            lines.push("$code = $LASTEXITCODE".to_owned());
            lines.push("if ($null -eq $code) { $code = 1 }".to_owned());
            lines.push(format!(
                "Set-Content -LiteralPath {} -Value $code -NoNewline",
                q(&file("exit-code.tmp"))
            ));
            lines.push(format!(
                "Move-Item -LiteralPath {} -Destination {} -Force",
                q(&file("exit-code.tmp")),
                q(&file(EXIT_CODE_FILE))
            ));
            lines.push("exit $code".to_owned());
            lines.join("\n") + "\n"
        }
        RunnerKind::Sh => {
            let q = |value: &str| sh_quote(value);
            let mut lines = vec![
                "#!/bin/sh".to_owned(),
                format!("echo $$ > {}", q(&file(RUNNER_PID_FILE))),
            ];
            for prefix in &command.remove_env_prefixes {
                lines.push(format!(
                    "for k in $(env | sed -n 's/^\\({prefix}[A-Za-z0-9_]*\\)=.*/\\1/p'); do unset \"$k\"; done"
                ));
            }
            for (name, value) in &command.set_env {
                lines.push(format!("export {name}={}", q(value)));
            }
            lines.push(format!("cd {} || exit 1", q(&worktree)));
            lines.push(format!("prompt=$(cat {})", q(&file(PROMPT_FILE))));
            let args: Vec<String> = command.args.iter().map(|arg| q(arg)).collect();
            lines.push(format!(
                "{} {} -- \"$prompt\"",
                q(&command.program),
                args.join(" ")
            ));
            lines.push("code=$?".to_owned());
            lines.push(format!(
                "printf '%s' \"$code\" > {tmp} && mv -f {tmp} {done}",
                tmp = q(&file("exit-code.tmp")),
                done = q(&file(EXIT_CODE_FILE))
            ));
            lines.push("exit $code".to_owned());
            lines.join("\n") + "\n"
        }
    }
}

/// The command typed into the tab's shell (PowerShell on Windows, the default shell
/// elsewhere). `bash` is Git Bash's `bash.exe` for [`RunnerKind::Sh`] on Windows.
pub fn tab_command(kind: RunnerKind, runner: &Path, bash: Option<&Path>) -> String {
    let runner = forward_slashes(runner);
    match (kind, bash) {
        (RunnerKind::Pwsh, _) => {
            format!("pwsh -NoProfile -ExecutionPolicy Bypass -File \"{runner}\"")
        }
        (RunnerKind::Sh, Some(bash)) => {
            format!("& \"{}\" \"{runner}\"", forward_slashes(bash))
        }
        (RunnerKind::Sh, None) => format!("sh \"{runner}\""),
    }
}

/// The exit code in the contents of `exit-code`.
pub fn parse_exit_code(text: &str) -> Option<i32> {
    text.trim().parse().ok()
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
