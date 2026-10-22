//! Sessions, hot exit (E1), periodic backups (X7), workspaces (W7, W8) and git status (X9).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use kladde_core::{CoreError, Document, OpenMode, OpenOptions, Progress};
use kladde_tools::GitStatus;
use kladde_workspace::{store_file, Node, NodeKind, Session, SplitMode, TabState, WorkspaceStore};

use crate::app::Bg;
use crate::docs::{apply_position, tab_position};
use crate::state::{App, DocId};

const BACKUP_DIR: &str = "backups";

/// Backup file of document `id`; `stamp` keeps names of different runs apart.
pub fn backup_path(data_dir: &Path, stamp: u128, id: DocId) -> PathBuf {
    data_dir.join(BACKUP_DIR).join(format!("{stamp}-{id}.bak"))
}

/// Writes the document bytes as they are (UTF-8 for text) via temp file and rename.
pub fn write_backup(doc: &Document, path: &Path) -> Result<(), CoreError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    doc.snapshot().write_raw(path, &Progress::default())
}

/// Replaces the whole content with hot-exit bytes as one undoable edit; identical content stays clean.
pub fn apply_backup(doc: &mut Document, bytes: &[u8]) -> Result<(), CoreError> {
    let len = doc.len();
    let same = len == bytes.len() as u64 && {
        let mut current = Vec::with_capacity(bytes.len());
        doc.read(0..len, &mut current);
        current == bytes
    };
    if same {
        Ok(())
    } else {
        doc.replace(0..len, bytes)
    }
}

/// Directories whose git status matters for the workspace tree.
fn collect_dirs(node: &Node, out: &mut Vec<PathBuf>) {
    let dir = match &node.kind {
        NodeKind::Group => None,
        NodeKind::Folder { path, .. } => Some(path.clone()),
        NodeKind::File { path } => path.parent().map(Path::to_path_buf),
    };
    if let Some(dir) = dir.filter(|d| !out.contains(d)) {
        out.push(dir);
    }
    node.children.iter().for_each(|c| collect_dirs(c, out));
}

fn git_statuses(dirs: &[PathBuf]) -> Result<HashMap<PathBuf, GitStatus>, String> {
    let mut map = HashMap::new();
    let errors: Vec<String> = dirs
        .iter()
        .filter_map(|dir| match kladde_tools::git_status(dir) {
            Ok(Some(found)) => {
                map.extend(found);
                None
            }
            Ok(None) => None,
            Err(e) => Some(format!("git status in {}: {e}", dir.display())),
        })
        .collect();
    if errors.is_empty() {
        Ok(map)
    } else {
        Err(errors.join("; "))
    }
}

impl App {
    /// Startup: load all workspaces, restore the active one's tabs, then open command-line files.
    pub(crate) fn startup(&mut self, args: Vec<PathBuf>) {
        self.load_store();
        let session = self.ws.current().session.clone();
        self.restore_session(session);
        args.iter().for_each(|p| {
            self.open_path(p, crate::docs::auto_open(), None);
        });
        self.ensure_tab();
        self.refresh_git();
    }

    pub(crate) fn ensure_tab(&mut self) {
        if self.panes.iter().all(|p| p.tabs.is_empty()) {
            self.focused_pane = 0;
            self.new_document();
        }
    }

    /// Portable mode stores workspace paths relative to the data directory so the drive letter may change.
    fn store_base(&self) -> Option<PathBuf> {
        kladde_workspace::is_portable().then(|| self.data_dir.clone())
    }

    /// Reads the workspace file; an unreadable one is moved aside so saving cannot overwrite it.
    fn load_store(&mut self) {
        let path = store_file(&self.data_dir);
        match WorkspaceStore::load(&path, self.store_base().as_deref()) {
            Ok(store) => self.ws = store,
            Err(e) => {
                let aside = path.with_extension(format!("json.unreadable-{}", self.shell.run_stamp));
                let moved = std::fs::rename(&path, &aside);
                let kept = match moved {
                    Ok(()) => format!("it was kept as {}", aside.display()),
                    Err(m) => format!("moving it aside failed too: {m}"),
                };
                self.toast_error(format!("Cannot read the workspaces {}: {e}; {kept}", path.display()));
            }
        }
    }

