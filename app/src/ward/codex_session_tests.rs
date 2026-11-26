use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::*;

const A: &str = "01a1f7f6-0000-7000-8000-00000000000a";
const B: &str = "01a210f6-0000-7000-8000-00000000000b";
const C: &str = "01a22222-0000-7000-8000-00000000000c";
const REPO: &str = "C:\\work\\e2e-exo-test";

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + seconds)
}

fn rollout(id: &str, created: u64, modified: u64) -> Rollout {
    Rollout {
        id: id.to_owned(),
        cwd: Some(REPO.to_owned()),
        created: Some(at(created)),
        modified: at(modified),
    }
}

fn taken(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

fn repo() -> Option<&'static Path> {
    Some(Path::new("c:/work/e2e-exo-test/"))
}

#[test]
fn resumed_tab_keeps_its_session_when_another_tab_wrote_last() {
    // Two tabs in one project: this one runs `codex resume A`, the other one wrote last (B).
    let rollouts = [rollout(A, 0, 100), rollout(B, 0, 200)];
    let chosen = choose(
        &Launch::Resume(Some(A.to_owned())),
        &[],
        None,
        &rollouts,
        repo(),
        at(50),
        &taken(&[B]),
    );
    assert_eq!(chosen.as_deref(), Some(A));
}

#[test]
fn open_files_win_over_the_resumed_id() {
    // Started as `codex resume A`, then `/new` in Codex: the process now writes C.
    let rollouts = [rollout(A, 0, 100), rollout(C, 120, 200)];
    let chosen = choose(
        &Launch::Resume(Some(A.to_owned())),
        &[C.to_owned()],
        Some(A),
        &rollouts,
        repo(),
        at(50),
        &HashSet::new(),
    );
    assert_eq!(chosen.as_deref(), Some(C));
}

#[test]
fn several_open_sessions_go_by_the_latest_write() {
    let rollouts = [rollout(A, 0, 300), rollout(B, 0, 200)];
    let chosen = choose(
        &Launch::New,
        &[B.to_owned(), A.to_owned()],
        None,
        &rollouts,
        repo(),
        at(50),
        &HashSet::new(),
    );
    assert_eq!(chosen.as_deref(), Some(A));
}

#[test]
fn new_session_skips_sessions_of_other_processes() {
    // `codex` started at 100 in the project where another tab holds B, written a second ago.
    let rollouts = [rollout(B, 0, 200), rollout(C, 110, 150)];
    let chosen = choose(
        &Launch::New,
        &[],
        None,
        &rollouts,
        repo(),
        at(100),
        &taken(&[B]),
    );
    assert_eq!(chosen.as_deref(), Some(C));
}

#[test]
fn new_session_does_not_take_an_older_session() {
    // Nothing created since the start: an older session written meanwhile belongs to a tab
    // whose Codex has exited or to another app; this tab has no session yet.
    let rollouts = [rollout(A, 0, 200)];
    let chosen = choose(
        &Launch::New,
        &[],
        None,
        &rollouts,
        repo(),
        at(100),
        &HashSet::new(),
    );
    assert_eq!(chosen, None);
}

#[test]
fn new_session_takes_the_first_one_created_since_start() {
    let rollouts = [rollout(C, 150, 400), rollout(A, 110, 300)];
    let chosen = choose(
        &Launch::New,
        &[],
        None,
        &rollouts,
        repo(),
        at(100),
        &HashSet::new(),
    );
    assert_eq!(chosen.as_deref(), Some(A));
}

#[test]
fn new_session_keeps_what_it_was_given() {
    let rollouts = [rollout(A, 110, 120), rollout(C, 130, 400)];
    let chosen = choose(
        &Launch::New,
        &[],
        Some(C),
        &rollouts,
        repo(),
        at(100),
        &HashSet::new(),
    );
    assert_eq!(chosen.as_deref(), Some(C));
}

#[test]
fn resume_without_id_takes_latest_free_session_in_its_directory() {
    let mut elsewhere = rollout(C, 0, 500);
    elsewhere.cwd = Some("C:\\work\\other".to_owned());
    let rollouts = [rollout(A, 0, 200), rollout(B, 0, 300), elsewhere];
    let chosen = choose(
        &Launch::Resume(None),
        &[],
        None,
        &rollouts,
        repo(),
        at(100),
        &taken(&[B]),
    );
    assert_eq!(chosen.as_deref(), Some(A));
}

