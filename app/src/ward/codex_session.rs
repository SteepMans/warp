//! Which Codex session a running Codex process holds.
//!
//! A pane remembers the session of the Codex process running in it, and Ward resumes exactly
//! that session when the pane is restored. Codex has no registry of its processes like Claude
//! Code's `~/.claude/sessions/<pid>.json`, so the session is worked out from what the process
//! itself shows, most reliable first:
//!
//! 1. The files the process holds open: the rollout it appends to
//!    (`rollout-<time>-<id>.jsonl`) and its writer lock `thread-writer-locks/<id>.lock`. This is
//!    exact and also follows `/new` and `/resume` inside Codex.
//! 2. The id it was started with: `codex resume <id>`. Every restored pane starts this way.
//! 3. What the process was given before (a new session keeps its first rollout).
//! 4. The rollouts of its directory written since it started, minus the sessions every other
//!    running Codex process holds. A new session (`codex`, `codex fork`) only takes a rollout
//!    created after the process started; `codex resume` without an id (the picker, `--last`)
//!    takes the most recently written one.
//!
//! Before this, step 4 came first and did not exclude other processes: every Codex pane of one
//! project directory took the session written last, and after a restart all of them reopened
//! the same conversation ("This conversation is open in another app").

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

/// Slack for clock and file system timestamp granularity when comparing rollout times with the
/// process start time.
pub const START_TIME_SLACK: Duration = Duration::from_secs(5);

/// A running process: its pid and start time (seconds since the epoch), so that a reused pid is
/// a different process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub pid: u32,
    pub started: u64,
}

/// How a Codex process was started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// `codex` or `codex fork`: the process makes a session of its own.
    New,
    /// `codex resume <id>` (the root session of `<id>`), or `codex resume` with the picker or
    /// `--last` when `None`.
    Resume(Option<String>),
}

/// A rollout written since the process started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rollout {
    /// The root session the rollout belongs to (a child thread stands for its root).
    pub id: String,
    /// The session's directory.
    pub cwd: Option<String>,
    /// When the session was created; `None` for a child thread, whose creation says nothing
    /// about its root.
    pub created: Option<SystemTime>,
    pub modified: SystemTime,
}

/// The sessions Codex processes were given, so that two processes in one directory do not take
/// the same one.
static CLAIMS: LazyLock<Mutex<HashMap<ProcessKey, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The session of the Codex process `key`; see the module documentation.
///
/// `open` are the root sessions the process holds open, `others` every other running Codex
/// process with the id it was resumed with. Choosing and remembering happen under one lock, so
/// panes polled at the same time do not take the same session.
pub fn assign(
    key: ProcessKey,
    launch: &Launch,
    open: &[String],
    rollouts: &[Rollout],
    cwd: Option<&Path>,
    others: &[(ProcessKey, Option<String>)],
) -> Option<String> {
    let mut claims = CLAIMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let running: HashSet<ProcessKey> = others
        .iter()
        .map(|(other, _)| *other)
        .chain([key])
        .collect();
    claims.retain(|process, _| running.contains(process));
    let taken = taken_by_others(&claims, key, others);
    let id = choose(
        launch,
        open,
        claims.get(&key).map(String::as_str),
        rollouts,
        cwd,
        started_at(key),
        &taken,
    )?;
    claims.insert(key, id.clone());
    Some(id)
}

fn started_at(key: ProcessKey) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(key.started)
}

/// The sessions other running Codex processes hold: what they were given, else the id they were
/// resumed with.
pub fn taken_by_others(
    claims: &HashMap<ProcessKey, String>,
    me: ProcessKey,
    others: &[(ProcessKey, Option<String>)],
) -> HashSet<String> {
    let mut taken: HashSet<String> = claims
        .iter()
        .filter(|(process, _)| **process != me)
        .map(|(_, id)| id.clone())
        .collect();
    for (process, resumed) in others {
        if *process == me || claims.contains_key(process) {
            continue;
        }
        if let Some(id) = resumed {
            taken.insert(id.clone());
        }
    }
    taken
}