    /// Writes the workspace file if anything in it changed.
    pub(crate) fn save_store(&mut self) -> Result<(), String> {
        if self.ws.is_dirty() {
            let (path, base) = (store_file(&self.data_dir), self.store_base());
            self.ws
                .save(&path, base.as_deref())
                .map_err(|e| format!("Cannot save the workspaces {}: {e}", path.display()))
        } else {
            Ok(())
        }
    }

    /// Recreates tabs of `session`, including unsaved content from backups.
    pub(crate) fn restore_session(&mut self, session: Session) {
        self.split = session.split;
        self.split_ratio =
            if session.split_ratio > 0.05 && session.split_ratio < 0.95 { session.split_ratio } else { 0.5 };
        session.tabs.into_iter().for_each(|tab| self.restore_tab(tab));
        (0..2).for_each(|p| {
            let len = self.panes[p].tabs.len();
            self.panes[p].active = session.active[p].unwrap_or(0).min(len.saturating_sub(1));
        });
        self.focused_pane = if self.panes[0].tabs.is_empty() && !self.panes[1].tabs.is_empty() { 1 } else { 0 };
        if self.split == SplitMode::None && !self.panes[1].tabs.is_empty() {
            self.split = SplitMode::Vertical;
        }
    }

    fn restore_tab(&mut self, tab: TabState) {
        let pane = usize::from(tab.pane.min(1));
        let backup = tab.backup.as_ref().and_then(|b| match std::fs::read(b) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                self.toast_error(format!("Cannot read the backup {}: {e}", b.display()));
                None
            }
        });
        let position = (tab.caret, tab.scroll);
        let id = match (&tab.path, backup) {
            (Some(path), backup) => {
                let opts = OpenOptions {
                    mode: OpenMode::Text { encoding: None },
                    read_only: tab.read_only,
                    no_lock: tab.read_only,
                };
                let id = self.open_in_pane(pane, path, opts, Some(tab.view));
                if let Some(pending) = self.shell.pending.get_mut(&id) {
                    pending.positions.insert(pane, position);
                    pending.backup = pending.backup.take().or(backup);
                }
                id
            }
            (None, Some(bytes)) => self.add_untitled(pane, tab.title.clone(), bytes, tab.view),
            (None, None) => self.add_untitled(pane, tab.title.clone(), Vec::new(), tab.view),
        };
        self.untitled_counter = self.untitled_counter.max(untitled_number(&tab.title));
        if let Some(entry) = self.docs.get_mut(&id) {
            entry.tail = tab.tail;
            entry.backup = entry.backup.take().or(tab.backup);
            if entry.load.is_none() {
                if let Some(i) = self.panes[pane].tabs.iter().position(|t| t.doc == id) {
                    apply_position(&mut self.panes[pane].tabs[i], &entry.doc, position);
                }
            }
        }
    }

    /// Writes or removes the backup of one document so it matches its dirty state.
    fn sync_backup(&mut self, id: DocId) -> Result<(), String> {
        let limit = self.settings.large_file_mb << 20;
        let path = backup_path(&self.data_dir, self.shell.run_stamp, id);
        let mut stale = None;
        let result = match self.docs.get_mut(&id) {
            Some(e) if e.load.is_none() && e.doc.is_dirty() && e.doc.len() <= limit => {
                let current =
                    e.backup_revision == Some(e.doc.revision()) && e.backup.as_ref().is_some_and(|b| b.exists());
                if current {
                    Ok(())
                } else {
                    let path = e.backup.clone().unwrap_or(path);
                    write_backup(&e.doc, &path).map_err(|err| format!("Backup of {} failed: {err}", e.title))?;
                    e.backup = Some(path);
                    e.backup_revision = Some(e.doc.revision());
                    Ok(())
                }
            }
            Some(e) if e.load.is_none() && !e.doc.is_dirty() => {
                stale = e.backup.take();
                e.backup_revision = None;
                Ok(())
            }
            _ => Ok(()),
        };
        self.delete_backup(stale.as_deref());
        result
    }

    /// Current tabs as a session; backups are taken as they are.
    fn session_snapshot(&self) -> Session {
        let tabs = self
            .panes
            .iter()
            .enumerate()
            .flat_map(|(p, pane)| pane.tabs.iter().map(move |t| (p, t)))
            .filter_map(|(p, t)| {
                let e = self.docs.get(&t.doc)?;
                let (caret, scroll) = tab_position(t);
                Some(TabState {
                    path: self.doc_path(t.doc),
                    backup: e.backup.clone().filter(|_| e.doc.is_dirty()),
                    title: e.title.clone(),
                    pane: p as u8,
                    view: t.view,
                    caret,
                    scroll,
                    read_only: e.open_opts.read_only,
                    tail: e.tail,
                })
            })
            .collect();
        let active = [0, 1].map(|p| (!self.panes[p].tabs.is_empty()).then_some(self.panes[p].active));
        Session { tabs, active, split: self.split, split_ratio: self.split_ratio }
    }

    /// Session with fresh backups of every modified document up to the large-file limit.
    pub(crate) fn capture_session(&mut self) -> Result<Session, String> {
        let mut ids: Vec<DocId> = self.docs.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().try_for_each(|id| self.sync_backup(id))?;
        Ok(self.session_snapshot())
    }

    fn large_dirty_docs(&self) -> Vec<DocId> {
        let limit = self.settings.large_file_mb << 20;
        let mut ids: Vec<DocId> =
            self.docs.values().filter(|e| e.doc.is_dirty() && e.doc.len() > limit).map(|e| e.id).collect();
        ids.sort_unstable();
        ids
    }

    fn saves_running(&self) -> bool {
        self.docs.values().any(|e| e.save.is_some())
    }

    /// Drops all documents and tabs; backups stay for the stored session.
    fn drop_all_docs(&mut self) {
        self.docs.values().for_each(|e| {
            if let Some(job) = &e.load {
                job.progress.cancel();
            }
        });
        self.docs.clear();
        self.panes.iter_mut().for_each(|p| {
            p.tabs.clear();
            p.active = 0;
        });
        self.shell.pending.clear();
        self.shell.close_queue.clear();
        self.shell.disk_ignored.clear();
        self.focused_pane = 0;
    }

    /// Stores the current tabs into the active workspace and loads those of workspace `target`.
    fn switch_to(&mut self, target: usize) -> bool {
        let large = self.large_dirty_docs();
        let blocked = if self.saves_running() || self.shell.backup_jobs > 0 {
            Some("Wait until saves and backups have finished".to_string())
        } else if !large.is_empty() {
            let names: Vec<String> = large.iter().filter_map(|id| self.docs.get(id).map(|e| e.title.clone())).collect();
            Some(format!("Save or close the large modified documents first: {}", names.join(", ")))
        } else {
            None
        };
        let stored = match blocked {
            Some(why) => Err(why),
            None => self.store_tabs(),
        };
        match stored {
            Ok(()) => {
                self.drop_all_docs();
                self.ws.active = target;
                let ws = self.ws.current_mut();
                ws.set_folded(false);
                // The file records which workspace is active, so it must be written.
                ws.mark_dirty();
                let session = ws.session.clone();
                self.restore_session(session);
                self.ensure_tab();
                self.shell.git.clear();
                self.refresh_git();
                if let Err(e) = self.save_store() {
                    self.toast_error(e);
                }
                true
            }
            Err(e) => {
                self.toast_error(e);
                false
            }
        }
    }

    /// Puts the current tabs into the active workspace and writes the workspace file.
    fn store_tabs(&mut self) -> Result<(), String> {
        let session = self.capture_session()?;
        self.set_current_session(session);
        self.save_store()
    }

    fn set_current_session(&mut self, session: Session) {
        let ws = self.ws.current_mut();
        if ws.session != session {
            ws.session = session;
            ws.mark_dirty();
        }
    }

    /// Adds a workspace named `name` (made unique) and switches to it.
    pub fn new_workspace(&mut self, name: &str) {
        let i = self.ws.add(name);
        if !self.switch_to(i) {
            self.toast_info(format!(
                "Workspace {} was created; switch to it once the hint above is resolved",
                self.ws.workspaces[i].name
            ));
        }
    }

    pub fn rename_workspace(&mut self, i: usize, name: &str) {
        match self.ws.rename(i, name) {
            Ok(()) => {
                if let Err(e) = self.save_store() {
                    self.toast_error(e);
                }
            }
            Err(e) => self.toast_error(format!("Cannot rename the workspace: {e}")),
        }
    }

    /// Removes workspace `i` with its tree and tab list; files on disk stay. The last workspace cannot go.
    pub fn delete_workspace(&mut self, i: usize) {
        let len = self.ws.workspaces.len();
        let ready = match (i < len, len > 1, i == self.ws.active) {
            (false, _, _) => false,
            (true, false, _) => {
                self.toast_info("The last workspace cannot be deleted");
                false
            }
            (true, true, true) => self.switch_to(if i == 0 { 1 } else { i - 1 }),
            (true, true, false) => true,
        };
        if ready {
            match self.ws.remove(i) {
                Ok(removed) => {
                    removed.session.tabs.iter().for_each(|t| self.delete_backup(t.backup.as_deref()));
                    if let Err(e) = self.save_store() {
                        self.toast_error(e);
                    }
                    self.toast_info(format!("Workspace {} deleted", removed.name));
                }
                Err(e) => self.toast_error(format!("Cannot delete the workspace: {e}")),
            }
        }
    }

    pub fn switch_workspace(&mut self, i: usize) {
        if i < self.ws.workspaces.len() && self.ws.active != i {
            self.switch_to(i);
        }
    }

    /// Switches to workspace `i` and activates its stored tab `tab` (index into its session's tab list).
    pub fn switch_workspace_to_tab(&mut self, i: usize, tab: usize) {
        let wanted = self.ws.workspaces.get(i).and_then(|w| w.session.tabs.get(tab)).map(|t| t.pane);
        let before = self
            .ws
            .workspaces
            .get(i)
            .map_or(0, |w| w.session.tabs.iter().take(tab).filter(|t| Some(t.pane) == wanted).count());
        if self.ws.active != i && self.switch_to(i) {
            if let Some(pane) = wanted.map(|p| usize::from(p.min(1))) {
                self.focus_tab(pane, before.min(self.panes[pane].tabs.len().saturating_sub(1)));
            }
        }
    }

    /// Hot exit: back up modified documents, persist session, settings and workspaces, then close the window.
    pub fn request_quit(&mut self, ctx: &egui::Context) {
        self.shell.quit_pending = true;
        if !self.saves_running() && self.shell.backup_jobs == 0 {
            self.large_dirty_docs().into_iter().for_each(|id| {
                if !self.shell.close_queue.contains(&id) {
                    self.shell.close_queue.push(id);
                }
            });
            if self.shell.close_queue.is_empty() {
                self.finish_quit(ctx);
            }
        }
    }

    /// Continues a quit that waited for saves, backups or the close prompt.
    pub(crate) fn continue_quit(&mut self, ctx: &egui::Context) {
        let waiting = self.saves_running() || self.shell.backup_jobs > 0 || !self.shell.close_queue.is_empty();
        if self.shell.quit_pending && !waiting {
            self.request_quit(ctx);
        }
    }

    fn finish_quit(&mut self, ctx: &egui::Context) {
        let errors = self.persist_all(ctx);
        if errors.is_empty() || self.shell.quit_ignore_errors {
            self.shell.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.relaunch_after_update();
        } else {
            self.shell.quit_pending = false;
            self.shell.quit_ignore_errors = true;
            errors.into_iter().for_each(|e| self.toast_error(e));
            self.toast_info("Quit again to exit anyway; unsaved changes may be lost");
        }
    }

    /// Session, settings and workspaces; returns the errors.
    fn persist_all(&mut self, ctx: &egui::Context) -> Vec<String> {
        let mut errors: Vec<String> = Vec::new();
        if let Err(e) = self.store_tabs() {
            errors.push(e);
        }
        self.settings.window_size = ctx.input(|i| i.viewport().inner_rect).map(|r| [r.width(), r.height()]);
        if let Err(e) = self.settings.save(&self.data_dir) {
            errors.push(format!("Cannot save settings: {e}"));
        }
        errors
    }

    /// X7: backs up modified documents on worker threads every `backup_interval_secs`.
    pub(crate) fn poll_backups(&mut self) {
        let interval = self.settings.backup_interval_secs;
        if interval > 0 && self.shell.last_backup.elapsed().as_secs() >= interval {
            self.shell.last_backup = Instant::now();
            let limit = self.settings.large_file_mb << 20;
            let (stamp, data_dir) = (self.shell.run_stamp, self.data_dir.clone());
            let jobs: Vec<(DocId, u64, PathBuf, kladde_core::Snapshot)> = self
                .docs
                .values()
                .filter(|e| e.load.is_none() && e.doc.is_dirty() && e.doc.len() <= limit)
                .filter(|e| e.backup_revision != Some(e.doc.revision()))
                .map(|e| {
                    let path = e.backup.clone().unwrap_or_else(|| backup_path(&data_dir, stamp, e.id));
                    (e.id, e.doc.revision(), path, e.doc.snapshot())
                })
                .collect();
            jobs.into_iter().for_each(|(id, revision, path, snapshot)| {
                let (tx, ctx) = (self.shell.bg_tx.clone(), self.shell.ctx.clone());
                self.shell.backup_jobs += 1;
                std::thread::spawn(move || {
                    let result = path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .map_err(CoreError::from)
                        .and_then(|()| snapshot.write_raw(&path, &Progress::default()))
                        .map_err(|e| format!("Backup to {} failed: {e}", path.display()));
                    // A dropped receiver means the app is shutting down.
                    tx.send(Bg::Backup { id, revision, path, result }).ok();
                    ctx.request_repaint();
                });
            });
            // Keeps tabs and tree edits safe from a crash without waiting for quit.
            let session = self.session_snapshot();
            self.set_current_session(session);
            if let Err(e) = self.save_store() {
                self.toast_error(e);
            }
        }
    }

    pub(crate) fn backup_done(&mut self, id: DocId, revision: u64, path: PathBuf, result: Result<(), String>) {
        self.shell.backup_jobs = self.shell.backup_jobs.saturating_sub(1);
        match (result, self.docs.get_mut(&id)) {
            (Ok(()), Some(entry)) => {
                entry.backup = Some(path);
                entry.backup_revision = Some(revision);
            }
            (Ok(()), None) => self.delete_backup(Some(&path)),
            (Err(e), _) => self.toast_error(e),
        }
    }

    /// X9: git status of the active workspace's folders on a worker thread.
    pub(crate) fn refresh_git(&mut self) {
        let dirs = match self.shell.git_available {
            true => {
                let mut dirs = Vec::new();
                collect_dirs(&self.ws.current().root, &mut dirs);
                dirs
            }
            false => Vec::new(),
        };
        if !dirs.is_empty() {
            self.shell.git_generation += 1;
            let generation = self.shell.git_generation;
            let (tx, ctx) = (self.shell.bg_tx.clone(), self.shell.ctx.clone());
            std::thread::spawn(move || {
                let result = git_statuses(&dirs);
                // A dropped receiver means the app is shutting down.
                tx.send(Bg::Git { generation, result }).ok();
                ctx.request_repaint();
            });
        }
    }

    /// Opens the version history repository on a worker thread when history is enabled.
    pub(crate) fn poll_history_setting(&mut self) {
        let enabled = self.settings.history_enabled;
        if !enabled && self.history.is_some() {
            self.history = None;
        }
        if enabled && self.history.is_none() && !self.shell.history_opening && !self.shell.history_missing {
            self.shell.history_opening = true;
            let (tx, ctx, dir) = (self.shell.bg_tx.clone(), self.shell.ctx.clone(), self.data_dir.clone());
            std::thread::spawn(move || {
                let result = kladde_tools::History::open(&dir)
                    .map(|h| h.map(Arc::new))
                    .map_err(|e| format!("Cannot open the version history: {e}"));
                // A dropped receiver means the app is shutting down.
                tx.send(Bg::HistoryOpened(result)).ok();
                ctx.request_repaint();
            });
        }
    }
}

