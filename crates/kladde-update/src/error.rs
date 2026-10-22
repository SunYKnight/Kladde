use std::fmt;
use std::io;
use std::path::PathBuf;

/// Errors of update checks and installs; `Display` texts are shown to the user as is.
#[derive(Debug)]
pub enum UpdateError {
    /// A version string is not `MAJOR.MINOR.PATCH`.
    InvalidVersion(String),
    /// A URL is neither https nor plain http to the local machine.
    InsecureUrl(String),
    /// The server could not be reached or the transfer broke off.
    Network(String),
    /// The server answered with an error status.
    Http(u16),
    /// The manifest is not valid JSON or misses required fields.
    Manifest(String),
    /// The downloaded file does not match the published SHA-256.
    Checksum { expected: String, actual: String },
    /// The progress callback asked to stop.
    Cancelled,
    /// The executable's folder cannot be written.
    NotWritable { dir: PathBuf, source: io::Error },
    /// The new file could not be put in place and the old executable stays at `old`.
    RestoreFailed { old: PathBuf, exe: PathBuf, source: io::Error },
    /// Any other file system error.
    Io(io::Error),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::InvalidVersion(v) => write!(f, "Invalid version \"{v}\" (expected MAJOR.MINOR.PATCH)."),
            UpdateError::InsecureUrl(url) => write!(f, "Refusing to download over an insecure connection: {url}"),
            UpdateError::Network(msg) => write!(f, "Could not reach the update server: {msg}"),
            UpdateError::Http(code) => write!(f, "The update server answered with HTTP status {code}."),
            UpdateError::Manifest(msg) => write!(f, "The update information is invalid: {msg}"),
            UpdateError::Checksum { expected, actual } => write!(
                f,
                "The downloaded file is damaged (SHA-256 {actual}, expected {expected}). Nothing was changed."
            ),
            UpdateError::Cancelled => write!(f, "The update was cancelled. Nothing was changed."),
            UpdateError::NotWritable { dir, source } => write!(
                f,
                "Cannot write to the folder {} ({source}). Move Kladde to a writable folder or update it manually.",
                dir.display()
            ),
            UpdateError::RestoreFailed { old, exe, source } => write!(
                f,
                "The update failed ({source}) and the previous version could not be restored. \
                 Rename {} to {} manually.",
                old.display(),
                exe.display()
            ),
            UpdateError::Io(e) => write!(f, "File error during update: {e}"),
        }
    }
}

impl std::error::Error for UpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UpdateError::NotWritable { source, .. } | UpdateError::RestoreFailed { source, .. } => Some(source),
            UpdateError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for UpdateError {
    fn from(e: io::Error) -> Self {
        UpdateError::Io(e)
    }
}

/// `Err(err())` unless `cond` holds; the closure keeps the success path free of allocations.
pub(crate) fn ensure(cond: bool, err: impl FnOnce() -> UpdateError) -> Result<(), UpdateError> {
    if cond {
        Ok(())
    } else {
        Err(err())
    }
}
