//! Ward installs an update it downloaded earlier when it starts, before anything else happens.
//!
//! While Ward runs it downloads a new version in the background (`crate::autoupdate`) and keeps
//! the installer in the temporary directory as `.tmp<version>-WarpOssSetup.exe`; the file
//! outlives the process. The next plain start of Ward (Start menu, taskbar, the installer
//! itself) checks for such an installer before the app opens a window, reads the session or
//! starts the agents worker. If it is newer than the running version, is the latest one in the
//! release manifest and matches the manifest's checksum, Ward copies the saved session
//! ([`super::session_backup`]), starts the installer with its progress window and exits. The
//! installer waits for Ward to exit, replaces the program files and starts the new version,
//! which restores the session exactly as after a plain restart: nothing of the session was read
//! or written by the start that handed over to the installer.
//!
//! Nothing is installed and the start goes on as usual when:
//! - "Install updates when Ward starts" is off (Settings → Updates);
//! - Ward was started with arguments: a link to open (an agent tab, a `new_tab` URI), the crash
//!   recovery watcher, a CLI command. The installer would start Ward without them;
//! - Ward is installed for all users: the installer would ask for administrator rights;
//! - no newer installer was downloaded: Ward does not download one at start;
//! - this version was already tried at a start: a failed installer must not keep Ward from
//!   starting (at most one attempt per version);
//! - the manifest cannot be read within [`MANIFEST_TIMEOUT`] (no network) or lists another
//!   version, or the installer does not match its checksum;
//! - the session cannot be copied or the installer cannot be started.
//!
//! The decision steps are pure functions ([`check_local`], [`check_manifest`]) and [`run`] takes
//! the network and the installer as a [`LaunchEnv`], so tests can play the whole start.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use channel_versions::ParsedVersion;
use serde::{Deserialize, Serialize};

use super::session_backup;
use crate::autoupdate::fork_manifest::{self, ForkManifest};

/// Private user preferences key of the setting; on unless it is "false".
pub const ENABLED_KEY: &str = "WardInstallUpdatesOnLaunch";

/// The longest a start waits for the release manifest.
pub const MANIFEST_TIMEOUT: Duration = Duration::from_secs(2);

/// What `tempfile::Builder` puts in front of the name of the downloaded installer
/// (`crate::autoupdate` uses no random part, so the name is fixed per version).
pub const INSTALLER_PREFIX: &str = ".tmp";

/// The record of the last version tried at a start, in the state directory.
pub const ATTEMPT_FILE: &str = "ward_launch_update.json";

/// The part of the downloaded installer's name after [`INSTALLER_PREFIX`].
pub fn installer_suffix(version: &str, installer_file_name: &str) -> String {
    format!("{version}-{installer_file_name}")
}

/// Where the installer of `version` is downloaded to.
pub fn installer_path(temp_dir: &Path, version: &str, installer_file_name: &str) -> PathBuf {
    temp_dir.join(format!(
        "{INSTALLER_PREFIX}{}",
        installer_suffix(version, installer_file_name)
    ))
}

/// The version of a downloaded installer, read from its file name.
pub fn downloaded_version<'a>(file_name: &'a str, installer_file_name: &str) -> Option<&'a str> {
    let version = file_name
        .strip_prefix(INSTALLER_PREFIX)?
        .strip_suffix(installer_file_name)?
        .strip_suffix('-')?;
    (!version.is_empty()).then_some(version)
}

fn parse(version: &str) -> Option<ParsedVersion> {
    ParsedVersion::try_from(version).ok()
}

/// The newest of the downloaded installers (file name and size) that is newer than `current`.
/// Empty files are left out: a download that never got its bytes.
pub fn newest_downloaded(
    files: impl IntoIterator<Item = (String, u64)>,
    installer_file_name: &str,
    current: &str,
) -> Option<String> {
    let current = parse(current)?;
    files
        .into_iter()
        .filter(|(_, size)| *size > 0)
        .filter_map(|(name, _)| {
            let version = downloaded_version(&name, installer_file_name)?.to_owned();
            let parsed = parse(&version)?;
            (parsed > current).then_some((parsed, version))
        })
        .max()
        .map(|(_, version)| version)
}

