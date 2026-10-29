//! Autoupdate for self-published builds of the OSS channel.
//!
//! Warp's own release infrastructure only serves the Stable, Preview and Dev channels. OSS builds
//! that are published to an S3-compatible bucket instead describe the latest release with a small
//! manifest stored at `<releases_base_url>/stable/manifest.json`:
//!
//! ```json
//! {
//!   "version": "v0.2026.10.29.12.00.oss_00",
//!   "url": "https://example.com/warp-fork/stable/v0.2026.10.29.12.00.oss_00/WarpOssSetup.exe",
//!   "sha256": "<hex digest of the installer>"
//! }
//! ```
//!
//! `url` is optional and defaults to `<releases_base_url>/stable/<version>/<installer name>`.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use channel_versions::VersionInfo;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use serde::Deserialize;
use warp_core::channel::ChannelState;

const MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ForkManifest {
    pub version: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
}

lazy_static! {
    /// The manifest from the most recent successful update check, used by the download step to
    /// locate and verify the installer.
    static ref LATEST_MANIFEST: Mutex<Option<ForkManifest>> = Default::default();
}

/// The directory that holds the manifest and the per-version release assets.
pub fn stable_dir_url() -> Option<String> {
    let base = ChannelState::releases_base_url();
    let base = base.trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/stable"))
}

/// Fetches the release manifest and returns the version it describes.
pub async fn fetch_version(client: &http_client::Client, nonce: &str) -> Result<VersionInfo> {
    let Some(stable_dir) = stable_dir_url() else {
        bail!("No releases URL configured for this build");
    };
    let url = format!("{stable_dir}/manifest.json?r={nonce}");
    log::info!("Fetching release manifest from {stable_dir}/manifest.json");
    let manifest: ForkManifest = client
        .get(&url)
        .timeout(MANIFEST_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("Failed to parse release manifest")?;
    log::info!("Release manifest lists version {}", manifest.version);
    let version_info = VersionInfo::new(manifest.version.clone());
    *LATEST_MANIFEST.lock() = Some(manifest);
    Ok(version_info)
}

/// Returns the manifest entry for the given version, if it was the result of the last check.
pub fn manifest_for_version(version: &str) -> Option<ForkManifest> {
    LATEST_MANIFEST
        .lock()
        .as_ref()
        .filter(|manifest| manifest.version == version)
        .cloned()
}

/// Checks that the downloaded bytes match the digest published in the manifest.
pub fn verify_sha256(bytes: &[u8], expected: Option<&str>) -> Result<()> {
    use sha2::{Digest as _, Sha256};

    let Some(expected) = expected else {
        bail!("Release manifest has no sha256 for the installer; refusing to install it");
    };
    let actual = hex::encode(Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        bail!("Installer checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "fork_manifest_tests.rs"]
mod tests;
