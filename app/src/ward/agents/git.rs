//! Clone, fetch and worktree of a session, with the `git` CLI (blocking: run it off the main
//! thread), and the search for the clones the owner already has on the machine. Git never
//! prompts: a repository that needs credentials Ward does not have fails instead of hanging.

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
    ensure_excluded_entries(clone, &launch::EXCLUDE_ENTRIES)
}

/// Adds `entries` to the clone's `.git/info/exclude`.
fn ensure_excluded_entries(clone: &Path, entries: &[&str]) -> std::io::Result<()> {
    let info = clone.join(".git").join("info");
    std::fs::create_dir_all(&info)?;
    let path = info.join("exclude");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    match launch::exclude_with(&existing, entries) {
        Some(text) => std::fs::write(&path, text),
        None => Ok(()),
    }
}

/// How the `vault` part of the machine profile wants the vault refreshed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VaultUpdate {
    /// Leave the vault alone.
    Off,
    /// `git fetch origin --prune` only.
    Fetch,
    /// Fetch and fast-forward; the default.
    #[default]
    Pull,
}

pub fn parse_vault_update(value: &str) -> VaultUpdate {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" | "off" | "no" | "false" => VaultUpdate::Off,
        "fetch" => VaultUpdate::Fetch,
        _ => VaultUpdate::Pull,
    }
}

/// The vault of the preparation phase: cloned when its directory is not there yet and
/// refreshed as the profile asks. Ward never writes inside the vault and never merges or
/// rebases: a fast-forward that does not apply (the two devices went apart) leaves the vault as
/// it is and says so, because the vault is still usable. The message goes into the profile
/// report; an error means no preparation can run on this machine.
pub fn prepare_vault(
    path: &Path,
    repo: &str,
    branch: &str,
    update: &str,
) -> Result<String, String> {
    if path.as_os_str().is_empty() {
        return Err("путь к волту не задан".to_owned());
    }
    if !path.join(".git").exists() {
        // A vault of plain notes is usable as it is; only updating it is off the table.
        if path.is_dir() {
            return Ok(format!(
                "волт на месте, но {} не репозиторий git: обновление пропущено",
                path.display()
            ));
        }
        if repo.is_empty() {
            return Err(format!(
                "волта нет в {} и в профиле не задан repo для клонирования",
                path.display()
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        }
        let target = path.to_string_lossy().into_owned();
        run(None, &["clone", "--", repo, &target])?;
        return Ok(format!("волт склонирован в {}", path.display()));
    }
    match parse_vault_update(update) {
        VaultUpdate::Off => Ok("волт на месте, обновление выключено в профиле".to_owned()),
        VaultUpdate::Fetch => {
            run(Some(path), &["fetch", "origin", "--prune"])?;
            Ok("волт: fetch origin".to_owned())
        }
        VaultUpdate::Pull => {
            run(Some(path), &["fetch", "origin", "--prune"])?;
            let current =
                run(Some(path), &["symbolic-ref", "-q", "--short", "HEAD"]).unwrap_or_default();
            let current = current.trim();
            if current.is_empty() {
                return Ok("волт не на ветке: только fetch".to_owned());
            }
            if !branch.is_empty() && current != branch {
                return Ok(format!(
                    "волт на ветке {current}, в профиле {branch}: только fetch"
                ));
            }
            let upstream = format!("origin/{current}");
            match run(Some(path), &["merge", "--ff-only", &upstream]) {
                Ok(_) => Ok(format!("волт обновлён до {upstream}")),
                Err(err) => Ok(format!(
                    "волт разошёлся с {upstream}, оставлен как есть: {err}"
                )),
            }
        }
    }
}

/// Folders never searched for local clones: build output and dependencies.
const SKIPPED_DIRS: [&str; 2] = ["node_modules", "target"];

/// How deep under a search folder a local clone is looked for: `C:/work/<group>/<repo>` is 2.
pub const SEARCH_DEPTH: usize = 3;

/// A remote URL reduced to what names the repository: the lowercase host and path, without the
/// user, the port, the scheme and `.git`. `https://github.com/Owner/Repo.git`,
/// `git@github.com:owner/repo` and `ssh://git@github.com:22/owner/repo` give the same key; a
/// local path (also `file://`) keeps its whole path with forward slashes.
pub fn remote_key(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            ("", rest)
        } else {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority.rsplit('@').next().unwrap_or(authority);
            (host.split(':').next().unwrap_or(host), path)
        }
    } else {
        match url.split_once(':') {
            // `git@host:owner/repo`, but not a Windows drive (`C:\...`).
            Some((left, path)) if left.len() > 1 && !left.contains(['/', '\\']) => {
                (left.rsplit('@').next().unwrap_or(left), path)
            }
            _ => ("", url),
        }
    };
    let path = path.replace('\\', "/");
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    if path.is_empty() {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    Some(format!("{host}/{}", path.to_lowercase()))
}

/// Whether two remote URLs name the same repository.
pub fn same_remote(a: &str, b: &str) -> bool {
    matches!((remote_key(a), remote_key(b)), (Some(a), Some(b)) if a == b)
}

/// The `origin` URL in `<repo>/.git/config`, read from the file: no git process per folder.
pub fn origin_url(repo: &Path) -> Option<String> {
    let config = std::fs::read_to_string(repo.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let section: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            in_origin = section == "[remote\"origin\"]";
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            let value = value.trim().trim_matches('"').trim();
            return (!value.is_empty()).then(|| value.to_owned());
        }
    }
    None
}