fn installers_in(temp_dir: &Path) -> Vec<(String, u64)> {
    let Ok(entries) = fs::read_dir(temp_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with(INSTALLER_PREFIX) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| (name, metadata.len()))
        })
        .collect()
}

/// The version a start last handed over to the installer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attempt {
    pub version: String,
    /// Unix time in milliseconds.
    pub at_ms: u64,
}

pub fn read_attempt(state_dir: &Path) -> Option<Attempt> {
    let raw = fs::read_to_string(state_dir.join(ATTEMPT_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn write_attempt(state_dir: &Path, attempt: &Attempt) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join(ATTEMPT_FILE);
    let temp = path.with_extension("json.tmp");
    fs::write(
        &temp,
        serde_json::to_string(attempt).map_err(io::Error::other)?,
    )?;
    fs::rename(&temp, path)
}

/// Why a start does not install an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    Disabled,
    /// Started with arguments, which the installer would not pass on.
    NotPlainLaunch,
    /// Installed for all users: the installer needs administrator rights.
    NeedsAdministrator,
    /// The build carries no version to compare with.
    NoVersion,
    NothingDownloaded,
    /// This version was already handed to the installer by a start.
    AlreadyTried(String),
    ManifestUnavailable(String),
    /// The manifest lists another version than the downloaded one.
    NotLatest {
        downloaded: String,
        latest: String,
    },
    NoChecksum,
    ChecksumMismatch(String),
    /// The attempt could not be recorded, so a failing installer could block every start.
    AttemptNotRecorded(String),
    BackupFailed(String),
    InstallerFailed(String),
}

impl Skip {
    pub fn text(&self) -> String {
        match self {
            Skip::Disabled => "the setting is off".to_owned(),
            Skip::NotPlainLaunch => "started with arguments".to_owned(),
            Skip::NeedsAdministrator => "installed for all users".to_owned(),
            Skip::NoVersion => "this build has no version".to_owned(),
            Skip::NothingDownloaded => "no newer installer downloaded".to_owned(),
            Skip::AlreadyTried(version) => format!("{version} was already tried at a start"),
            Skip::ManifestUnavailable(err) => format!("release manifest unavailable: {err}"),
            Skip::NotLatest { downloaded, latest } => {
                format!("downloaded {downloaded}, but the latest release is {latest}")
            }
            Skip::NoChecksum => "the release manifest has no checksum".to_owned(),
            Skip::ChecksumMismatch(err) => format!("installer check failed: {err}"),
            Skip::AttemptNotRecorded(err) => format!("could not record the attempt: {err}"),
            Skip::BackupFailed(err) => format!("could not copy the session: {err}"),
            Skip::InstallerFailed(err) => format!("could not start the installer: {err}"),
        }
    }
}

/// What a start knows without the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Local {
    pub enabled: bool,
    pub plain_launch: bool,
    pub per_user_install: bool,
    pub current_version: Option<String>,
    /// The newest downloaded installer newer than the running version.
    pub downloaded: Option<String>,
    /// The version the last start handed to the installer.
    pub attempted: Option<String>,
}

/// The local conditions; returns the version to install.
pub fn check_local(local: &Local) -> Result<String, Skip> {
    if !local.enabled {
        return Err(Skip::Disabled);
    }
    if !local.plain_launch {
        return Err(Skip::NotPlainLaunch);
    }
    if !local.per_user_install {
        return Err(Skip::NeedsAdministrator);
    }
    if local.current_version.is_none() {
        return Err(Skip::NoVersion);
    }
    let Some(downloaded) = &local.downloaded else {
        return Err(Skip::NothingDownloaded);
    };
    if local.attempted.as_deref() == Some(downloaded.as_str()) {
        return Err(Skip::AlreadyTried(downloaded.clone()));
    }
    Ok(downloaded.clone())
}

