//! Clone, fetch and worktree of a session, with the `git` CLI (blocking: run it off the main
//! thread). Git never prompts: a repository that needs credentials Ward does not have fails
//! instead of hanging.

use std::path::{Path, PathBuf};

use command::blocking::Command;

use super::launch::{self, RepoId};

/// Longest git error kept for the status message.
const MAX_ERROR_CHARS: usize = 400;

fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(command::Stdio::null());
    command
}

fn short_error(text: &str) -> String {
    let text = text.trim();
    let mut short: String = text.chars().take(MAX_ERROR_CHARS).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    short
}

/// Runs git in `dir`; the error is git's stderr.
fn run(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = git_command();
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let output = command
        .args(args)
        .output()
        .map_err(|err| format!("git is not available: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            short_error(&stderr)
        ))
    }
}

fn has_ref(dir: &Path, reference: &str) -> bool {
    run(Some(dir), &["show-ref", "--verify", "--quiet", reference]).is_ok()
}

fn is_valid_branch(name: &str) -> bool {
    !name.is_empty() && run(None, &["check-ref-format", "--branch", name]).is_ok()
}

/// Adds Ward's lines to the clone's `.git/info/exclude`.
pub fn ensure_excluded(clone: &Path) -> std::io::Result<()> {
    let info = clone.join(".git").join("info");
    std::fs::create_dir_all(&info)?;
    let path = info.join("exclude");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    match launch::exclude_with_entries(&existing) {
        Some(text) => std::fs::write(&path, text),
        None => Ok(()),
    }
}

/// What the worktree of a session needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSpec<'a> {
    pub repo_url: &'a str,
    pub repo: &'a RepoId,
    pub base_branch: &'a str,
    pub branch: &'a str,
    pub session_id: &'a str,
}

/// Clones or fetches `<root>/<owner>/<repo>` and creates the session's worktree on `branch`:
/// from `origin/<branch>` when it exists, else from `origin/<base_branch>`. An existing
/// worktree of the same session is reused.
pub fn prepare_worktree(root: &Path, spec: &WorktreeSpec<'_>) -> Result<PathBuf, String> {
    if !is_valid_branch(spec.branch) {
        return Err(format!("invalid branch name: {}", spec.branch));
    }
    let base_branch = if spec.base_branch.trim().is_empty() {
        "main"
    } else {
        spec.base_branch.trim()
    };
    if !is_valid_branch(base_branch) {
        return Err(format!("invalid base branch name: {base_branch}"));
    }
    let clone = launch::clone_dir(root, spec.repo);
    if clone.join(".git").exists() {
        run(Some(&clone), &["fetch", "origin", "--prune"])?;
    } else {
        if let Some(parent) = clone.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        }
        let target = clone.to_string_lossy().into_owned();
        run(None, &["clone", "--", spec.repo_url, &target])?;
    }
    ensure_excluded(&clone).map_err(|err| format!("cannot update .git/info/exclude: {err}"))?;

    let worktree = launch::worktree_dir(&clone, spec.session_id)
        .ok_or_else(|| "invalid session id".to_owned())?;
    if worktree.join(".git").exists() {
        return Ok(worktree);
    }
    // The clone itself may have the branch checked out (a fresh clone of a repository whose
    // default branch is the session's): git refuses a second worktree on it.
    let clone_head =
        run(Some(&clone), &["symbolic-ref", "-q", "--short", "HEAD"]).unwrap_or_default();
    if clone_head.trim() == spec.branch {
        run(Some(&clone), &["checkout", "-q", "--detach"])?;
    }
    let worktree_arg = worktree.to_string_lossy().into_owned();
    let remote_branch = format!("refs/remotes/origin/{}", spec.branch);
    if has_ref(&clone, &format!("refs/heads/{}", spec.branch)) {
        run(
            Some(&clone),
            &["worktree", "add", &worktree_arg, spec.branch],
        )?;
    } else if has_ref(&clone, &remote_branch) {
        let start = format!("origin/{}", spec.branch);
        run(
            Some(&clone),
            &[
                "worktree",
                "add",
                "--track",
                "-b",
                spec.branch,
                &worktree_arg,
                &start,
            ],
        )?;
    } else {
        let start = format!("origin/{base_branch}");
        if !has_ref(&clone, &format!("refs/remotes/{start}")) {
            return Err(format!("origin has no branch {base_branch}"));
        }
        run(
            Some(&clone),
            &[
                "worktree",
                "add",
                "--no-track",
                "-b",
                spec.branch,
                &worktree_arg,
                &start,
            ],
        )?;
    }
    Ok(worktree)
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