/// A working copy with its own `.git` directory (a worktree or a submodule has a `.git` file).
fn is_clone(dir: &Path) -> bool {
    dir.join(".git").is_dir()
}

fn collect_clones(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if is_clone(dir) {
        out.push(dir.to_path_buf());
        // Nothing of another repository is looked for inside a working copy.
        return;
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref())
        })
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for child in children {
        collect_clones(&child, depth - 1, out);
    }
}

/// The local clones in `search`: each entry is a clone itself or a folder searched
/// [`SEARCH_DEPTH`] levels down, without hidden folders, `node_modules` and `target`. In the
/// order of `search`, without repeats.
pub fn find_clones(search: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in search {
        collect_clones(entry, SEARCH_DEPTH, &mut out);
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|path| seen.insert(launch::forward_slashes(path).to_lowercase()));
    out
}

/// The first local clone in `search` whose `origin` is `repo_url`.
pub fn find_local_clone(search: &[PathBuf], repo_url: &str) -> Option<PathBuf> {
    find_clones(search)
        .into_iter()
        .find(|clone| origin_url(clone).is_some_and(|url| same_remote(&url, repo_url)))
}

/// `owner/repo` of the local clones in `search`, for the passport.
pub fn local_repos(search: &[PathBuf]) -> Vec<String> {
    let mut repos: Vec<String> = find_clones(search)
        .iter()
        .filter_map(|clone| origin_url(clone))
        .filter_map(|url| launch::parse_repo(&url))
        .map(|repo| repo.to_string())
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

/// The default branch of `origin`: `refs/remotes/origin/HEAD`, else what the remote says.
fn default_branch(clone: &Path) -> Option<String> {
    if let Ok(head) = run(
        Some(clone),
        &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
    ) && let Some(branch) = head.trim().strip_prefix("origin/")
        && !branch.is_empty()
    {
        return Some(branch.to_owned());
    }
    let listing = run(Some(clone), &["ls-remote", "--symref", "origin", "HEAD"]).ok()?;
    parse_symref_head(&listing)
}

/// The branch of `ref: refs/heads/<branch>\tHEAD` in `git ls-remote --symref origin HEAD`.
pub fn parse_symref_head(listing: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let (reference, name) = line
            .strip_prefix("ref:")?
            .trim()
            .split_once(char::is_whitespace)?;
        if name.trim() != "HEAD" {
            return None;
        }
        reference
            .strip_prefix("refs/heads/")
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned)
    })
}

/// The base the new branch starts from: `requested` when origin has it, else the default
/// branch of origin, with a note saying so.
fn resolve_base(clone: &Path, requested: &str) -> Result<(String, Option<String>), String> {
    if !requested.is_empty() && has_ref(clone, &format!("refs/remotes/origin/{requested}")) {
        return Ok((requested.to_owned(), None));
    }
    let default = default_branch(clone)
        .filter(|branch| has_ref(clone, &format!("refs/remotes/origin/{branch}")));
    match default {
        Some(branch) => {
            let note = if requested.is_empty() {
                format!("база не задана, взята ветка по умолчанию origin/{branch}")
            } else {
                format!("ветки {requested} на origin нет, взята ветка по умолчанию origin/{branch}")
            };
            Ok((branch, Some(note)))
        }
        None if requested.is_empty() => Err("origin has no default branch".to_owned()),
        None => Err(format!("origin has no branch {requested}")),
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
    /// Where the owner's own clones are looked for before cloning (see [`find_clones`]).
    pub search: &'a [PathBuf],
}

/// The worktree of a session and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedWorktree {
    pub path: PathBuf,
    /// The repository the worktree belongs to: the clone under the root or the owner's own.
    pub clone: PathBuf,
    /// The clone is the owner's, found outside the root.
    pub local_clone: bool,
    /// `<base>` of `origin/<base>` the new branch started from; `None` for a branch that
    /// already existed or a reused worktree.
    pub base: Option<String>,
    /// Why the base is not the one the assignment asked for.
    pub base_note: Option<String>,
}

