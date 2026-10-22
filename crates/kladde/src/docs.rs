//! Documents and tabs: opening and saving on worker threads, closing, disk watching and tail mode.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

use kladde_core::{looks_binary, CoreError, DiskState, Document, OpenMode, OpenOptions, Progress};
use kladde_editor::{EditorConfig, EditorState, Selection};
use kladde_hex::{HexConfig, HexState};
use kladde_workspace::ViewKind;

use crate::state::{App, DiskNotice, DocEntry, DocId, LoadJob, SaveTask, TabView};

/// Bytes inspected for binary detection (H3).
const SNIFF_BYTES: u64 = 8 << 10;
/// Interval of the external-change check (E8).
const DISK_CHECK_EVERY: Duration = Duration::from_secs(2);
/// Saves restarted after `Busy` before giving up.
const SAVE_ATTEMPTS: u32 = 5;

/// Work to apply once a worker thread delivered a document.
pub(crate) struct PendingLoad {
    pub path: PathBuf,
    /// Switch tabs to the hex view when the file turns out to be binary.
    pub auto_view: bool,
    /// Hot-exit content that replaces the file content (dirty, undoable).
    pub backup: Option<Vec<u8>>,
    /// Caret and scroll offset per pane.
    pub positions: HashMap<usize, (u64, u64)>,
    pub select: Option<Range<u64>>,
}

/// Size and modification time of a file whose change the user chose to ignore; None = deleted.
pub(crate) type DiskStamp = Option<(u64, SystemTime)>;

/// Open options that auto-detect binary files and the text encoding.
pub fn auto_open() -> OpenOptions {
    OpenOptions { mode: OpenMode::Text { encoding: None }, read_only: false, no_lock: false }
}

pub fn file_title(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// Windows paths compare case-insensitively.
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

pub(crate) fn disk_stamp(path: &Path) -> DiskStamp {
    // Unreadable metadata counts as gone, matching the Deleted notice shown for it.
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok().map(|t| (m.len(), t)))
}

fn is_binary_mode(opts: &OpenOptions) -> bool {
    matches!(opts.mode, OpenMode::Binary)
}

/// Resolves auto-detection: binary files open in binary mode.
fn sniff_mode(path: &Path, opts: OpenOptions) -> Result<OpenOptions, CoreError> {
    match opts.mode {
        OpenMode::Text { encoding: None } => {
            let mut sample = Vec::with_capacity(SNIFF_BYTES as usize);
            std::fs::File::open(path)?.take(SNIFF_BYTES).read_to_end(&mut sample)?;
            let mode = if looks_binary(&sample) { OpenMode::Binary } else { opts.mode };
            Ok(OpenOptions { mode, ..opts })
        }
        _ => Ok(opts),
    }
}

fn spawn_load(ctx: &egui::Context, path: PathBuf, opts: OpenOptions) -> LoadJob {
    let progress = Progress::new();
    let (tx, rx) = mpsc::channel();
    let worker_progress = Arc::clone(&progress);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = sniff_mode(&path, opts).and_then(|opts| Document::open(&path, &opts, &worker_progress));
        // A dropped receiver means the tab was closed; the result is moot.
        tx.send(result).ok();
        ctx.request_repaint();
    });
    LoadJob { progress, rx }
}

/// Caret and scroll offset of a tab in its current view.
pub(crate) fn tab_position(tab: &TabView) -> (u64, u64) {
    match tab.view {
        ViewKind::Text => (tab.editor.caret(), tab.editor.scroll_top()),
        ViewKind::Hex => (tab.hex.cursor(), tab.hex.top_offset()),
    }
}

pub(crate) fn apply_position(tab: &mut TabView, doc: &Document, (caret, scroll): (u64, u64)) {
    match tab.view {
        ViewKind::Text => {
            tab.editor.set_scroll_top(doc, scroll);
            tab.editor.set_selections(doc, vec![Selection::caret(caret)]);
        }
        ViewKind::Hex => {
            tab.hex.goto(doc, caret);
            tab.hex.set_top_offset(scroll.min(doc.len()));
        }
    }
}