/// N of an untitled title "new N"; 0 otherwise.
fn untitled_number(title: &str) -> u32 {
    title.strip_prefix("new ").and_then(|n| n.parse().ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kladde_core::OpenOptions;

    fn content(doc: &Document) -> Vec<u8> {
        let mut out = Vec::new();
        doc.read(0..doc.len(), &mut out);
        out
    }

    #[test]
    fn untitled_backup_round_trips_through_the_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut doc = Document::new_empty();
        doc.insert(0, "unsaved ä\r\nline".as_bytes()).expect("insert");
        let backup = backup_path(dir.path(), 7, 3);
        write_backup(&doc, &backup).expect("backup");
        let mut store = WorkspaceStore::default();
        let tab = TabState { backup: Some(backup), title: "new 3".into(), ..TabState::default() };
        store.current_mut().session = Session { tabs: vec![tab], ..Session::default() };
        store.current_mut().mark_dirty();
        store.save(&store_file(dir.path()), None).expect("save workspaces");

        let loaded = WorkspaceStore::load(&store_file(dir.path()), None).expect("load workspaces");
        let loaded = &loaded.current().session;
        let bytes = std::fs::read(loaded.tabs[0].backup.as_ref().expect("backup path")).expect("read backup");
        let restored = Document::from_bytes(bytes);
        assert_eq!(content(&restored), content(&doc));
        assert!(restored.is_dirty());
        assert_eq!(untitled_number(&loaded.tabs[0].title), 3);
    }

    #[test]
    fn file_backup_restores_dirty_and_undo_returns_to_disk_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"on disk\n").expect("write file");
        let opts = OpenOptions { mode: OpenMode::Text { encoding: None }, read_only: false, no_lock: false };
        let mut doc = Document::open(&file, &opts, &Progress::default()).expect("open");
        doc.insert(doc.len(), b"edited\n").expect("edit");
        let backup = backup_path(dir.path(), 1, 1);
        write_backup(&doc, &backup).expect("backup");

        let mut reopened = Document::open(&file, &opts, &Progress::default()).expect("reopen");
        apply_backup(&mut reopened, &std::fs::read(&backup).expect("read backup")).expect("apply");
        assert_eq!(content(&reopened), b"on disk\nedited\n");
        assert_eq!(reopened.path(), Some(file.as_path()));
        assert!(reopened.is_dirty());
        reopened.undo();
        assert_eq!(content(&reopened), b"on disk\n");
        assert!(!reopened.is_dirty());
    }

    #[test]
    fn identical_backup_keeps_the_document_clean() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("b.txt");
        std::fs::write(&file, b"same").expect("write file");
        let opts = OpenOptions { mode: OpenMode::Text { encoding: None }, read_only: false, no_lock: false };
        let mut doc = Document::open(&file, &opts, &Progress::default()).expect("open");
        apply_backup(&mut doc, b"same").expect("apply");
        assert!(!doc.is_dirty());
    }
}