/// The downloaded version must be the one the manifest lists now (a newer release or a
/// rollback means this installer is not the one to install), with a checksum to check it by.
/// Returns the checksum.
pub fn check_manifest(
    downloaded: &str,
    manifest: &Result<ForkManifest, String>,
) -> Result<String, Skip> {
    let manifest = manifest
        .as_ref()
        .map_err(|err| Skip::ManifestUnavailable(err.clone()))?;
    if manifest.version != downloaded {
        return Err(Skip::NotLatest {
            downloaded: downloaded.to_owned(),
            latest: manifest.version.clone(),
        });
    }
    manifest
        .sha256
        .clone()
        .filter(|sha| !sha.trim().is_empty())
        .ok_or(Skip::NoChecksum)
}

/// The network and the installer, behind a trait for tests.
pub trait LaunchEnv {
    /// Reads the release manifest, giving up after [`MANIFEST_TIMEOUT`].
    fn fetch_manifest(&self) -> Result<ForkManifest>;
    /// Starts the installer; it waits for Ward to exit, installs and starts Ward again.
    fn spawn_installer(&self, installer: &Path) -> Result<()>;
}

/// Everything about this start that [`run`] looks at.
pub struct Launch<'a> {
    pub enabled: bool,
    pub plain_launch: bool,
    pub per_user_install: bool,
    pub current_version: Option<&'a str>,
    /// `WarpOssSetup.exe`.
    pub installer_file_name: &'a str,
    pub temp_dir: &'a Path,
    pub state_dir: &'a Path,
    pub database: &'a Path,
    /// Unix time in milliseconds.
    pub now_ms: u64,
    /// Orders the session copies (`2026-11-13_10-42-07`).
    pub stamp: &'a str,
}

/// What the start does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Go on starting as usual.
    Start(Skip),
    /// The installer runs: exit now, it starts the new version.
    Installing {
        version: String,
        backup: Option<PathBuf>,
    },
}

/// Decides and, when everything holds, copies the session and starts the installer. The
/// session files are only read (for the copy), never changed.
pub fn run(launch: &Launch<'_>, env: &dyn LaunchEnv) -> Outcome {
    let local = Local {
        enabled: launch.enabled,
        plain_launch: launch.plain_launch,
        per_user_install: launch.per_user_install,
        current_version: launch.current_version.map(str::to_owned),
        downloaded: launch.current_version.and_then(|current| {
            newest_downloaded(
                installers_in(launch.temp_dir),
                launch.installer_file_name,
                current,
            )
        }),
        attempted: read_attempt(launch.state_dir).map(|attempt| attempt.version),
    };
    let version = match check_local(&local) {
        Ok(version) => version,
        Err(skip) => return Outcome::Start(skip),
    };

    let manifest = env.fetch_manifest().map_err(|err| format!("{err:#}"));
    let sha256 = match check_manifest(&version, &manifest) {
        Ok(sha256) => sha256,
        Err(skip) => return Outcome::Start(skip),
    };
    let installer = installer_path(launch.temp_dir, &version, launch.installer_file_name);
    let checked = fs::read(&installer)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| fork_manifest::verify_sha256(&bytes, Some(sha256.as_str())));
    if let Err(err) = checked {
        return Outcome::Start(Skip::ChecksumMismatch(format!("{err:#}")));
    }

    // Recorded before anything starts: an installer that fails without starting Ward again
    // leaves the record, and the next start goes on as usual instead of trying again.
    let attempt = Attempt {
        version: version.clone(),
        at_ms: launch.now_ms,
    };
    if let Err(err) = write_attempt(launch.state_dir, &attempt) {
        return Outcome::Start(Skip::AttemptNotRecorded(err.to_string()));
    }

    let label = launch.current_version.unwrap_or("unknown");
    let backup =
        match session_backup::backup(launch.state_dir, launch.database, launch.stamp, label) {
            Ok(backup) => backup,
            Err(err) => return Outcome::Start(Skip::BackupFailed(err.to_string())),
        };

    match env.spawn_installer(&installer) {
        Ok(()) => Outcome::Installing { version, backup },
        Err(err) => Outcome::Start(Skip::InstallerFailed(format!("{err:#}"))),
    }
}

/// "Install updates when Ward starts", on by default.
pub fn is_enabled_in(preferences: &dyn warpui_extras::user_preferences::UserPreferences) -> bool {
    preferences
        .read_value(ENABLED_KEY)
        .ok()
        .flatten()
        .is_none_or(|value| value.trim() != "false")
}