fn select_in(tab: &mut TabView, doc: &Document, range: Range<u64>) {
    match tab.view {
        ViewKind::Text => tab.editor.select(doc, range),
        ViewKind::Hex => tab.hex.select(doc, range),
    }
}

impl App {
    pub fn active_tab(&self) -> Option<(usize, usize)> {
        let pane = &self.panes[self.focused_pane];
        (pane.active < pane.tabs.len()).then_some((self.focused_pane, pane.active))
    }

    /// The focused tab and its document; None while the document is still loading.
    pub fn active_parts(&mut self) -> Option<(&mut DocEntry, &mut TabView)> {
        let (p, i) = self.active_tab()?;
        let tab = &mut self.panes[p].tabs[i];
        let entry = self.docs.get_mut(&tab.doc).filter(|e| e.load.is_none())?;
        Some((entry, tab))
    }

    pub fn editor_config(&self) -> EditorConfig {
        let s = &self.settings;
        EditorConfig {
            font_size: s.font_size,
            tab_width: s.tab_width,
            insert_spaces: s.insert_spaces,
            show_whitespace: s.show_whitespace,
            word_wrap: s.word_wrap,
            show_line_numbers: s.show_line_numbers,
            large_file_bytes: s.large_file_mb << 20,
        }
    }

    pub fn hex_config(&self) -> HexConfig {
        HexConfig {
            bytes_per_row: self.settings.hex_bytes_per_row,
            group: self.settings.hex_group,
            mode: self.shell.hex_mode,
            text: self.shell.hex_text,
            font_size: self.settings.font_size,
        }
    }

    /// File path of a document, also while it is still loading.
    pub fn doc_path(&self, id: DocId) -> Option<PathBuf> {
        self.shell
            .pending
            .get(&id)
            .map(|p| p.path.clone())
            .or_else(|| self.docs.get(&id).and_then(|e| e.doc.path().map(Path::to_path_buf)))
    }

    pub fn open_files_set(&self) -> HashSet<PathBuf> {
        self.docs.keys().filter_map(|id| self.doc_path(*id)).collect()
    }

    pub(crate) fn dirty_files_set(&self) -> HashSet<PathBuf> {
        self.docs.values().filter(|e| e.doc.is_dirty()).filter_map(|e| e.doc.path().map(Path::to_path_buf)).collect()
    }

    fn insert_doc(&mut self, doc: Document, title: String, open_opts: OpenOptions) -> DocId {
        self.next_doc_id += 1;
        let id = self.next_doc_id;
        let entry = DocEntry {
            id,
            doc,
            title,
            open_opts,
            load: None,
            save: None,
            tail: false,
            disk_notice: None,
            backup: None,
            backup_revision: None,
        };
        self.docs.insert(id, entry);
        id
    }

    /// Adds a tab for `id` to `pane` and focuses it.
    pub(crate) fn add_tab(&mut self, pane: usize, id: DocId, view: ViewKind) -> usize {
        let path = self.doc_path(id);
        let tab =
            TabView { doc: id, view, editor: EditorState::new(path.as_deref()), hex: HexState::new(), csv_mode: false };
        self.panes[pane].tabs.push(tab);
        let idx = self.panes[pane].tabs.len() - 1;
        self.focus_tab(pane, idx);
        idx
    }

    pub(crate) fn focus_tab(&mut self, pane: usize, idx: usize) {
        self.panes[pane].active = idx;
        self.focused_pane = pane;
        self.shell.focus_view = true;
    }

    pub(crate) fn tab_index(&self, pane: usize, id: DocId) -> Option<usize> {
        self.panes[pane].tabs.iter().position(|t| t.doc == id)
    }

    pub(crate) fn next_untitled_title(&mut self) -> String {
        self.untitled_counter += 1;
        format!("new {}", self.untitled_counter)
    }

    pub fn new_document(&mut self) -> DocId {
        let title = self.next_untitled_title();
        let id = self.insert_doc(Document::new_empty(), title, auto_open());
        self.add_tab(self.focused_pane, id, ViewKind::Text);
        id
    }