/// Picks the session of a Codex process. Pure; see the module documentation for the order.
pub fn choose(
    launch: &Launch,
    open: &[String],
    claimed: Option<&str>,
    rollouts: &[Rollout],
    cwd: Option<&Path>,
    started: SystemTime,
    taken: &HashSet<String>,
) -> Option<String> {
    // The process holds the session's files open: exact. Several open roots (rare) go by the
    // most recently written rollout.
    if !open.is_empty() {
        let newest = open.iter().max_by_key(|id| {
            rollouts
                .iter()
                .filter(|rollout| rollout.id == **id)
                .map(|rollout| rollout.modified)
                .max()
        });
        return newest.cloned();
    }
    if let Launch::Resume(Some(id)) = launch {
        return Some(id.clone());
    }
    if let (Launch::New, Some(claimed)) = (launch, claimed) {
        return Some(claimed.to_owned());
    }

    let threshold = started.checked_sub(START_TIME_SLACK).unwrap_or(started);
    let candidates = rollouts.iter().filter(|rollout| {
        !taken.contains(&rollout.id)
            && rollout.modified >= threshold
            && match (cwd, rollout.cwd.as_deref()) {
                (Some(cwd), Some(session_cwd)) => same_dir(cwd, Path::new(session_cwd)),
                _ => true,
            }
    });
    match launch {
        // A new session is the first one the process created; an older session written to
        // since then belongs to another process, running or gone.
        Launch::New => candidates
            .filter_map(|rollout| Some((rollout.created?, rollout)))
            .filter(|(created, _)| *created >= threshold)
            .min_by_key(|(created, _)| *created)
            .map(|(_, rollout)| rollout.id.clone()),
        Launch::Resume(_) => candidates
            .max_by_key(|rollout| rollout.modified)
            .map(|rollout| rollout.id.clone()),
    }
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .replace('\\', "/")
            .to_lowercase()
    };
    normalize(a) == normalize(b)
}

/// The thread id a file Codex holds open names: its rollout `rollout-<time>-<id>.jsonl` or its
/// writer lock `thread-writer-locks/<id>.lock`.
pub fn thread_id_from_path(path: &str) -> Option<String> {
    let mut parts = path.rsplit(['/', '\\']);
    let name = parts.next()?;
    let dir = parts.next().unwrap_or_default();
    let id = if let Some(stem) = name.strip_suffix(".jsonl") {
        if !stem.starts_with("rollout-") || stem.len() < 36 {
            return None;
        }
        &stem[stem.len() - 36..]
    } else if let Some(stem) = name.strip_suffix(".lock") {
        if !dir.eq_ignore_ascii_case("thread-writer-locks") {
            return None;
        }
        stem
    } else {
        return None;
    };
    crate::ward::codex_threads::looks_like_session_id(id).then(|| id.to_owned())
}

/// The thread ids of the Codex files the process `pid` holds open. `None` when they cannot be
/// read on this platform or for this process.
pub fn open_thread_ids(pid: u32) -> Option<Vec<String>> {
    let paths = open_file_paths(pid)?;
    let mut ids: Vec<String> = Vec::new();
    for id in paths.iter().filter_map(|path| thread_id_from_path(path)) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Some(ids)
}

#[cfg(target_os = "linux")]
fn open_file_paths(pid: u32) -> Option<Vec<String>> {
    let entries = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
    Some(
        entries
            .flatten()
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    )
}

