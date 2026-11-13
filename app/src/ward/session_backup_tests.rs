use super::*;

struct Dirs {
    _root: tempfile::TempDir,
    state_dir: PathBuf,
    database: PathBuf,
}

fn dirs() -> Dirs {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("data");
    fs::create_dir_all(&state_dir).unwrap();
    let database = state_dir.join("warp.sqlite");
    Dirs {
        _root: root,
        state_dir,
        database,
    }
}

#[test]
fn nothing_saved_makes_no_copy() {
    let dirs = dirs();
    assert_eq!(
        backup(&dirs.state_dir, &dirs.database, "2026-11-13_10-00-00", "v1").unwrap(),
        None
    );
    assert!(!dirs.state_dir.join(DIR_NAME).exists());
}

#[test]
fn copy_holds_the_files_that_exist() {
    let dirs = dirs();
    fs::write(&dirs.database, b"db").unwrap();
    fs::write(dirs.state_dir.join("warp.sqlite-wal"), b"wal").unwrap();
    fs::write(dirs.state_dir.join("warp.sqlite-shm"), b"shm").unwrap();

    let copy = backup(
        &dirs.state_dir,
        &dirs.database,
        "2026-11-13_10-00-00",
        "v0.2026.11.13.08.43.oss_00",
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        copy,
        dirs.state_dir
            .join(DIR_NAME)
            .join("2026-11-13_10-00-00_v0.2026.11.13.08.43.oss_00")
    );
    assert_eq!(fs::read(copy.join("warp.sqlite")).unwrap(), b"db");
    assert_eq!(fs::read(copy.join("warp.sqlite-wal")).unwrap(), b"wal");
    // The index is rebuilt by SQLite and the handoff did not exist.
    assert!(!copy.join("warp.sqlite-shm").exists());
    assert!(!copy.join(session_handoff::FILE_NAME).exists());
}

#[test]
fn copies_in_the_same_second_do_not_overwrite_each_other() {
    let dirs = dirs();
    fs::write(&dirs.database, b"db").unwrap();
    let first = backup(&dirs.state_dir, &dirs.database, "2026-11-13_10-00-00", "v1")
        .unwrap()
        .unwrap();
    let second = backup(&dirs.state_dir, &dirs.database, "2026-11-13_10-00-00", "v1")
        .unwrap()
        .unwrap();
    assert_ne!(first, second);
    assert!(first.is_dir() && second.is_dir());
}

#[test]
fn only_the_newest_copies_are_kept() {
    let dirs = dirs();
    fs::write(&dirs.database, b"db").unwrap();
    for minute in 0..KEEP + 3 {
        let stamp = format!("2026-11-13_10-{minute:02}-00");
        backup(&dirs.state_dir, &dirs.database, &stamp, "v1").unwrap();
    }
    let kept = list(&dirs.state_dir.join(DIR_NAME)).unwrap();
    assert_eq!(kept.len(), KEEP);
    let newest = format!("2026-11-13_10-{:02}-00_v1", KEEP + 2);
    assert_eq!(kept[0].file_name().unwrap().to_str().unwrap(), newest);
    assert!(
        kept.iter()
            .all(|dir| !dir.ends_with("2026-11-13_10-00-00_v1"))
    );
}

#[test]
fn label_cannot_leave_the_copies_directory() {
    let dirs = dirs();
    fs::write(&dirs.database, b"db").unwrap();
    let copy = backup(
        &dirs.state_dir,
        &dirs.database,
        "2026-11-13_10-00-00",
        r"..\x/y",
    )
    .unwrap()
    .unwrap();
    assert_eq!(copy.parent().unwrap(), dirs.state_dir.join(DIR_NAME));
}

#[test]
fn restore_puts_the_copy_back_and_drops_the_newer_log() {
    let dirs = dirs();
    let handoff = dirs.state_dir.join(session_handoff::FILE_NAME);
    fs::write(&dirs.database, b"db before").unwrap();
    fs::write(dirs.state_dir.join("warp.sqlite-wal"), b"wal before").unwrap();
    fs::write(&handoff, b"{\"version\":1}").unwrap();
    let copy = backup(&dirs.state_dir, &dirs.database, "2026-11-13_10-00-00", "v1")
        .unwrap()
        .unwrap();

    fs::write(&dirs.database, b"db after").unwrap();
    fs::remove_file(dirs.state_dir.join("warp.sqlite-wal")).unwrap();
    fs::write(dirs.state_dir.join("warp.sqlite-shm"), b"shm after").unwrap();
    fs::remove_file(&handoff).unwrap();

    restore(&copy, &dirs.state_dir, &dirs.database).unwrap();

    assert_eq!(fs::read(&dirs.database).unwrap(), b"db before");
    assert_eq!(
        fs::read(dirs.state_dir.join("warp.sqlite-wal")).unwrap(),
        b"wal before"
    );
    assert!(!dirs.state_dir.join("warp.sqlite-shm").exists());
    assert_eq!(fs::read(&handoff).unwrap(), b"{\"version\":1}");
}

#[test]
fn restore_without_a_database_in_the_copy_changes_nothing() {
    let dirs = dirs();
    fs::write(&dirs.database, b"db now").unwrap();
    let empty = dirs.state_dir.join(DIR_NAME).join("empty");
    fs::create_dir_all(&empty).unwrap();

    assert!(restore(&empty, &dirs.state_dir, &dirs.database).is_err());
    assert_eq!(fs::read(&dirs.database).unwrap(), b"db now");
}