pub fn is_enabled(app: &warpui::AppContext) -> bool {
    use warp_core::user_preferences::GetUserPreferences as _;
    is_enabled_in(app.private_user_preferences())
}

pub fn set_enabled(enabled: bool, app: &mut warpui::AppContext) {
    use warp_core::user_preferences::GetUserPreferences as _;
    if let Err(err) = app
        .private_user_preferences()
        .write_value(ENABLED_KEY, enabled.to_string())
    {
        log::warn!("Failed to save the Ward setting to install updates at start: {err:?}");
    }
}

/// Whether the process got no arguments at all: a start from the Start menu, the taskbar or the
/// installer.
pub fn is_plain_launch() -> bool {
    std::env::args_os().count() <= 1
}

#[cfg(windows)]
mod platform {
    use std::path::Path;

    use anyhow::{Context as _, Result, bail};

    use super::{LaunchEnv, MANIFEST_TIMEOUT};
    use crate::autoupdate::fork_manifest::{self, ForkManifest};

    pub struct RealEnv;

    impl LaunchEnv for RealEnv {
        fn fetch_manifest(&self) -> Result<ForkManifest> {
            let Some(stable_dir) = fork_manifest::stable_dir_url() else {
                bail!("no releases URL in this build");
            };
            let nonce = chrono::Utc::now().timestamp_millis();
            let client = reqwest::blocking::Client::builder()
                .timeout(MANIFEST_TIMEOUT)
                .connect_timeout(MANIFEST_TIMEOUT)
                .build()?;
            let manifest = client
                .get(format!("{stable_dir}/manifest.json?r={nonce}"))
                .send()?
                .error_for_status()?
                .json::<ForkManifest>()
                .context("unreadable release manifest")?;
            Ok(manifest)
        }

        fn spawn_installer(&self, installer: &Path) -> Result<()> {
            crate::autoupdate::spawn_installer_at_launch(installer)
        }
    }
}

/// What the start before the app does about a downloaded update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeforeStart {
    /// Go on starting.
    Start,
    /// Exit at once: the installer runs and starts Ward again when it is done.
    Exit,
}

/// Installs a downloaded update before Ward starts. Call before the app opens anything: after
/// the single-instance check and the user preferences, before the crash recovery watcher, the
/// terminal server and the app itself.
#[cfg(windows)]
pub fn before_start(
    preferences: &dyn warpui_extras::user_preferences::UserPreferences,
) -> BeforeStart {
    use warp_core::channel::ChannelState;

    use crate::features::FeatureFlag;

    if !crate::features::is_fork_oss() || !FeatureFlag::Autoupdate.is_enabled() {
        return BeforeStart::Start;
    }
    let enabled = is_enabled_in(preferences);
    let plain_launch = is_plain_launch();
    let Ok(installer_file_name) = crate::autoupdate::installer_file_name() else {
        return BeforeStart::Start;
    };
    let state_dir = warp_core::paths::state_dir();
    let database = session_backup::database_path();
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let launch = Launch {
        enabled,
        plain_launch,
        // Read only when it can matter: it is a registry lookup.
        per_user_install: enabled && plain_launch && crate::autoupdate::can_install_unattended(),
        current_version: ChannelState::app_version(),
        installer_file_name: &installer_file_name,
        temp_dir: &std::env::temp_dir(),
        state_dir: &state_dir,
        database: &database,
        now_ms: chrono::Utc::now().timestamp_millis().max(0) as u64,
        stamp: &stamp,
    };
    match run(&launch, &platform::RealEnv) {
        Outcome::Installing { version, backup } => {
            log::info!(
                "Ward: installing {version} before the start (session copy: {})",
                backup
                    .as_deref()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| "nothing to copy".to_owned())
            );
            log::logger().flush();
            BeforeStart::Exit
        }
        Outcome::Start(skip) => {
            log::info!("Ward: no update install at start: {}", skip.text());
            BeforeStart::Start
        }
    }
}

#[cfg(test)]
#[path = "launch_update_tests.rs"]
mod tests;
