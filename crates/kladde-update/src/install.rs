use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{ensure, UpdateError};
use crate::net::{agent, check_url, net_error};
use crate::Release;

const CHUNK: usize = 64 * 1024;

/// Download target next to the executable; deleted on drop unless it was moved into place.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl TempFile {
    fn new(dir: &Path, exe: &Path) -> TempFile {
        let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        TempFile { path: dir.join(format!(".{name}.{}.update", std::process::id())), armed: true }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            // Only reached on failure; the caller already reports the primary error
            // and a leftover hidden temp file does no harm.
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Downloads, verifies and swaps in `release` at `exe`; see [`crate::install_to`].
pub(crate) fn install_to(
    release: &Release,
    exe: &Path,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<PathBuf, UpdateError> {
    check_url(&release.url)?;
    let expected = parse_sha256(&release.sha256)?;
    let dir = exe.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut tmp = TempFile::new(dir, exe);
    // Declared after `tmp`, so it is closed before the guard deletes the file;
    // Windows cannot delete or rename open files.
    let mut file = fs::File::create(&tmp.path).map_err(|e| not_writable(dir, e))?;
    let mut response = agent(&release.url, None).get(&release.url).call().map_err(net_error)?;
    let total = response.body().content_length();
    let actual = copy_hashed(&mut response.body_mut().as_reader(), &mut file, total, progress)?;
    file.sync_all()?;
    drop(file);
    ensure(actual == expected, || UpdateError::Checksum { expected: expected.clone(), actual: actual.clone() })?;
    replace(&mut tmp, exe, dir)?;
    Ok(exe.to_path_buf())
}

/// Normalised lowercase hex digest, or a manifest error for anything but 64 hex digits.
pub(crate) fn parse_sha256(text: &str) -> Result<String, UpdateError> {
    let digest = text.trim();
    ensure(digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()), || {
        UpdateError::Manifest(format!("\"{text}\" is not a SHA-256 checksum"))
    })?;
    Ok(digest.to_ascii_lowercase())
}

/// Streams `reader` into `file`, reporting progress; returns the lowercase hex SHA-256.
fn copy_hashed(
    reader: &mut dyn Read,
    file: &mut fs::File,
    total: Option<u64>,
    progress: &dyn Fn(u64, Option<u64>) -> bool,
) -> Result<String, UpdateError> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    ensure(progress(done, total), || UpdateError::Cancelled)?;
    let mut n = read_chunk(reader, &mut buf)?;
    while n > 0 {
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        ensure(progress(done, total), || UpdateError::Cancelled)?;
        n = read_chunk(reader, &mut buf)?;
    }
    Ok(to_hex(&hasher.finalize()))
}

fn read_chunk(reader: &mut dyn Read, buf: &mut [u8]) -> Result<usize, UpdateError> {
    let mut result = reader.read(buf);
    while matches!(&result, Err(e) if e.kind() == io::ErrorKind::Interrupted) {
        result = reader.read(buf);
    }
    result.map_err(|e| UpdateError::Network(e.to_string()))
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0f)]));
        s
    })
}

fn not_writable(dir: &Path, source: io::Error) -> UpdateError {
    UpdateError::NotWritable { dir: dir.to_path_buf(), source }
}

/// `<exe>.old`, where Windows keeps the replaced executable until the next start.
pub(crate) fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

pub(crate) fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Unix lets a running executable be renamed over; the process keeps its inode.
#[cfg(not(windows))]
fn replace(tmp: &mut TempFile, exe: &Path, dir: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp.path, fs::Permissions::from_mode(0o755))?;
    }
    fs::rename(&tmp.path, exe).map_err(|e| not_writable(dir, e))?;
    tmp.armed = false;
    Ok(())
}

/// Windows cannot overwrite a running executable but can rename it out of the way.
#[cfg(windows)]
fn replace(tmp: &mut TempFile, exe: &Path, dir: &Path) -> Result<(), UpdateError> {
    let old = old_path(exe);
    remove_if_exists(&old).map_err(|e| not_writable(dir, e))?;
    fs::rename(exe, &old).map_err(|e| not_writable(dir, e))?;
    match fs::rename(&tmp.path, exe) {
        Ok(()) => {
            tmp.armed = false;
            Ok(())
        }
        Err(e) => {
            fs::rename(&old, exe).map_err(|source| UpdateError::RestoreFailed {
                old: old.clone(),
                exe: exe.to_path_buf(),
                source,
            })?;
            Err(not_writable(dir, e))
        }
    }
}
