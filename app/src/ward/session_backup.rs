//! Copies of the saved session Ward makes right before an update installer runs.
//!
//! The installer replaces the program files only: it never touches the state directory
//! (`%LOCALAPPDATA%\warp\WarpOss\data`), which only the uninstaller removes. The copy is there
//! for the case nobody planned for, a new version that cannot read the session or a migration of
//! the session database that fails: the windows and tabs of the session before the update can
//! then be put back by hand (see WARD.md, "Обновление при запуске").
//!
//! A copy holds the session database (`warp.sqlite` and its write-ahead log; the shared-memory
//! index is rebuilt by SQLite) and Ward's own tab handoff (`ward_session_handoff.json`), when they
//! exist. Each copy is a directory `<state dir>/ward_session_backups/<time>_<version>`; the
//! newest [`KEEP`] are kept.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::session_handoff;

/// Directory in the state directory that holds the copies.
pub const DIR_NAME: &str = "ward_session_backups";

/// How many copies are kept; older ones are removed when a new one is made.
pub const KEEP: usize = 5;

/// The session database of the app, as `persistence::sqlite` places it. On Windows there is no
/// secure state directory, so it is `<state dir>/warp.sqlite`.
pub fn database_path() -> PathBuf {
    warp_core::paths::secure_state_dir()
        .unwrap_or_else(warp_core::paths::state_dir)
        .join("warp.sqlite")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The write-ahead log of the database: SQLite keeps the latest writes there until a
/// checkpoint, so the database file alone can miss them.
fn wal_path(database: &Path) -> PathBuf {
    with_suffix(database, "-wal")
}

fn shm_path(database: &Path) -> PathBuf {
    with_suffix(database, "-shm")
}

/// The files that make up the saved session, whether they exist or not.
pub fn session_files(state_dir: &Path, database: &Path) -> Vec<PathBuf> {
    vec![
        database.to_owned(),
        wal_path(database),
        state_dir.join(session_handoff::FILE_NAME),
    ]
}

/// Keeps a name usable as part of a directory name.
fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Copies the session files that exist into a new directory `<state dir>/DIR_NAME/<stamp>_<label>`
/// and removes the copies beyond [`KEEP`]. `stamp` orders the copies (`2026-11-13_10-42-07`).
/// Returns the new directory, or `None` when there was nothing to copy.
pub fn backup(
    state_dir: &Path,
    database: &Path,
    stamp: &str,
    label: &str,
) -> io::Result<Option<PathBuf>> {
    let sources: Vec<PathBuf> = session_files(state_dir, database)
        .into_iter()
        .filter(|path| path.is_file())
        .collect();
    if sources.is_empty() {
        return Ok(None);
    }
    let root = state_dir.join(DIR_NAME);
    fs::create_dir_all(&root)?;
    let base = format!("{}_{}", sanitize(stamp), sanitize(label));
    let mut dir = root.join(&base);
    let mut n = 1;
    while dir.exists() {
        dir = root.join(format!("{base}-{n}"));
        n += 1;
    }
    fs::create_dir(&dir)?;
    for source in &sources {
        let Some(name) = source.file_name() else {
            continue;
        };
        if let Err(err) = fs::copy(source, dir.join(name)) {
            // Half a copy is no copy: it must not push a complete one out of the kept ones.
            let _ = fs::remove_dir_all(&dir);
            return Err(err);
        }
    }
    // The new copy is complete; an old one that cannot be removed only takes space.
    if let Err(err) = prune(&root, KEEP) {
        log::warn!("Ward: could not remove old session copies: {err}");
    }
    Ok(Some(dir))
}

/// The copies, newest first.
pub fn list(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    dirs.sort();
    dirs.reverse();
    Ok(dirs)
}

/// Removes all but the `keep` newest copies.
pub fn prune(root: &Path, keep: usize) -> io::Result<()> {
    for old in list(root)?.into_iter().skip(keep) {
        fs::remove_dir_all(old)?;
    }
    Ok(())
}

/// Puts a copy back: the database with its log and the handoff replace the current ones. Ward
/// must not be running. The current shared-memory index and log are removed first, so SQLite
/// does not apply a newer log to the older database.
pub fn restore(backup: &Path, state_dir: &Path, database: &Path) -> io::Result<()> {
    let Some(database_name) = database.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the session database has no file name",
        ));
    };
    if !backup.join(database_name).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the copy has no session database",
        ));
    }
    for stale in [wal_path(database), shm_path(database)] {
        match fs::remove_file(&stale) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    let database_dir = database.parent().unwrap_or(state_dir);
    for source in session_files(state_dir, database) {
        let Some(name) = source.file_name() else {
            continue;
        };
        let copy = backup.join(name);
        if !copy.is_file() {
            continue;
        }
        let target = if name == session_handoff::FILE_NAME {
            state_dir.join(name)
        } else {
            database_dir.join(name)
        };
        fs::copy(&copy, target)?;
    }
    Ok(())
}

/// Copies the session of this machine before an update installer starts. `label` names the
/// version that made the copy. A failure is logged and returned.
pub fn backup_now(label: &str) -> io::Result<Option<PathBuf>> {
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let result = backup(
        &warp_core::paths::state_dir(),
        &database_path(),
        &stamp,
        label,
    );
    match &result {
        Ok(Some(dir)) => log::info!(
            "Ward: copied the session to {} before the update",
            dir.display()
        ),
        Ok(None) => log::info!("Ward: no saved session to copy before the update"),
        Err(err) => log::warn!("Ward: could not copy the session before the update: {err}"),
    }
    result
}

#[cfg(test)]
#[path = "session_backup_tests.rs"]
mod tests;
