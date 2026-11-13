use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use sha2::{Digest as _, Sha256};

use super::*;
use crate::ward::session_handoff::{self, Handoff, HandoffTab, HandoffWindow};

const SETUP: &str = "WarpOssSetup.exe";
const CURRENT: &str = "v0.2026.11.13.08.43.oss_00";
const NEWER: &str = "v0.2026.11.13.12.00.oss_00";
const NEWEST: &str = "v0.2026.11.14.09.30.oss_00";
const OLDER: &str = "v0.2026.11.04.15.35.oss_00";
const INSTALLER_BYTES: &[u8] = b"installer of the newer version";

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn manifest(version: &str, sha256: Option<String>) -> ForkManifest {
    ForkManifest {
        version: version.to_owned(),
        url: None,
        sha256,
    }
}

/// The network and the installer of a test: the manifest it answers with and the installers
/// it was asked to start.
struct FakeEnv {
    manifest: Result<ForkManifest, String>,
    spawn_fails: bool,
    fetched: RefCell<usize>,
    spawned: RefCell<Vec<PathBuf>>,
}

impl FakeEnv {
    fn answering(manifest: Result<ForkManifest, String>) -> Self {
        Self {
            manifest,
            spawn_fails: false,
            fetched: RefCell::new(0),
            spawned: RefCell::new(Vec::new()),
        }
    }

    fn latest_is_newer() -> Self {
        Self::answering(Ok(manifest(NEWER, Some(sha256_hex(INSTALLER_BYTES)))))
    }
}

impl LaunchEnv for FakeEnv {
    fn fetch_manifest(&self) -> anyhow::Result<ForkManifest> {
        *self.fetched.borrow_mut() += 1;
        self.manifest.clone().map_err(|err| anyhow!(err))
    }

    fn spawn_installer(&self, installer: &Path) -> anyhow::Result<()> {
        if self.spawn_fails {
            return Err(anyhow!("access denied"));
        }
        self.spawned.borrow_mut().push(installer.to_owned());
        Ok(())
    }
}

/// A machine with a saved session (database, its log and the tab handoff) and the installer of
/// the newer version in the temporary directory.
struct Machine {
    _root: tempfile::TempDir,
    temp_dir: PathBuf,
    state_dir: PathBuf,
    database: PathBuf,
}

fn tabs() -> Handoff {
    Handoff {
        version: 1,
        written_at_ms: 1_000,
        from_version: Some(CURRENT.to_owned()),
        windows: vec![HandoffWindow {
            active_tab_index: 1,
            tabs: vec![
                HandoffTab {
                    title: Some("shell".to_owned()),
                    cwd: Some(r"C:\work".to_owned()),
                    shell: Some("pwsh".to_owned()),
                    resume_command: None,
                },
                HandoffTab {
                    title: None,
                    cwd: Some(r"C:\work\agent".to_owned()),
                    shell: Some("pwsh".to_owned()),
                    resume_command: Some("claude --resume 1234".to_owned()),
                },
            ],
        }],
    }
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let temp_dir = root.path().join("temp");
        let state_dir = root.path().join("data");
        fs::create_dir_all(&temp_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        let database = state_dir.join("warp.sqlite");
        fs::write(&database, b"session database").unwrap();
        fs::write(state_dir.join("warp.sqlite-wal"), b"latest writes").unwrap();
        session_handoff::write(&state_dir.join(session_handoff::FILE_NAME), &tabs()).unwrap();
        fs::write(installer_path(&temp_dir, NEWER, SETUP), INSTALLER_BYTES).unwrap();
        Self {
            _root: root,
            temp_dir,
            state_dir,
            database,
        }
    }

    fn launch(&self) -> Launch<'_> {
        Launch {
            enabled: true,
            plain_launch: true,
            per_user_install: true,
            current_version: Some(CURRENT),
            installer_file_name: SETUP,
            temp_dir: &self.temp_dir,
            state_dir: &self.state_dir,
            database: &self.database,
            now_ms: 5_000,
            stamp: "2026-11-13_10-42-07",
        }
    }

    /// The session files as they are now.
    fn session(&self) -> Vec<Option<Vec<u8>>> {
        session_backup::session_files(&self.state_dir, &self.database)
            .iter()
            .map(|path| fs::read(path).ok())
            .collect()
    }

    fn backups(&self) -> Vec<PathBuf> {
        session_backup::list(&self.state_dir.join(session_backup::DIR_NAME)).unwrap()
    }
}

#[test]
fn installer_name_is_the_one_the_download_creates() {
    let dir = tempfile::tempdir().unwrap();
    let file = tempfile::Builder::new()
        .prefix(INSTALLER_PREFIX)
        .rand_bytes(0)
        .suffix(&installer_suffix(NEWER, SETUP))
        .tempfile_in(dir.path())
        .unwrap();
    assert_eq!(file.path(), installer_path(dir.path(), NEWER, SETUP));
}