#[cfg(windows)]
fn open_file_paths(pid: u32) -> Option<Vec<String>> {
    windows_handles::open_file_paths(pid)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn open_file_paths(_pid: u32) -> Option<Vec<String>> {
    None
}

#[cfg(windows)]
mod windows_handles {
    //! The disk files a process holds open, from the snapshot of its handle table.

    use std::collections::HashSet;
    use std::sync::{LazyLock, Mutex, mpsc};
    use std::time::Duration;

    use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessHandleInformation};
    use windows::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, STATUS_INFO_LENGTH_MISMATCH,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, FILE_TYPE_DISK, GetFileType, GetFinalPathNameByHandleW,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_INFORMATION,
    };

    /// `PROCESS_HANDLE_TABLE_ENTRY_INFO`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    struct HandleEntry {
        handle_value: HANDLE,
        handle_count: usize,
        pointer_count: usize,
        granted_access: u32,
        object_type_index: u32,
        handle_attributes: u32,
        reserved: u32,
    }

    /// `FILE_READ_DATA | FILE_WRITE_DATA | FILE_APPEND_DATA`: handles without data access (to
    /// attributes or for synchronization only) are skipped.
    const DATA_ACCESS: u32 = 0x1 | 0x2 | 0x4;

    /// A path query blocks while a synchronous operation is pending on the file; the query then
    /// gives up on the process.
    const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

    /// Processes whose query did not finish in time. They are not asked again, so that a stuck
    /// query does not leave a thread behind on every poll.
    static STALLED: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

    fn stalled() -> std::sync::MutexGuard<'static, HashSet<u32>> {
        STALLED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn open_file_paths(pid: u32) -> Option<Vec<String>> {
        if stalled().contains(&pid) {
            return None;
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("ward-codex-handles".to_owned())
            .spawn(move || {
                let _ = sender.send(query(pid));
            })
            .ok()?;
        match receiver.recv_timeout(QUERY_TIMEOUT) {
            Ok(paths) => paths,
            Err(_) => {
                log::warn!("Ward: reading the open files of Codex process {pid} timed out");
                stalled().insert(pid);
                None
            }
        }
    }

    fn query(pid: u32) -> Option<Vec<String>> {
        // SAFETY: plain Win32 calls; every handle opened or duplicated here is closed.
        unsafe {
            let process =
                OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_DUP_HANDLE, false, pid).ok()?;
            let entries = handle_entries(process);
            let mut paths = Vec::new();
            for entry in entries.unwrap_or_default() {
                if entry.granted_access & DATA_ACCESS == 0 {
                    continue;
                }
                let mut duplicate = HANDLE::default();
                if DuplicateHandle(
                    process,
                    entry.handle_value,
                    GetCurrentProcess(),
                    &mut duplicate,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )
                .is_err()
                {
                    continue;
                }
                if GetFileType(duplicate) == FILE_TYPE_DISK {
                    let mut buffer = vec![0u16; 1024];
                    let len =
                        GetFinalPathNameByHandleW(duplicate, &mut buffer, FILE_NAME_NORMALIZED)
                            as usize;
                    if len > 0 && len < buffer.len() {
                        paths.push(String::from_utf16_lossy(&buffer[..len]));
                    }
                }
                let _ = CloseHandle(duplicate);
            }
            let _ = CloseHandle(process);
            Some(paths)
        }
    }

    /// The handle table of `process` (`ProcessHandleSnapshotInformation`).
    unsafe fn handle_entries(process: HANDLE) -> Option<Vec<HandleEntry>> {
        // `PROCESS_HANDLE_SNAPSHOT_INFORMATION`: two pointer-sized fields, then the entries.
        const HEADER: usize = 2 * std::mem::size_of::<usize>();
        let mut size: usize = 64 * 1024;
        for _ in 0..8 {
            // u64 keeps the buffer aligned for the pointer-sized fields.
            let mut buffer = vec![0u64; size.div_ceil(8)];
            let mut needed: u32 = 0;
            // SAFETY: the buffer is `buffer.len() * 8` bytes long and outlives the call.
            let status = unsafe {
                NtQueryInformationProcess(
                    process,
                    ProcessHandleInformation,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * 8) as u32,
                    &mut needed,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                size = (needed as usize).max(size * 2) + 4096;
                continue;
            }
            if status.0 < 0 {
                return None;
            }
            let bytes = buffer.len() * 8;
            let base = buffer.as_ptr().cast::<u8>();
            // SAFETY: the call filled the header and `count` entries after it within `bytes`.
            let count = unsafe { base.cast::<usize>().read() };
            let fits = count
                .checked_mul(std::mem::size_of::<HandleEntry>())
                .and_then(|len| len.checked_add(HEADER))
                .is_some_and(|len| len <= bytes);
            if !fits {
                return None;
            }
            let entries = (0..count)
                .map(|index| unsafe {
                    base.add(HEADER + index * std::mem::size_of::<HandleEntry>())
                        .cast::<HandleEntry>()
                        .read_unaligned()
                })
                .collect();
            return Some(entries);
        }
        None
    }
}

#[cfg(test)]
#[path = "codex_session_tests.rs"]
mod tests;