impl PreparedWorktree {
    /// For the log and the status of the session: which base and which clone.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        match (&self.base_note, &self.base) {
            (Some(note), _) => parts.push(note.clone()),
            (None, Some(base)) => parts.push(format!("база origin/{base}")),
            (None, None) => {}
        }
        if self.local_clone {
            parts.push(format!(
                "локальный клон {}",
                launch::forward_slashes(&self.clone)
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

/// Creates the session's worktree on `branch`: from the local branch or `origin/<branch>` when
/// one exists, else from `origin/<base_branch>`, else from the default branch of origin. The
/// repository is, in this order, the clone `<root>/<owner>/<repo>`, a clone of the same remote
/// the owner already has (see [`WorktreeSpec::search`]) or a new clone under the root. The
/// owner's clone is only fetched: its working copy and its current branch stay as they are,
/// and the worktree goes under the root. An existing worktree of the same session is reused.
pub fn prepare_worktree(root: &Path, spec: &WorktreeSpec<'_>) -> Result<PreparedWorktree, String> {
    if !is_valid_branch(spec.branch) {
        return Err(format!("invalid branch name: {}", spec.branch));
    }
    let requested_base = spec.base_branch.trim();
    if !requested_base.is_empty() && !is_valid_branch(requested_base) {
        return Err(format!("invalid base branch name: {requested_base}"));
    }
    let own = launch::clone_dir(root, spec.repo);
    let (clone, local_clone) = if own.join(".git").exists() {
        run(Some(&own), &["fetch", "origin", "--prune"])?;
        (own, false)
    } else if let Some(local) = find_local_clone(spec.search, spec.repo_url) {
        log::info!(
            "Ward agents: {} is already cloned at {}, no new clone",
            spec.repo,
            local.display()
        );
        // No `--prune`: the owner's remote-tracking branches stay as they are.
        run(Some(&local), &["fetch", "origin"])?;
        (local, true)
    } else {
        if let Some(parent) = own.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        }
        let target = own.to_string_lossy().into_owned();
        run(None, &["clone", "--", spec.repo_url, &target])?;
        (own, false)
    };
    let excluded = if local_clone {
        ensure_excluded_entries(&clone, &launch::LOCAL_CLONE_EXCLUDE_ENTRIES)
    } else {
        ensure_excluded(&clone)
    };
    excluded.map_err(|err| format!("cannot update .git/info/exclude: {err}"))?;

    let worktree = if local_clone {
        launch::external_worktree_dir(root, spec.repo, spec.session_id)
    } else {
        launch::worktree_dir(&clone, spec.session_id)
    }
    .ok_or_else(|| "invalid session id".to_owned())?;
    let prepared = |base: Option<String>, base_note: Option<String>| PreparedWorktree {
        path: worktree.clone(),
        clone: clone.clone(),
        local_clone,
        base,
        base_note,
    };
    if worktree.join(".git").exists() {
        return Ok(prepared(None, None));
    }
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    if !local_clone {
        // The clone itself may have the branch checked out (a fresh clone of a repository
        // whose default branch is the session's): git refuses a second worktree on it. The
        // owner's clone is never switched: git then reports the branch as checked out.
        let clone_head =
            run(Some(&clone), &["symbolic-ref", "-q", "--short", "HEAD"]).unwrap_or_default();
        if clone_head.trim() == spec.branch {
            run(Some(&clone), &["checkout", "-q", "--detach"])?;
        }
    }
    let worktree_arg = worktree.to_string_lossy().into_owned();
    if has_ref(&clone, &format!("refs/heads/{}", spec.branch)) {
        run(
            Some(&clone),
            &["worktree", "add", &worktree_arg, spec.branch],
        )?;
        return Ok(prepared(None, None));
    }
    if has_ref(&clone, &format!("refs/remotes/origin/{}", spec.branch)) {
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
        return Ok(prepared(None, None));
    }
    let (base, base_note) = resolve_base(&clone, requested_base)?;
    let start = format!("origin/{base}");
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
    Ok(prepared(Some(base), base_note))
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
