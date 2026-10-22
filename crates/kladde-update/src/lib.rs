//! Self-update of the Kladde executable from GitHub releases.
//!
//! Every release carries a `latest.json` manifest naming version, notes and one download per platform
//! with its SHA-256. [`check`] compares it to the running version, [`install`] swaps the executable.

mod error;
mod install;
mod net;
mod version;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;

pub use error::UpdateError;

/// GitHub `owner/name` the releases are published under.
pub const REPO: &str = "SunYKnight/Kladde";

/// Upper bound for fetching the manifest, so a stalled server cannot hang a check forever.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// A newer release available for the running platform.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Version as written in the manifest, e.g. `0.2.0`.
    pub version: String,
    /// Release notes (the CHANGELOG section).
    pub notes: String,
    /// Download URL of this platform's executable.
    pub url: String,
    /// Lowercase hex SHA-256 of the download.
    pub sha256: String,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    platforms: HashMap<String, Asset>,
}

#[derive(Deserialize)]
struct Asset {
    url: String,
    sha256: String,
}

/// `https://github.com/SunYKnight/Kladde/releases/latest/download/latest.json`
pub fn manifest_url() -> String {
    format!("https://github.com/{REPO}/releases/latest/download/latest.json")
}

/// Manifest key of the running platform: `linux-x86_64` or `windows-x86_64`; `None` on other targets.
pub fn platform() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else {
        None
    }
}

/// Fetches the manifest (blocking) and returns the release for this platform if it is newer than `current`.
///
/// `Ok(None)` when `current` is not older than the manifest version or the manifest has no entry for this
/// platform. Both versions must be `MAJOR.MINOR.PATCH` with an optional leading `v`.
pub fn check(manifest_url: &str, current: &str) -> Result<Option<Release>, UpdateError> {
    let current = version::parse_version(current)?;
    net::check_url(manifest_url)?;
    let body = net::agent(manifest_url, Some(CHECK_TIMEOUT))
        .get(manifest_url)
        .call()
        .map_err(net::net_error)?
        .body_mut()
        .read_to_string()
        .map_err(net::net_error)?;
    let mut manifest: Manifest = serde_json::from_str(&body).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    let newer = version::parse_version(&manifest.version)? > current;
    let asset = platform().filter(|_| newer).and_then(|p| manifest.platforms.remove(p));
    asset
        .map(|asset| {
            Ok(Release {
                version: manifest.version,
                notes: manifest.notes,
                sha256: install::parse_sha256(&asset.sha256)?,
                url: asset.url,
            })
        })
        .transpose()
}

/// Downloads `release.url` (blocking), verifies its SHA-256 and replaces the running executable.
///
/// Returns the executable's path for [`relaunch`]. `progress(done, total)` is called after every chunk;
/// returning `false` cancels. The executable is only touched after the checksum matched.
pub fn install(release: &Release, progress: &dyn Fn(u64, Option<u64>) -> bool) -> Result<PathBuf, UpdateError> {
    install::install_to(release, &std::env::current_exe()?, progress)
}

/// Same as [`install`] but replaces `exe` instead of the running executable.
pub fn install_to(
    release: &Release,
    exe: &Path,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<PathBuf, UpdateError> {
    install::install_to(release, exe, progress)
}

/// Starts `exe` with `args`, detached from this process; the caller then closes its window.
pub fn relaunch(exe: &Path, args: &[OsString]) -> std::io::Result<()> {
    let mut command = Command::new(exe);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    detach(&mut command);
    // The child outlives this process by design; nobody waits for it.
    command.spawn().map(drop)
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Own process group, so signals aimed at the old process (e.g. terminal Ctrl+C) spare the new one.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut Command) {}

/// Deletes `<exe>.old` left behind by a Windows update; call once at startup.
pub fn cleanup_previous() -> std::io::Result<()> {
    install::remove_if_exists(&install::old_path(&std::env::current_exe()?))
}
