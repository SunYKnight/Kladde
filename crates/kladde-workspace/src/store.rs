use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ensure, WsError};
use crate::fsops::write_atomic;
use crate::model::{StoredWorkspace, Workspace};

/// Format version written to the workspace file; bump when the layout changes incompatibly.
const FILE_VERSION: u32 = 1;
const STORE_FILE: &str = "workspaces.json";
/// Name of the workspace created when none exists.
pub const DEFAULT_WORKSPACE: &str = "Default";

/// On-disk layout; field order here is the field order in the file.
#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    active: usize,
    workspaces: Vec<StoredWorkspace>,
}

/// Every workspace of the app, kept in one fixed file in the data directory. Never empty.
#[derive(Clone, Debug)]
pub struct WorkspaceStore {
    pub workspaces: Vec<Workspace>,
    pub active: usize,
}

impl Default for WorkspaceStore {
    fn default() -> WorkspaceStore {
        WorkspaceStore { workspaces: vec![Workspace::new(DEFAULT_WORKSPACE)], active: 0 }
    }
}

/// Location of the workspace file in `data_dir`.
pub fn store_file(data_dir: &Path) -> PathBuf {
    data_dir.join(STORE_FILE)
}

impl WorkspaceStore {
    /// Reads `path`; a missing file yields one empty default workspace. With `base`, relative paths in the file
    /// resolve against it (portable mode keeps paths relative to the data directory so drive letters may change).
    pub fn load(path: &Path, base: Option<&Path>) -> Result<WorkspaceStore, WsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let file: StoreFile = serde_json::from_str(&text)?;
                ensure(file.version <= FILE_VERSION, &format!("unsupported workspace file version {}", file.version))?;
                ensure(!file.workspaces.is_empty(), "the workspace file lists no workspace")?;
                let workspaces = file
                    .workspaces
                    .into_iter()
                    .map(|w| Workspace::from_stored(w, base))
                    .collect::<Result<Vec<_>, _>>()?;
                let active = file.active.min(workspaces.len() - 1);
                Ok(WorkspaceStore { workspaces, active })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(WorkspaceStore::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic write of all workspaces; marks them clean. With `base`, paths are stored relative to it.
    pub fn save(&mut self, path: &Path, base: Option<&Path>) -> Result<(), WsError> {
        let file = StoreFile {
            version: FILE_VERSION,
            active: self.active,
            workspaces: self.workspaces.iter().map(|w| w.to_stored(base)).collect(),
        };
        let mut text = serde_json::to_string_pretty(&file)?;
        text.push('\n');
        write_atomic(path, text.as_bytes())?;
        self.workspaces.iter_mut().for_each(Workspace::mark_clean);
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.workspaces.iter().any(Workspace::is_dirty)
    }

    pub fn current(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    pub fn current_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    /// `name` if no workspace uses it (case-insensitive), else `name 2`, `name 3` …
    pub fn free_name(&self, name: &str) -> String {
        let taken = |n: &str| self.workspaces.iter().any(|w| w.name.eq_ignore_ascii_case(n));
        let base = name.trim();
        let base = if base.is_empty() { "Workspace" } else { base };
        (1..)
            .map(|i| if i == 1 { base.to_string() } else { format!("{base} {i}") })
            .find(|n| !taken(n))
            .expect("unbounded range always yields a free name")
    }

    /// Appends a new empty workspace and returns its index.
    pub fn add(&mut self, name: &str) -> usize {
        let mut ws = Workspace::new(&self.free_name(name));
        ws.mark_dirty();
        self.workspaces.push(ws);
        self.workspaces.len() - 1
    }

    /// Removes workspace `i` unless it is the last one; keeps `active` on the same workspace where possible.
    pub fn remove(&mut self, i: usize) -> Result<Workspace, WsError> {
        ensure(i < self.workspaces.len(), &format!("no workspace {i}"))?;
        ensure(self.workspaces.len() > 1, "the last workspace cannot be deleted")?;
        let removed = self.workspaces.remove(i);
        if self.active > i || self.active == self.workspaces.len() {
            self.active -= 1;
        }
        // The file must be rewritten without the removed workspace.
        self.workspaces[self.active].mark_dirty();
        Ok(removed)
    }

    /// Renames workspace `i`; `Invalid` for an empty name or one another workspace uses.
    pub fn rename(&mut self, i: usize, name: &str) -> Result<(), WsError> {
        let name = name.trim();
        ensure(!name.is_empty(), "a workspace needs a name")?;
        let clash = self.workspaces.iter().enumerate().any(|(j, w)| j != i && w.name.eq_ignore_ascii_case(name));
        ensure(!clash, &format!("a workspace named {name} exists already"))?;
        let ws = self.workspaces.get_mut(i).ok_or_else(|| WsError::Invalid(format!("no workspace {i}")))?;
        ws.name = name.to_string();
        ws.mark_dirty();
        Ok(())
    }
}