#[test]
fn version_is_read_from_the_installer_name() {
    assert_eq!(
        downloaded_version(".tmpv0.2026.11.13.12.00.oss_00-WarpOssSetup.exe", SETUP),
        Some(NEWER)
    );
    assert_eq!(downloaded_version(".tmpabc.tmp", SETUP), None);
    assert_eq!(downloaded_version(".tmp-WarpOssSetup.exe", SETUP), None);
    assert_eq!(downloaded_version("WarpOssSetup.exe", SETUP), None);
}

#[test]
fn newest_newer_installer_is_picked() {
    let name = |version: &str| format!("{INSTALLER_PREFIX}{}", installer_suffix(version, SETUP));
    let files = vec![
        (name(OLDER), 10),
        (name(CURRENT), 10),
        (name(NEWER), 10),
        (name(NEWEST), 10),
        (".tmp12345".to_owned(), 10),
    ];
    assert_eq!(
        newest_downloaded(files, SETUP, CURRENT),
        Some(NEWEST.to_owned())
    );
}

#[test]
fn empty_old_or_current_installers_are_not_picked() {
    let name = |version: &str| format!("{INSTALLER_PREFIX}{}", installer_suffix(version, SETUP));
    let files = vec![(name(OLDER), 10), (name(CURRENT), 10), (name(NEWER), 0)];
    assert_eq!(newest_downloaded(files, SETUP, CURRENT), None);
}

fn local() -> Local {
    Local {
        enabled: true,
        plain_launch: true,
        per_user_install: true,
        current_version: Some(CURRENT.to_owned()),
        downloaded: Some(NEWER.to_owned()),
        attempted: None,
    }
}

#[test]
fn local_conditions() {
    assert_eq!(check_local(&local()), Ok(NEWER.to_owned()));
    assert_eq!(
        check_local(&Local {
            enabled: false,
            ..local()
        }),
        Err(Skip::Disabled)
    );
    assert_eq!(
        check_local(&Local {
            plain_launch: false,
            ..local()
        }),
        Err(Skip::NotPlainLaunch)
    );
    assert_eq!(
        check_local(&Local {
            per_user_install: false,
            ..local()
        }),
        Err(Skip::NeedsAdministrator)
    );
    assert_eq!(
        check_local(&Local {
            current_version: None,
            ..local()
        }),
        Err(Skip::NoVersion)
    );
    assert_eq!(
        check_local(&Local {
            downloaded: None,
            ..local()
        }),
        Err(Skip::NothingDownloaded)
    );
    assert_eq!(
        check_local(&Local {
            attempted: Some(NEWER.to_owned()),
            ..local()
        }),
        Err(Skip::AlreadyTried(NEWER.to_owned()))
    );
    // An attempt at an older version does not hold back a newer one.
    assert_eq!(
        check_local(&Local {
            attempted: Some(CURRENT.to_owned()),
            ..local()
        }),
        Ok(NEWER.to_owned())
    );
}

#[test]
fn manifest_conditions() {
    assert_eq!(
        check_manifest(NEWER, &Ok(manifest(NEWER, Some("abc".to_owned())))),
        Ok("abc".to_owned())
    );
    assert_eq!(
        check_manifest(NEWER, &Err("timed out".to_owned())),
        Err(Skip::ManifestUnavailable("timed out".to_owned()))
    );
    assert_eq!(
        check_manifest(NEWER, &Ok(manifest(NEWEST, Some("abc".to_owned())))),
        Err(Skip::NotLatest {
            downloaded: NEWER.to_owned(),
            latest: NEWEST.to_owned(),
        })
    );
    assert_eq!(
        check_manifest(NEWER, &Ok(manifest(NEWER, None))),
        Err(Skip::NoChecksum)
    );
    assert_eq!(
        check_manifest(NEWER, &Ok(manifest(NEWER, Some("  ".to_owned())))),
        Err(Skip::NoChecksum)
    );
}

#[test]
fn downloaded_update_is_installed_after_a_copy_of_the_session() {
    let machine = Machine::new();
    let before = machine.session();
    let env = FakeEnv::latest_is_newer();

    let outcome = run(&machine.launch(), &env);

    let Outcome::Installing { version, backup } = outcome else {
        panic!("expected an install, got {outcome:?}");
    };
    assert_eq!(version, NEWER);
    assert_eq!(
        *env.spawned.borrow(),
        vec![installer_path(&machine.temp_dir, NEWER, SETUP)]
    );
    // The session itself is untouched, and the copy holds exactly it.
    assert_eq!(machine.session(), before);
    let backup = backup.expect("a copy of the session");
    assert_eq!(machine.backups(), vec![backup.clone()]);
    for (file, content) in session_backup::session_files(&machine.state_dir, &machine.database)
        .iter()
        .zip(&before)
    {
        let copy = backup.join(file.file_name().unwrap());
        assert_eq!(fs::read(copy).ok(), *content);
    }
    assert_eq!(
        read_attempt(&machine.state_dir).map(|attempt| attempt.version),
        Some(NEWER.to_owned())
    );
}