#[test]
fn child_thread_is_no_evidence_of_a_new_session() {
    let child_of_b = Rollout {
        id: B.to_owned(),
        cwd: Some(REPO.to_owned()),
        created: None,
        modified: at(200),
    };
    let chosen = choose(
        &Launch::New,
        &[],
        None,
        &[child_of_b],
        repo(),
        at(100),
        &HashSet::new(),
    );
    assert_eq!(chosen, None);
}

#[test]
fn others_hold_what_they_were_given_or_resumed() {
    let me = ProcessKey {
        pid: 1,
        started: 10,
    };
    let given = ProcessKey {
        pid: 2,
        started: 10,
    };
    let resumed = ProcessKey {
        pid: 3,
        started: 10,
    };
    let fresh = ProcessKey {
        pid: 4,
        started: 10,
    };
    let claims = HashMap::from([(me, A.to_owned()), (given, B.to_owned())]);
    let others = [
        (given, Some(A.to_owned())),
        (resumed, Some(C.to_owned())),
        (fresh, None),
    ];
    // Process 2 was resumed with A but holds B now; this process's own session is not taken.
    assert_eq!(taken_by_others(&claims, me, &others), taken(&[B, C]));
}

#[test]
fn two_new_processes_in_one_directory_get_different_sessions() {
    // Both started at 100 with `codex`, two sessions created since. Separate pids, so the
    // shared claims do not collide with other tests.
    let first = ProcessKey {
        pid: 4_000_001,
        started: 1_800_000_100,
    };
    let second = ProcessKey {
        pid: 4_000_002,
        started: 1_800_000_100,
    };
    let rollouts = [rollout(A, 110, 300), rollout(C, 120, 310)];
    let first_id = assign(
        first,
        &Launch::New,
        &[],
        &rollouts,
        repo(),
        &[(second, None)],
    );
    let second_id = assign(
        second,
        &Launch::New,
        &[],
        &rollouts,
        repo(),
        &[(first, None)],
    );
    assert_eq!(first_id.as_deref(), Some(A));
    assert_eq!(second_id.as_deref(), Some(C));
    // Polled again, each keeps its own.
    let again = assign(
        first,
        &Launch::New,
        &[],
        &rollouts,
        repo(),
        &[(second, None)],
    );
    assert_eq!(again.as_deref(), Some(A));
}

#[test]
fn reads_thread_ids_from_codex_file_names() {
    assert_eq!(
        thread_id_from_path(&format!(
            "\\\\?\\C:\\Users\\u\\.codex\\sessions\\2026\\11\\26\\rollout-2026-11-26T10-00-00-{A}.jsonl"
        ))
        .as_deref(),
        Some(A)
    );
    assert_eq!(
        thread_id_from_path(&format!("/home/u/.codex/thread-writer-locks/{B}.lock")).as_deref(),
        Some(B)
    );
    // A lock elsewhere, the coordination lock and other files are not threads.
    assert_eq!(thread_id_from_path(&format!("/tmp/{B}.lock")), None);
    assert_eq!(
        thread_id_from_path("/home/u/.codex/thread-writer-locks/.coordination.lock"),
        None
    );
    assert_eq!(
        thread_id_from_path("C:\\Users\\u\\.codex\\history.jsonl"),
        None
    );
}

/// The handle query finds the files this very process holds open.
#[cfg(any(windows, target_os = "linux"))]
#[test]
fn finds_files_the_process_holds_open() {
    let dir = tempfile::tempdir().unwrap();
    let locks = dir.path().join("thread-writer-locks");
    std::fs::create_dir_all(&locks).unwrap();
    let rollout_path = dir
        .path()
        .join(format!("rollout-2026-11-26T10-00-00-{A}.jsonl"));
    let _rollout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rollout_path)
        .unwrap();
    let _lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(locks.join(format!("{B}.lock")))
        .unwrap();

    let ids = open_thread_ids(std::process::id()).unwrap();
    assert!(ids.contains(&A.to_owned()), "{ids:?}");
    assert!(ids.contains(&B.to_owned()), "{ids:?}");
}
