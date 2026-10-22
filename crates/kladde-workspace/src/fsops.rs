use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::{ensure, WsError};

/// One entry of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Writes via a temp file in the target dir so readers never see a half-written file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), WsError> {
    let dir = path.parent().ok_or_else(|| WsError::Invalid(format!("no parent directory: {}", path.display())))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    {
        let mut file = fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            fs::remove_file(&tmp)?;
            Err(WsError::Io(e))
        }
    }
}

/// Compiled include/exclude globs, matched against entry names.
pub(crate) struct Filter {
    include: Option<GlobSet>,
    exclude: GlobSet,
}

fn build_set(patterns: &[String]) -> Result<GlobSet, WsError> {
    let builder = patterns.iter().try_fold(GlobSetBuilder::new(), |mut b, p| {
        let glob = GlobBuilder::new(p)
            .case_insensitive(cfg!(windows))
            .literal_separator(true)
            .build()
            .map_err(|e| WsError::Invalid(format!("glob '{p}': {e}")))?;
        b.add(glob);
        Ok::<_, WsError>(b)
    })?;
    builder.build().map_err(|e| WsError::Invalid(e.to_string()))
}

impl Filter {
    pub(crate) fn new(include: &[String], exclude: &[String]) -> Result<Filter, WsError> {
        let include = if include.is_empty() { None } else { Some(build_set(include)?) };
        Ok(Filter { include, exclude: build_set(exclude)? })
    }

    fn accepts(&self, name: &str, is_dir: bool) -> bool {
        let included = is_dir || self.include.as_ref().map_or(true, |set| set.is_match(name));
        included && !self.exclude.is_match(name)
    }

    pub(crate) fn read(&self, dir: &Path) -> Result<Vec<DirEntry>, WsError> {
        let mut entries = fs::read_dir(dir)?.try_fold(Vec::new(), |mut acc, entry| {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            let is_dir = if file_type.is_symlink() { path.is_dir() } else { file_type.is_dir() };
            let name = entry.file_name().to_string_lossy().into_owned();
            if self.accepts(&name, is_dir) {
                acc.push(DirEntry { name, path, is_dir });
            }
            Ok::<_, WsError>(acc)
        })?;
        entries.sort_by(|a, b| {
            let by_name = a.name.to_lowercase().cmp(&b.name.to_lowercase());
            b.is_dir.cmp(&a.is_dir).then(by_name).then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    /// Recursive file scan; unreadable directories are skipped because a partial file list is
    /// still useful for quick-open, and symlinked directories are not followed to avoid cycles.
    pub(crate) fn scan(&self, root: &Path, limit: usize, seen: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
        let mut stack = vec![root.to_path_buf()];
        // An extra pop after reaching the limit is harmless: the scan ends.
        while let Some(dir) = stack.pop().filter(|_| out.len() < limit) {
            if let Ok(entries) = self.read(&dir) {
                let (dirs, files): (Vec<DirEntry>, Vec<DirEntry>) = entries.into_iter().partition(|e| e.is_dir);
                stack.extend(dirs.into_iter().rev().filter(|d| !d.path.is_symlink()).map(|d| d.path));
                let room = limit - out.len();
                let fresh = files.into_iter().map(|f| f.path).filter(|p| seen.insert(p.clone()));
                out.extend(fresh.take(room));
            }
        }
    }
}

/// One directory level: dirs first, case-insensitive by name; `include` filters files, `exclude` both.
pub fn list_dir(dir: &Path, include: &[String], exclude: &[String]) -> Result<Vec<DirEntry>, WsError> {
    Filter::new(include, exclude)?.read(dir)
}

/// Renames a file or folder within its directory and returns the new path.
pub fn rename_path_on_disk(old: &Path, new_name: &str) -> Result<PathBuf, WsError> {
    ensure(!new_name.trim().is_empty(), "name must not be empty")?;
    ensure(new_name != "." && new_name != "..", "name must not be '.' or '..'")?;
    ensure(!new_name.contains(['/', '\\']), "name must not contain path separators")?;
    let parent = old.parent().ok_or_else(|| WsError::Invalid(format!("cannot rename {}", old.display())))?;
    let new = parent.join(new_name);
    let old_name = old.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Windows: a case-only rename finds the old entry under the new name.
    let case_only = cfg!(windows) && old_name.to_lowercase() == new_name.to_lowercase();
    ensure(case_only || !new.exists(), "target already exists")?;
    fs::rename(old, &new)?;
    Ok(new)
}
