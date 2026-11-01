use super::*;

fn demo(session_id: &str, started_at_ms: i64) -> SavedSession {
    SavedSession {
        version: FORMAT_VERSION,
        session_id: session_id.to_owned(),
        provider: "claude".to_owned(),
        phase: SavedPhase::Launched,
        status: SessionStatus::Running,
        reason: None,
        started_at_ms,
        launched_at_ms: started_at_ms + 1,
        assignment: Assignment {
            session_id: session_id.to_owned(),
            branch: "fix/4644-demo".to_owned(),
            ..Assignment::default()
        },
        worktree: Some("C:/agents-work/SteepMans/demo/.worktrees/s-good".to_owned()),
        worktree_note: None,
        runner_pid: Some(4242),
        agent: Some(SavedAgent {
            agent_session_id: "a-1".to_owned(),
            pid: Some(4343),
            remote_control_url: Some("https://claude.ai/code/session_1".to_owned()),
        }),
        transcript: Some("C:/home/.claude/projects/p/a-1.jsonl".to_owned()),
        role: Some("executor".to_owned()),
        journal: SavedJournal {
            offset: 1234,
            next_seq: 7,
        },
        prepare: None,
    }
}

#[test]
fn a_state_file_comes_back_as_it_was_written() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agents-work");
    let state = demo("s-good", 10);
    let path = path(&root, &state.session_id).unwrap();
    write(&path, &state).unwrap();
    assert!(path.ends_with("session.json"));
    assert_eq!(read(&path).unwrap(), state);

    // The temporary file of the write is gone, and the state of a session that ended too.
    assert!(!path.with_extension("json.tmp").exists());
    remove(&path);
    assert!(read(&path).is_none());
    remove(&path);
}

#[test]
fn a_scan_finds_the_sessions_in_the_order_they_started() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agents-work");
    assert!(scan(&root).is_empty());
    for (id, started) in [("s-second", 20), ("s-first", 10)] {
        let state = demo(id, started);
        write(&path(&root, id).unwrap(), &state).unwrap();
    }
    // A directory of the preparation phase without a state file is no session of ours.
    std::fs::create_dir_all(root.join(launch::AGENTS_DIR).join("s-gone")).unwrap();
    // Neither is a file of another format version.
    let other = demo("s-older", 5);
    let other_path = path(&root, "s-older").unwrap();
    write(
        &other_path,
        &SavedSession {
            version: FORMAT_VERSION + 1,
            ..other
        },
    )
    .unwrap();
    assert!(read(&other_path).is_none());

    let found: Vec<String> = scan(&root)
        .into_iter()
        .map(|state| state.session_id)
        .collect();
    assert_eq!(found, ["s-first", "s-second"]);
}