    /// Untitled document with restored content (hot exit) in `pane`.
    pub(crate) fn add_untitled(&mut self, pane: usize, title: String, bytes: Vec<u8>, view: ViewKind) -> DocId {
        let id = self.insert_doc(Document::from_bytes(bytes), title, auto_open());
        self.add_tab(pane, id, view);
        id
    }

    pub fn open_path(&mut self, path: &Path, opts: OpenOptions, view: Option<ViewKind>) -> DocId {
        self.open_in_pane(self.focused_pane, path, opts, view)
    }

    /// Open document matching `path` and the requested mode (auto-detect matches any mode).
    fn find_open(&self, path: &Path, opts: &OpenOptions) -> Option<DocId> {
        let auto = matches!(opts.mode, OpenMode::Text { encoding: None });
        let mut ids: Vec<DocId> = self.docs.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().find(|id| {
            let entry = &self.docs[id];
            let same_mode = auto || is_binary_mode(&entry.open_opts) == is_binary_mode(opts);
            same_mode && self.doc_path(*id).is_some_and(|p| same_path(&p, path))
        })
    }

    pub(crate) fn open_in_pane(
        &mut self,
        pane: usize,
        path: &Path,
        opts: OpenOptions,
        view: Option<ViewKind>,
    ) -> DocId {
        // An unresolvable path is opened as given; the loader then reports the error.
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let id = match self.find_open(&path, &opts) {
            Some(id) => {
                let idx = match self.tab_index(pane, id) {
                    Some(idx) => idx,
                    None => {
                        let shown = self.panes.iter().flat_map(|p| p.tabs.iter()).find(|t| t.doc == id);
                        let view = view.or(shown.map(|t| t.view)).unwrap_or(ViewKind::Text);
                        self.add_tab(pane, id, view)
                    }
                };
                if let Some(v) = view {
                    self.panes[pane].tabs[idx].view = v;
                }
                self.focus_tab(pane, idx);
                id
            }
            None => {
                let binary = is_binary_mode(&opts);
                let load = spawn_load(&self.shell.ctx, path.clone(), opts.clone());
                let id = self.insert_doc(Document::new_empty(), file_title(&path), opts);
                if let Some(entry) = self.docs.get_mut(&id) {
                    entry.load = Some(load);
                }
                let pending = PendingLoad {
                    path: path.clone(),
                    auto_view: view.is_none(),
                    backup: None,
                    positions: HashMap::new(),
                    select: None,
                };
                self.shell.pending.insert(id, pending);
                let initial = view.unwrap_or(if binary { ViewKind::Hex } else { ViewKind::Text });
                self.add_tab(pane, id, initial);
                self.settings.push_recent_file(&path);
                id
            }
        };
        id
    }

    pub fn reopen_doc(&mut self, id: DocId, opts: OpenOptions) {
        match (self.doc_path(id), self.docs.contains_key(&id)) {
            (Some(path), true) => {
                let positions: HashMap<usize, (u64, u64)> = (0..2)
                    .filter_map(|p| self.tab_index(p, id).map(|i| (p, tab_position(&self.panes[p].tabs[i]))))
                    .collect();
                let load = spawn_load(&self.shell.ctx, path.clone(), opts.clone());
                if let Some(entry) = self.docs.get_mut(&id) {
                    if let Some(old) = entry.load.replace(load) {
                        old.progress.cancel();
                    }
                    entry.open_opts = opts;
                    entry.disk_notice = None;
                }
                self.shell.disk_ignored.remove(&id);
                let pending = PendingLoad { path, auto_view: false, backup: None, positions, select: None };
                self.shell.pending.insert(id, pending);
            }
            (None, true) => self.toast_info("The document has not been saved to a file yet"),
            _ => {}
        }
    }

    /// Re-reads a document from disk, discarding unsaved changes.
    pub fn reload_doc(&mut self, id: DocId) {
        if let Some(opts) = self.docs.get(&id).map(|e| e.open_opts.clone()) {
            self.reopen_doc(id, opts);
        }
    }