#[test]
fn no_network_starts_as_usual_and_touches_nothing() {
    let machine = Machine::new();
    let before = machine.session();
    let env = FakeEnv::answering(Err("operation timed out".to_owned()));

    let outcome = run(&machine.launch(), &env);

    assert_eq!(
        outcome,
        Outcome::Start(Skip::ManifestUnavailable("operation timed out".to_owned()))
    );
    assert!(env.spawned.borrow().is_empty());
    assert_eq!(machine.session(), before);
    assert!(machine.backups().is_empty());
    assert_eq!(read_attempt(&machine.state_dir), None);
}

#[test]
fn failing_installer_starts_as_usual_and_is_not_tried_again() {
    let machine = Machine::new();
    let before = machine.session();
    let env = FakeEnv {
        spawn_fails: true,
        ..FakeEnv::latest_is_newer()
    };

    let outcome = run(&machine.launch(), &env);

    assert!(
        matches!(outcome, Outcome::Start(Skip::InstallerFailed(_))),
        "{outcome:?}"
    );
    assert_eq!(machine.session(), before);

    // The next start does not try the same version again, and does not even ask the network.
    let env = FakeEnv::latest_is_newer();
    assert_eq!(
        run(&machine.launch(), &env),
        Outcome::Start(Skip::AlreadyTried(NEWER.to_owned()))
    );
    assert_eq!(*env.fetched.borrow(), 0);
    assert!(env.spawned.borrow().is_empty());
    assert_eq!(machine.session(), before);
}

#[test]
fn installer_that_does_not_match_the_checksum_is_not_started() {
    let machine = Machine::new();
    let before = machine.session();
    let env = FakeEnv::answering(Ok(manifest(NEWER, Some(sha256_hex(b"something else")))));

    let outcome = run(&machine.launch(), &env);

    assert!(
        matches!(outcome, Outcome::Start(Skip::ChecksumMismatch(_))),
        "{outcome:?}"
    );
    assert!(env.spawned.borrow().is_empty());
    assert_eq!(machine.session(), before);
    assert!(machine.backups().is_empty());
}

#[test]
fn newer_release_than_the_download_waits_for_its_own_download() {
    let machine = Machine::new();
    let env = FakeEnv::answering(Ok(manifest(NEWEST, Some(sha256_hex(INSTALLER_BYTES)))));

    let outcome = run(&machine.launch(), &env);

    assert!(
        matches!(outcome, Outcome::Start(Skip::NotLatest { .. })),
        "{outcome:?}"
    );
    assert!(env.spawned.borrow().is_empty());
}

#[test]
fn start_with_arguments_or_setting_off_asks_no_network() {
    let machine = Machine::new();
    let env = FakeEnv::latest_is_newer();

    let with_link = Launch {
        plain_launch: false,
        ..machine.launch()
    };
    assert_eq!(run(&with_link, &env), Outcome::Start(Skip::NotPlainLaunch));
    let off = Launch {
        enabled: false,
        ..machine.launch()
    };
    assert_eq!(run(&off, &env), Outcome::Start(Skip::Disabled));
    let for_all_users = Launch {
        per_user_install: false,
        ..machine.launch()
    };
    assert_eq!(
        run(&for_all_users, &env),
        Outcome::Start(Skip::NeedsAdministrator)
    );

    assert_eq!(*env.fetched.borrow(), 0);
    assert!(env.spawned.borrow().is_empty());
}

#[test]
fn nothing_downloaded_asks_no_network() {
    let machine = Machine::new();
    fs::remove_file(installer_path(&machine.temp_dir, NEWER, SETUP)).unwrap();
    let env = FakeEnv::latest_is_newer();

    assert_eq!(
        run(&machine.launch(), &env),
        Outcome::Start(Skip::NothingDownloaded)
    );
    assert_eq!(*env.fetched.borrow(), 0);
}

#[test]
fn update_and_restore_bring_back_the_same_tabs() {
    let machine = Machine::new();
    let env = FakeEnv::latest_is_newer();
    let Outcome::Installing {
        backup: Some(backup),
        ..
    } = run(&machine.launch(), &env)
    else {
        panic!("expected an install with a copy of the session");
    };

    // Whatever happened to the session during the update, the copy puts it back.
    let handoff = machine.state_dir.join(session_handoff::FILE_NAME);
    session_handoff::write(&handoff, &Handoff::default()).unwrap();
    fs::write(&machine.database, b"broken").unwrap();
    session_backup::restore(&backup, &machine.state_dir, &machine.database).unwrap();

    let restored = session_handoff::read(&handoff);
    assert_eq!(restored, Some(tabs()));
    assert_eq!(
        session_handoff::plan(restored, 0, 2_000),
        session_handoff::Plan::Open(tabs().windows)
    );
    assert_eq!(fs::read(&machine.database).unwrap(), b"session database");
}
