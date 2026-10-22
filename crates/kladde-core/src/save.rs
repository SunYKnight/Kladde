use std::fs::{self, File};
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::encoding::Reencoder;
use crate::source::unique_suffix;
use crate::text::Text;
use crate::{CoreError, Progress, TextEncoding};

const IO_CHUNK: u64 = 4 << 20;

/// Immutable view of a document at one revision; cheap to clone, usable from any thread.
#[derive(Clone)]
pub struct Snapshot {
    pub(crate) text: Text,
}

impl Snapshot {
    pub fn len(&self) -> u64 {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends the clamped `range` to `out`.
    pub fn read(&self, range: Range<u64>, out: &mut Vec<u8>) {
        self.text.read(range, out);
    }

    pub fn read_string_lossy(&self, range: Range<u64>) -> String {
        let mut bytes = Vec::new();
        self.read(range, &mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Writes the bytes unchanged to `dest` via a temp file and rename.
    pub fn write_raw(&self, dest: &Path, progress: &Progress) -> Result<(), CoreError> {
        let (temp, _) = write_temp(&self.text, dest, None, progress)?;
        commit(&temp, dest)
    }
}

/// Background part of a save: streams a snapshot into a temp file next to the target.
pub struct SaveJob {
    snapshot: Snapshot,
    target: PathBuf,
    encoding: Option<TextEncoding>,
    revision: u64,
}

impl SaveJob {
    pub(crate) fn new(snapshot: Snapshot, target: PathBuf, encoding: Option<TextEncoding>, revision: u64) -> SaveJob {
        SaveJob { snapshot, target, encoding, revision }
    }

    /// Temp file in the target directory, streamed in chunks, re-encoded (encoding + BOM).
    pub fn run(self, progress: &Progress) -> Result<PreparedSave, CoreError> {
        let (temp, unmappable) = write_temp(&self.snapshot.text, &self.target, self.encoding, progress)?;
        progress.finish();
        Ok(PreparedSave { temp, target: self.target, encoding: self.encoding, revision: self.revision, unmappable })
    }
}

/// A fully written temp file waiting to replace its target.
pub struct PreparedSave {
    temp: PathBuf,
    target: PathBuf,
    encoding: Option<TextEncoding>,
    revision: u64,
    unmappable: u64,
}

impl PreparedSave {
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Characters the encoding could not represent; they were written as '?'.
    pub fn unmappable(&self) -> u64 {
        self.unmappable
    }

    /// Deletes the temp file without saving.
    pub fn discard(self) -> Result<(), CoreError> {
        fs::remove_file(&self.temp)?;
        Ok(())
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn encoding(&self) -> Option<TextEncoding> {
        self.encoding
    }

    pub(crate) fn commit(self) -> Result<(), CoreError> {
        commit(&self.temp, &self.target)
    }

    /// Deletes the temp file and returns `err`, extended if the deletion failed too.
    pub(crate) fn discard_with(self, err: CoreError) -> CoreError {
        cleanup(&self.temp, err)
    }
}

fn commit(temp: &Path, target: &Path) -> Result<(), CoreError> {
    fs::rename(temp, target).map_err(|e| cleanup(temp, e.into()))
}

fn cleanup(temp: &Path, err: CoreError) -> CoreError {
    match fs::remove_file(temp) {
        Ok(()) => err,
        Err(e) => CoreError::Io(io::Error::new(e.kind(), format!("{err}; temp file {} left: {e}", temp.display()))),
    }
}

/// Writes `text` to a fresh temp file beside `target`; returns its path and the unmappable count.
fn write_temp(
    text: &Text,
    target: &Path,
    encoding: Option<TextEncoding>,
    progress: &Progress,
) -> Result<(PathBuf, u64), CoreError> {
    let (temp, file) = create_beside(target)?;
    match write_stream(file, text, encoding, progress) {
        Ok(unmappable) => Ok((temp, unmappable)),
        Err(e) => Err(cleanup(&temp, e)),
    }
}

fn create_beside(target: &Path) -> Result<(PathBuf, File), CoreError> {
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("not a file path: {}", target.display())))?;
    let temp = dir.join(format!(".{}.{}.kladde-tmp", name.to_string_lossy(), unique_suffix()));
    let file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
    #[cfg(unix)]
    match fs::metadata(target) {
        Ok(meta) => fs::set_permissions(&temp, meta.permissions()).map_err(|e| cleanup(&temp, e.into()))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => Err(cleanup(&temp, e.into()))?,
    }
    Ok((temp, file))
}

fn write_stream(
    mut file: File,
    text: &Text,
    encoding: Option<TextEncoding>,
    progress: &Progress,
) -> Result<u64, CoreError> {
    let len = text.len();
    progress.set_total(len);
    let mut encoder = Reencoder::new(encoding);
    let mut chunk = Vec::new();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < len {
        if progress.is_cancelled() {
            Err(CoreError::Cancelled)?;
        }
        let end = len.min(pos + IO_CHUNK);
        chunk.clear();
        text.read_checked(pos..end, &mut chunk)?;
        encoder.encode(&chunk, false, &mut out);
        file.write_all(&out)?;
        pos = end;
        progress.set_done(pos);
    }
    encoder.encode(&[], true, &mut out);
    file.write_all(&out)?;
    file.sync_all()?;
    Ok(encoder.unmappable())
}