    pub fn open_at(&mut self, path: Option<&Path>, offset: u64, len: u64) {
        let id = match path {
            Some(p) => Some(self.open_path(p, auto_open(), None)),
            None => self.active_tab().map(|(p, i)| self.panes[p].tabs[i].doc),
        };
        let range = offset..offset.saturating_add(len);
        match (id, id.and_then(|id| self.shell.pending.get_mut(&id))) {
            (_, Some(pending)) => pending.select = Some(range),
            (Some(id), None) => {
                let pane = self.focused_pane;
                if let (Some(i), Some(entry)) = (self.tab_index(pane, id), self.docs.get(&id)) {
                    select_in(&mut self.panes[pane].tabs[i], &entry.doc, range);
                    self.panes[pane].active = i;
                }
            }
            (None, None) => {}
        }
    }

    /// Integrates documents delivered by loader threads.
    pub(crate) fn poll_loads(&mut self) {
        let finished: Vec<(DocId, Result<Document, CoreError>)> = self
            .docs
            .iter()
            .filter_map(|(id, e)| {
                e.load.as_ref().and_then(|job| match job.rx.try_recv() {
                    Ok(result) => Some((*id, result)),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some((*id, Err(CoreError::Io(std::io::Error::other("the loader stopped without a result")))))
                    }
                })
            })
            .collect();
        finished.into_iter().for_each(|(id, result)| self.finish_load(id, result));
    }

    fn finish_load(&mut self, id: DocId, result: Result<Document, CoreError>) {
        let pending = self.shell.pending.remove(&id);
        if let Some(entry) = self.docs.get_mut(&id) {
            entry.load = None;
        }
        match (result, pending) {
            (Ok(doc), Some(pending)) => self.install_loaded(id, doc, pending),
            (Err(CoreError::Cancelled), _) => self.remove_doc(id),
            (Err(e), Some(PendingLoad { path, backup: Some(bytes), .. })) => {
                // Keep unsaved hot-exit content even though its file is gone.
                self.toast_error(format!("Cannot open {}: {e}; restored the unsaved copy as untitled", path.display()));
                if let Some(entry) = self.docs.get_mut(&id) {
                    entry.doc = Document::from_bytes(bytes);
                    entry.open_opts = auto_open();
                }
                self.reset_tab_states(id, &HashMap::new(), None);
            }
            (Err(e), pending) => {
                let what = pending.map_or_else(|| "file".to_string(), |p| p.path.display().to_string());
                self.toast_error(format!("Cannot open {what}: {e}"));
                self.remove_doc(id);
            }
            (Ok(_), None) => self.remove_doc(id),
        }
    }

    fn install_loaded(&mut self, id: DocId, mut doc: Document, pending: PendingLoad) {
        let restored = pending.backup.as_deref().map(|bytes| crate::session::apply_backup(&mut doc, bytes));
        if let Some(Err(e)) = restored {
            self.toast_error(format!("Restoring unsaved changes of {} failed: {e}", pending.path.display()));
        }
        let binary = doc.is_binary();
        if let Some(entry) = self.docs.get_mut(&id) {
            if binary {
                entry.open_opts.mode = OpenMode::Binary;
            }
            entry.title = file_title(&pending.path);
            entry.doc = doc;
        }
        if pending.auto_view && binary {
            self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).filter(|t| t.doc == id).for_each(|t| {
                t.view = ViewKind::Hex;
            });
        }
        self.reset_tab_states(id, &pending.positions, pending.select);
    }

    /// Fresh view states for a replaced document, keeping the chosen language and the requested positions.
    fn reset_tab_states(&mut self, id: DocId, positions: &HashMap<usize, (u64, u64)>, select: Option<Range<u64>>) {
        let path = self.doc_path(id);
        if let Some(entry) = self.docs.get(&id) {
            self.panes.iter_mut().enumerate().for_each(|(p, pane)| {
                let active = pane.active;
                pane.tabs.iter_mut().enumerate().filter(|(_, t)| t.doc == id).for_each(|(i, tab)| {
                    let lang = tab.editor.language();
                    tab.editor = EditorState::new(path.as_deref());
                    if tab.editor.language().id != lang.id {
                        tab.editor.set_language(lang);
                    }
                    tab.hex = HexState::new();
                    if let Some(pos) = positions.get(&p) {
                        apply_position(tab, &entry.doc, *pos);
                    }
                    if let Some(range) = select.clone().filter(|_| i == active) {
                        select_in(tab, &entry.doc, range);
                    }
                });
            });
        }
    }

    /// Drops a document and all its tabs without asking; deletes its backup file.
    pub(crate) fn remove_doc(&mut self, id: DocId) {
        self.panes.iter_mut().for_each(|pane| {
            let active_doc = pane.tabs.get(pane.active).map(|t| t.doc);
            pane.tabs.retain(|t| t.doc != id);
            pane.active = active_doc
                .and_then(|d| pane.tabs.iter().position(|t| t.doc == d))
                .unwrap_or(pane.active.min(pane.tabs.len().saturating_sub(1)));
        });
        self.shell.pending.remove(&id);
        self.shell.disk_ignored.remove(&id);
        self.shell.close_queue.retain(|d| *d != id);
        if let Some(entry) = self.docs.remove(&id) {
            if let Some(job) = &entry.load {
                job.progress.cancel();
            }
            if let Some(task) = &entry.save {
                task.progress.cancel();
            }
            self.delete_backup(entry.backup.as_deref());
        }
    }

    pub(crate) fn delete_backup(&mut self, backup: Option<&Path>) {
        if let Some(Err(e)) = backup.filter(|p| p.exists()).map(std::fs::remove_file) {
            self.toast_error(format!(
                "Cannot delete backup {}: {e}",
                backup.map_or(String::new(), |p| p.display().to_string())
            ));
        }
    }

    /// Asks for a target path when needed; None if the dialog was cancelled.
    fn save_target(&self, id: DocId, save_as: bool) -> Option<PathBuf> {
        let entry = self.docs.get(&id)?;
        match entry.doc.path().filter(|_| !save_as) {
            Some(path) => Some(path.to_path_buf()),
            None => {
                let mut dialog = rfd::FileDialog::new().set_title("Save as").set_file_name(entry.title.as_str());
                if let Some(dir) = entry.doc.path().and_then(Path::parent) {
                    dialog = dialog.set_directory(dir);
                }
                dialog.save_file()
            }
        }
    }

    pub fn save_doc(&mut self, id: DocId, save_as: bool) {
        self.save_doc_then(id, save_as, false);
    }

    /// Starts a save; `close_after` closes the document once it succeeded. False if nothing was started.
    pub(crate) fn save_doc_then(&mut self, id: DocId, save_as: bool, close_after: bool) -> bool {
        let state = self.docs.get(&id).map(|e| (e.load.is_some(), e.save.is_some()));
        match state {
            Some((true, _)) => {
                self.toast_info("The document is still loading");
                false
            }
            Some((_, true)) => {
                self.toast_info("The document is already being saved");
                false
            }
            Some((false, false)) => match self.save_target(id, save_as) {
                Some(target) => {
                    self.shell.save_attempts.remove(&id);
                    self.start_save(id, target, close_after);
                    true
                }
                None => false,
            },
            None => false,
        }
    }

    fn start_save(&mut self, id: DocId, target: PathBuf, close_after: bool) {
        let ctx = self.shell.ctx.clone();
        if let Some(entry) = self.docs.get_mut(&id) {
            let job = entry.doc.prepare_save(&target);
            let progress = Progress::new();
            let worker_progress = Arc::clone(&progress);
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let result = job.run(&worker_progress);
                // A dropped receiver means the document was closed; the temp file is left for the OS.
                tx.send(result).ok();
                ctx.request_repaint();
            });
            entry.save = Some(SaveTask { progress, rx, target, close_after });
        }
    }

    pub fn save_all(&mut self) {
        let mut ids: Vec<DocId> =
            self.docs.values().filter(|e| e.doc.is_dirty() && e.save.is_none()).map(|e| e.id).collect();
        ids.sort_unstable();
        ids.into_iter().for_each(|id| self.save_doc(id, false));
    }

    pub(crate) fn poll_saves(&mut self) {
        let finished: Vec<(DocId, Result<kladde_core::PreparedSave, CoreError>)> = self
            .docs
            .iter()
            .filter_map(|(id, e)| {
                e.save.as_ref().and_then(|task| match task.rx.try_recv() {
                    Ok(result) => Some((*id, result)),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some((*id, Err(CoreError::Io(std::io::Error::other("the save stopped without a result")))))
                    }
                })
            })
            .collect();
        finished.into_iter().for_each(|(id, result)| self.finish_save_task(id, result));
    }

    fn finish_save_task(&mut self, id: DocId, result: Result<kladde_core::PreparedSave, CoreError>) {
        let task = self.docs.get_mut(&id).and_then(|e| e.save.take());
        if let Some(task) = task {
            let unmappable = result.as_ref().map_or(0, |p| p.unmappable());
            let finished = match (result, self.docs.get_mut(&id)) {
                (Ok(prepared), Some(entry)) => entry.doc.finish_save(prepared),
                (Ok(prepared), None) => prepared.discard(),
                (Err(e), _) => Err(e),
            };
            match finished {
                Ok(()) => {
                    if unmappable > 0 {
                        self.toast_info(format!("{unmappable} characters could not be encoded and were saved as '?'"));
                    }
                    self.saved(id, &task.target, task.close_after);
                }
                Err(CoreError::Busy(why)) => {
                    let attempts = self.shell.save_attempts.entry(id).or_insert(0);
                    *attempts += 1;
                    if *attempts < SAVE_ATTEMPTS {
                        self.cancel_doc_jobs(id);
                        self.start_save(id, task.target, task.close_after);
                    } else {
                        self.shell.save_attempts.remove(&id);
                        self.save_failed(&task.target, &format!("busy: {why}"));
                    }
                }
                Err(CoreError::Cancelled) => {
                    self.shell.quit_pending = false;
                    self.toast_info(format!("Saving {} cancelled", task.target.display()));
                }
                Err(e) => self.save_failed(&task.target, &e.to_string()),
            }
        }
    }

    fn save_failed(&mut self, target: &Path, why: &str) {
        self.shell.quit_pending = false;
        self.toast_error(format!("Saving {} failed: {why}", target.display()));
    }

    fn saved(&mut self, id: DocId, target: &Path, close_after: bool) {
        self.shell.save_attempts.remove(&id);
        self.shell.disk_ignored.remove(&id);
        let backup = self.docs.get_mut(&id).and_then(|entry| {
            entry.title = file_title(target);
            entry.disk_notice = None;
            entry.backup_revision = None;
            entry.backup.take()
        });
        self.delete_backup(backup.as_deref());
        let lang = kladde_syntax::language_for_path(target);
        self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).filter(|t| t.doc == id).for_each(|t| {
            if t.editor.language().id == kladde_syntax::languages()[0].id {
                t.editor.set_language(lang);
            }
        });
        self.settings.push_recent_file(target);
        self.after_save(target);
        if close_after {
            self.remove_doc(id);
        }
    }

    /// Version snapshot (V1, V3) and git status refresh (X9) after a successful save.
    pub fn after_save(&mut self, path: &Path) {
        let limit = self.settings.history_max_mb << 20;
        let history = self.history.clone().filter(|_| self.settings.history_enabled);
        if let Some(history) = history {
            let path = path.to_path_buf();
            let (tx, ctx) = (self.shell.bg_tx.clone(), self.shell.ctx.clone());
            self.shell.history_jobs += 1;
            std::thread::spawn(move || {
                let result = std::fs::metadata(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|m| match m.len() <= limit {
                        true => history.snapshot(&path).map(|_| ()).map_err(|e| e.to_string()),
                        false => Ok(()),
                    })
                    .map_err(|e| format!("Version snapshot of {} failed: {e}", path.display()));
                // A dropped receiver means the app is shutting down.
                tx.send(crate::app::Bg::History(result)).ok();
                ctx.request_repaint();
            });
        }
        self.refresh_git();
    }

    /// Closes a tab; the last view of a modified document asks first.
    pub fn close_tab(&mut self, pane: usize, idx: usize) {
        if let Some(id) = self.panes[pane].tabs.get(idx).map(|t| t.doc) {
            let views = self.panes.iter().flat_map(|p| p.tabs.iter()).filter(|t| t.doc == id).count();
            if views > 1 {
                self.panes[pane].tabs.remove(idx);
                let len = self.panes[pane].tabs.len();
                let active = self.panes[pane].active;
                self.panes[pane].active = if active > idx { active - 1 } else { active.min(len.saturating_sub(1)) };
            } else {
                self.close_doc(id);
            }
        }
    }

    /// Closes a document or queues the Save / Don't save / Cancel question.
    pub(crate) fn close_doc(&mut self, id: DocId) {
        let state = self.docs.get(&id).map(|e| (e.save.is_some(), e.doc.is_dirty() && e.load.is_none()));
        match state {
            Some((true, _)) => self.toast_info("Wait until the save has finished"),
            Some((false, true)) => {
                if !self.shell.close_queue.contains(&id) {
                    self.shell.close_queue.push(id);
                }
            }
            Some((false, false)) => self.remove_doc(id),
            None => {}
        }
    }

    pub fn close_all(&mut self) {
        let mut ids: Vec<DocId> = Vec::new();
        self.panes.iter().flat_map(|p| p.tabs.iter()).for_each(|t| {
            if !ids.contains(&t.doc) {
                ids.push(t.doc);
            }
        });
        ids.into_iter().for_each(|id| self.close_doc(id));
    }

    /// Save / Don't save / Cancel for the first queued modified document.
    pub(crate) fn show_close_prompt(&mut self, ctx: &egui::Context) {
        let stale: Vec<DocId> = self
            .shell
            .close_queue
            .iter()
            .copied()
            .filter(|id| !self.docs.get(id).is_some_and(|e| e.doc.is_dirty()))
            .collect();
        stale.into_iter().for_each(|id| {
            self.shell.close_queue.retain(|d| *d != id);
            if self.docs.contains_key(&id) {
                self.remove_doc(id);
            }
        });
        let first = self.shell.close_queue.first().copied();
        if let Some(id) = first {
            let title = self.docs.get(&id).map_or(String::new(), |e| e.title.clone());
            let mut choice = None;
            egui::Window::new("Unsaved changes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Save changes to \"{title}\"?"));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            choice = Some(CloseChoice::Save);
                        }
                        if ui.button("Don't save").clicked() {
                            choice = Some(CloseChoice::Discard);
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            choice = Some(CloseChoice::Cancel);
                        }
                    });
                });
            match choice {
                Some(CloseChoice::Save) => {
                    self.shell.close_queue.retain(|d| *d != id);
                    if !self.save_doc_then(id, false, true) {
                        self.cancel_closing();
                    }
                }
                Some(CloseChoice::Discard) => self.remove_doc(id),
                Some(CloseChoice::Cancel) => self.cancel_closing(),
                None => {}
            }
        }
    }

    fn cancel_closing(&mut self) {
        self.shell.close_queue.clear();
        self.shell.quit_pending = false;
    }

    /// Per-frame document upkeep: line index, deferred read errors, disk watch, tail mode.
    pub(crate) fn poll_documents(&mut self) {
        let errors: Vec<String> = self
            .docs
            .values_mut()
            .filter(|e| e.load.is_none())
            .filter_map(|e| {
                e.doc.poll_line_index();
                e.doc.take_error().map(|err| format!("{}: {err}", e.title))
            })
            .collect();
        errors.into_iter().for_each(|e| self.toast_error(e));
        if self.shell.last_disk_check.elapsed() >= DISK_CHECK_EVERY {
            self.shell.last_disk_check = Instant::now();
            self.check_disk();
        }
    }

    fn check_disk(&mut self) {
        let mut grown: Vec<DocId> = Vec::new();
        let ignored = &self.shell.disk_ignored;
        self.docs.values_mut().filter(|e| e.load.is_none() && e.save.is_none()).for_each(|e| {
            let notice = match e.doc.check_disk() {
                DiskState::Unchanged => None,
                DiskState::Grew(_) if e.tail && !e.doc.is_dirty() => {
                    grown.push(e.id);
                    e.disk_notice
                }
                DiskState::Grew(_) | DiskState::Modified => Some(DiskNotice::Changed),
                DiskState::Deleted => Some(DiskNotice::Deleted),
            };
            let suppressed =
                notice.is_some() && ignored.get(&e.id).is_some_and(|s| *s == e.doc.path().and_then(disk_stamp));
            e.disk_notice = if suppressed { None } else { notice };
        });
        grown.into_iter().for_each(|id| self.tail_append(id));
    }

    /// tail -f: appends what the file gained and keeps the carets at the end.
    fn tail_append(&mut self, id: DocId) {
        let appended = self.docs.get_mut(&id).map(|e| e.doc.append_from_disk());
        match appended {
            Some(Ok(n)) if n > 0 => {
                if let Some(entry) = self.docs.get(&id) {
                    let len = entry.doc.len();
                    self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).filter(|t| t.doc == id).for_each(|t| {
                        t.editor.set_caret(&entry.doc, len);
                        t.hex.goto(&entry.doc, len);
                    });
                }
            }
            Some(Err(e)) => self.toast_error(format!("Tail update failed: {e}")),
            _ => {}
        }
    }

    /// Hides the external-change notice until the file changes again.
    pub(crate) fn ignore_disk_change(&mut self, id: DocId) {
        let stamp = self.docs.get(&id).and_then(|e| e.doc.path()).and_then(disk_stamp);
        self.shell.disk_ignored.insert(id, stamp);
        if let Some(entry) = self.docs.get_mut(&id) {
            entry.disk_notice = None;
        }
    }

    /// Reopens documents at or below a path renamed on disk under their new path, keeping unsaved edits.
    pub(crate) fn docs_renamed(&mut self, old: &Path, new: &Path) {
        let mut moved: Vec<(DocId, PathBuf)> = self
            .docs
            .keys()
            .filter_map(|id| {
                let path = self.doc_path(*id)?;
                let rel = if same_path(&path, old) {
                    Some(PathBuf::new())
                } else {
                    path.strip_prefix(old).ok().map(Path::to_path_buf)
                };
                rel.map(|rel| (*id, if rel.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rel) }))
            })
            .collect();
        moved.sort_unstable_by_key(|(id, _)| *id);
        let limit = self.settings.large_file_mb << 20;
        moved.into_iter().for_each(|(id, path)| {
            let (dirty, len) = self.docs.get(&id).map_or((false, 0), |e| (e.doc.is_dirty(), e.doc.len()));
            let backup = match dirty && len <= limit {
                true => self.docs.get(&id).map(|e| {
                    let mut bytes = Vec::with_capacity(len as usize);
                    e.doc.read(0..len, &mut bytes);
                    bytes
                }),
                false => None,
            };
            if dirty && backup.is_none() {
                self.toast_error(format!("{} was renamed; its large unsaved changes were dropped", path.display()));
            }
            let positions: HashMap<usize, (u64, u64)> = (0..2)
                .filter_map(|p| self.tab_index(p, id).map(|i| (p, tab_position(&self.panes[p].tabs[i]))))
                .collect();
            let opts = self.docs.get(&id).map_or_else(auto_open, |e| e.open_opts.clone());
            let load = spawn_load(&self.shell.ctx, path.clone(), opts);
            if let Some(entry) = self.docs.get_mut(&id) {
                if let Some(old) = entry.load.replace(load) {
                    old.progress.cancel();
                }
                entry.title = file_title(&path);
                entry.disk_notice = None;
            }
            let pending = PendingLoad { path, auto_view: false, backup, positions, select: None };
            self.shell.pending.insert(id, pending);
        });
    }
}

enum CloseChoice {
    Save,
    Discard,
    Cancel,
}
