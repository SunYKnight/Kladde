//! Find/replace bar, background searches, match highlighting and the Results tab (E5, H6, L4).

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use kladde_core::{Progress, Searcher};
use kladde_panels::{
    file_matches, parse_hex, show_find_bar, show_results, DocSearchJob, FileMatch, FilesSearchJob, FindAction,
    FindScope, ResultsEvent,
};
use kladde_workspace::ViewKind;

use crate::commands::{poll_task, spawn_task, BottomTab};
use crate::state::{App, DocId};

const HIGHLIGHT_MAX: usize = 10_000;
const FIND_ALL_MAX: usize = 10_000;
const FILES_MAX: usize = 10_000;
const REPLACE_ALL_MAX: usize = 1_000_000;
const WORKSPACE_FILES_MAX: usize = 50_000;
/// Selections longer than this do not seed the find query.
const SEED_MAX: u64 = 1_000;

enum Purpose {
    Step { forward: bool, wrapped: bool },
    ReplaceAll,
    FindAll { path: Option<PathBuf>, title: String },
}

enum Found {
    One(Option<Range<u64>>),
    Many(Vec<Range<u64>>),
    Listed(Vec<FileMatch>),
}

struct FindJob {
    doc: DocId,
    revision: u64,
    purpose: Purpose,
    searcher: Arc<Searcher>,
    progress: Arc<Progress>,
    rx: Receiver<Result<Found, String>>,
}

struct Highlight {
    doc: DocId,
    key: String,
    job: Option<DocSearchJob>,
    count: Option<usize>,
}

/// Match selected by Next/Prev; ReplaceOne replaces it while it is still selected and unchanged.
struct LastMatch {
    doc: DocId,
    revision: u64,
    range: Range<u64>,
    searcher: Arc<Searcher>,
}

#[derive(Default)]
pub struct SearchState {
    job: Option<FindJob>,
    highlight: Option<Highlight>,
    last: Option<LastMatch>,
    status: String,
    /// Documents whose views currently paint find matches.
    marked: HashSet<DocId>,
}

impl SearchState {
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.highlight.as_ref().is_some_and(|h| h.job.is_some())
    }

    /// Cancels searches holding a snapshot of `id`; the highlight is not restarted until the text changes.
    pub fn cancel_doc(&mut self, id: DocId) {
        if self.job.as_ref().is_some_and(|j| j.doc == id) {
            if let Some(job) = self.job.take() {
                job.progress.cancel();
            }
        }
        if let Some(h) = self.highlight.as_mut().filter(|h| h.doc == id) {
            if let Some(job) = h.job.take() {
                job.cancel();
            }
        }
    }

    fn cancel_job(&mut self) {
        if let Some(job) = self.job.take() {
            job.progress.cancel();
        }
    }
}

impl App {
    fn compile_search(&mut self) -> Option<Arc<Searcher>> {
        let compiled =
            self.feat.find.pattern().and_then(|p| Searcher::new(&p).map_err(|e| e.to_string())).map(Arc::new);
        match compiled {
            Ok(s) => Some(s),
            Err(e) => {
                self.feat.search.status = e.clone();
                self.toast_error(format!("Find: {e}"));
                None
            }
        }
    }

    fn start_job(&mut self, doc: DocId, revision: u64, purpose: Purpose, searcher: Arc<Searcher>, work: Work) {
        self.feat.search.cancel_job();
        let progress = Progress::new();
        let (p, s) = (Arc::clone(&progress), Arc::clone(&searcher));
        let rx = spawn_task(move || work(&s, &p));
        self.feat.search.job = Some(FindJob { doc, revision, purpose, searcher, progress, rx });
    }

    pub fn open_find(&mut self, replace: bool) {
        let seed = self
            .focused_view()
            .filter(|(_, t)| t.view == ViewKind::Text)
            .filter(|(_, t)| t.editor.primary_range().end - t.editor.primary_range().start <= SEED_MAX)
            .map(|(e, t)| t.editor.selected_text(&e.doc))
            .unwrap_or_default();
        self.feat.find.open(replace, &seed);
    }

    pub fn open_find_in_files(&mut self) {
        self.open_find(false);
        self.feat.find.scope = if self.active_ws.is_some() { FindScope::Workspace } else { FindScope::OpenFiles };
    }

    pub fn find_step(&mut self, forward: bool) {
        if self.feat.find.query.is_empty() {
            self.open_find(false);
        } else {
            self.start_step(forward, false);
        }
    }

    fn start_step(&mut self, forward: bool, wrapped: bool) {
        let target = self.active_parts().map(|(entry, tab)| {
            let sel = match tab.view {
                ViewKind::Text => tab.editor.primary_range(),
                ViewKind::Hex => {
                    let s = tab.hex.selection();
                    if s.is_empty() {
                        tab.hex.cursor()..tab.hex.cursor()
                    } else {
                        s
                    }
                }
            };
            let pos = match (wrapped, forward) {
                (true, true) => 0,
                (true, false) => entry.doc.len(),
                (false, true) => sel.end,
                (false, false) => sel.start,
            };
            (entry.id, entry.doc.revision(), entry.doc.snapshot(), pos)
        });
        match target {
            None => self.toast_info("Find: no document open"),
            Some((doc, revision, snapshot, pos)) => {
                if let Some(searcher) = self.compile_search() {
                    let work: Work = Box::new(move |s, p| {
                        let found =
                            if forward { s.find_next(&snapshot, pos, p) } else { s.find_prev(&snapshot, pos, p) };
                        found.map(Found::One).map_err(|e| e.to_string())
                    });
                    self.start_job(doc, revision, Purpose::Step { forward, wrapped }, searcher, work);
                }
            }
        }
    }

    fn replace_one(&mut self) {
        let replacement = self.feat.find.replacement.clone();
        let bytes_mode = self.feat.find.bytes;
        let last = self.feat.search.last.take();
        let outcome = self.active_parts().and_then(|(entry, tab)| {
            let current = match tab.view {
                ViewKind::Text => tab.editor.primary_range(),
                ViewKind::Hex => tab.hex.selection(),
            };
            let last = last.filter(|l| {
                l.doc == entry.id && l.revision == entry.doc.revision() && l.range == current && !current.is_empty()
            })?;
            let bytes = if bytes_mode {
                parse_hex(&replacement)
            } else {
                Ok(last.searcher.expand_replacement(&entry.doc.snapshot(), last.range.clone(), &replacement))
            };
            let result = bytes.and_then(|b| {
                entry.doc.replace(last.range.clone(), &b).map_err(|e| e.to_string())?;
                let end = last.range.start + b.len() as u64;
                match tab.view {
                    ViewKind::Text => tab.editor.set_caret(&entry.doc, end),
                    ViewKind::Hex => tab.hex.goto(&entry.doc, end),
                }
                Ok(())
            });
            Some(result)
        });
        match outcome {
            Some(Err(e)) => self.toast_error(format!("Replace: {e}")),
            _ => self.start_step(true, false),
        }
    }

    fn replace_all(&mut self) {
        let target = self.focused_view().map(|(e, _)| (e.id, e.doc.revision(), e.doc.snapshot(), e.doc.is_read_only()));
        match target {
            None => self.toast_info("Replace all: no document open"),
            Some((_, _, _, true)) => self.toast_info("Replace all: document is read-only"),
            Some((doc, revision, snapshot, false)) => {
                if let Some(searcher) = self.compile_search() {
                    let work: Work = Box::new(move |s, p| {
                        let mut found = Vec::new();
                        let result =
                            s.find_all(&snapshot, 0..snapshot.len(), p, REPLACE_ALL_MAX, &mut |m| found.push(m));
                        result.map(|_| Found::Many(found)).map_err(|e| e.to_string())
                    });
                    self.start_job(doc, revision, Purpose::ReplaceAll, searcher, work);
                }
            }
        }
    }

    fn apply_replace_all(&mut self, job: &FindJob, ranges: &[Range<u64>]) {
        let replacement = self.feat.find.replacement.clone();
        let bytes_mode = self.feat.find.bytes;
        let outcome = match self.docs.get_mut(&job.doc) {
            None => Err("the document was closed".to_string()),
            Some(entry) if entry.doc.revision() != job.revision => {
                Err("the document changed during the search; nothing replaced".to_string())
            }
            Some(entry) => {
                let snapshot = entry.doc.snapshot();
                let fixed = if bytes_mode { Some(parse_hex(&replacement)) } else { None };
                let replaced: Result<Vec<Edit>, String> = ranges
                    .iter()
                    .map(|m| match &fixed {
                        Some(b) => b.clone().map(|b| (m.clone(), b)),
                        None => Ok((m.clone(), job.searcher.expand_replacement(&snapshot, m.clone(), &replacement))),
                    })
                    .collect();
                replaced.and_then(|edits| {
                    entry.doc.begin_undo_group();
                    let result = edits.iter().rev().try_for_each(|(m, b)| entry.doc.replace(m.clone(), b));
                    entry.doc.end_undo_group();
                    result.map(|_| edits.len()).map_err(|e| e.to_string())
                })
            }
        };
        match outcome {
            Ok(0) => self.toast_info("Replace all: no matches"),
            Ok(n) if n >= REPLACE_ALL_MAX => {
                self.toast_info(format!("Replaced the first {n} occurrences (limit); run Replace all again"))
            }
            Ok(n) => self.toast_info(format!("Replaced {n} occurrences")),
            Err(e) => self.toast_error(format!("Replace all: {e}")),
        }
    }

    fn find_all(&mut self) {
        match self.feat.find.scope {
            FindScope::Document => self.find_all_in_doc(),
            FindScope::OpenFiles => {
                let mut files: Vec<PathBuf> =
                    self.docs.values().filter_map(|e| e.doc.path().map(Path::to_path_buf)).collect();
                files.sort();
                files.dedup();
                self.find_in_files(files, "open files");
            }
            FindScope::Workspace => {
                let files = self.active_workspace().map(|ws| ws.all_files(WORKSPACE_FILES_MAX));
                match files {
                    Some(files) => self.find_in_files(files, "workspace"),
                    None => self.toast_info("Find in workspace: no workspace open; choose the scope \"Open files\""),
                }
            }
        }
    }

    fn find_in_files(&mut self, files: Vec<PathBuf>, scope: &str) {
        match self.feat.find.pattern() {
            Err(e) => self.toast_error(format!("Find: {e}")),
            Ok(_) if files.is_empty() => self.toast_info(format!("Find in {scope}: no files to search")),
            Ok(pattern) => {
                let title = format!("\"{}\" in {scope} ({} files)", self.feat.find.query, files.len());
                self.feat.results.set_job(title, FilesSearchJob::start(files, pattern, FILES_MAX));
                self.feat.show_bottom = Some(BottomTab::Results);
            }
        }
    }

    fn find_all_in_doc(&mut self) {
        let target = self.focused_view().map(|(e, _)| {
            let path = e.doc.path().map(Path::to_path_buf);
            let name =
                path.as_deref().and_then(Path::file_name).map_or(e.title.clone(), |n| n.to_string_lossy().into());
            (e.id, e.doc.revision(), e.doc.snapshot(), path, name)
        });
        match target {
            None => self.toast_info("Find all: no document open"),
            Some((doc, revision, snapshot, path, name)) => {
                if let Some(searcher) = self.compile_search() {
                    let title = format!("\"{}\" in {name}", self.feat.find.query);
                    let label = path.clone().unwrap_or_else(|| PathBuf::from(&name));
                    let work: Work = Box::new(move |s, p| {
                        let mut found = Vec::new();
                        let result = s.find_all(&snapshot, 0..snapshot.len(), p, FIND_ALL_MAX, &mut |m| found.push(m));
                        result
                            .map(|_| Found::Listed(file_matches(&label, &snapshot, &found)))
                            .map_err(|e| e.to_string())
                    });
                    self.start_job(doc, revision, Purpose::FindAll { path, title }, searcher, work);
                }
            }
        }
    }

    fn select_in_active(&mut self, doc: DocId, range: Range<u64>) {
        if let Some((entry, tab)) = self.active_parts().filter(|(e, _)| e.id == doc) {
            match tab.view {
                ViewKind::Text => tab.editor.select(&entry.doc, range),
                ViewKind::Hex => tab.hex.select(&entry.doc, range),
            }
        }
    }

    fn finish_job(&mut self, job: FindJob, result: Result<Found, String>) {
        let cancelled = job.progress.is_cancelled();
        match (result, &job.purpose) {
            (Err(_), _) if cancelled => self.feat.search.status = "Search cancelled".to_string(),
            (Err(e), _) => {
                self.feat.search.status = e.clone();
                self.toast_error(format!("Find: {e}"));
            }
            (Ok(Found::One(Some(m))), _) => {
                self.feat.search.status.clear();
                self.select_in_active(job.doc, m.clone());
                self.feat.search.last =
                    Some(LastMatch { doc: job.doc, revision: job.revision, range: m, searcher: job.searcher });
            }
            (Ok(Found::One(None)), Purpose::Step { forward, wrapped: false }) => {
                self.toast_info(if *forward { "Find: wrapped to the start" } else { "Find: wrapped to the end" });
                self.start_step(*forward, true);
            }
            (Ok(Found::One(None)), _) => {
                self.feat.search.status = "Not found".to_string();
                self.toast_info(format!("Find: \"{}\" not found", self.feat.find.query));
            }
            (Ok(Found::Many(ranges)), _) => self.apply_replace_all(&job, &ranges),
            (Ok(Found::Listed(matches)), Purpose::FindAll { path, title }) => {
                let title = format!("{title}: {} matches", matches.len());
                self.feat.results.set_doc_results(path.clone(), title, matches);
                self.feat.show_bottom = Some(BottomTab::Results);
            }
            (Ok(Found::Listed(_)), _) => {}
        }
    }

    pub fn poll_search(&mut self) {
        let done = self.feat.search.job.as_ref().and_then(|j| poll_task(&j.rx, "search"));
        if let Some(result) = done {
            if let Some(job) = self.feat.search.job.take() {
                self.finish_job(job, result.and_then(|r| r));
            }
        }
        self.update_highlight();
    }

    fn clear_marks(&mut self) {
        let marked: Vec<DocId> = self.feat.search.marked.drain().collect();
        marked.into_iter().for_each(|d| self.views_of(d).for_each(|t| t.editor.set_find_matches(Vec::new())));
    }

    /// Highlights all matches of the open find bar in the active document (docs up to the large-file size).
    fn update_highlight(&mut self) {
        let limit = self.settings.large_file_mb.saturating_mul(1 << 20);
        let pattern = self.feat.find.pattern().ok().filter(|_| self.feat.find.is_open());
        let target = self
            .focused_view()
            .filter(|(e, t)| e.load.is_none() && t.view == ViewKind::Text && e.doc.len() <= limit)
            .map(|(e, _)| (e.id, e.doc.revision()));
        match (pattern, target) {
            (Some(p), Some((doc, revision))) => {
                let key = format!("{p:?}|{doc}|{revision}");
                let stale = self.feat.search.highlight.as_ref().is_none_or(|h| h.key != key);
                if stale {
                    self.start_highlight(doc, key, &p);
                }
                self.poll_highlight();
            }
            _ => {
                if let Some(job) = self.feat.search.highlight.take().and_then(|h| h.job) {
                    job.cancel();
                }
                self.clear_marks();
            }
        }
    }

    fn start_highlight(&mut self, doc: DocId, key: String, pattern: &kladde_core::SearchPattern) {
        if let Some(job) = self.feat.search.highlight.take().and_then(|h| h.job) {
            job.cancel();
        }
        let job = match (Searcher::new(pattern), self.docs.get(&doc)) {
            (Ok(s), Some(e)) => Some(DocSearchJob::start(e.doc.snapshot(), Arc::new(s), HIGHLIGHT_MAX)),
            _ => None,
        };
        self.feat.search.highlight = Some(Highlight { doc, key, job, count: None });
    }

    fn poll_highlight(&mut self) {
        let polled = self.feat.search.highlight.as_mut().and_then(|h| {
            let result = h.job.as_mut()?.poll()?;
            h.job = None;
            Some((h.doc, result))
        });
        match polled {
            Some((doc, Ok(ranges))) => {
                if let Some(h) = self.feat.search.highlight.as_mut() {
                    h.count = Some(ranges.len());
                }
                self.clear_marks();
                self.views_of(doc).for_each(|t| t.editor.set_find_matches(ranges.clone()));
                self.feat.search.marked.insert(doc);
            }
            Some((_, Err(e))) => self.feat.search.status = e,
            None => {}
        }
    }

    fn find_status(&self) -> String {
        let search = &self.feat.search;
        let highlight = search.highlight.as_ref();
        match (&search.job, highlight.and_then(|h| h.job.as_ref()), highlight.and_then(|h| h.count)) {
            (Some(job), _, _) => format!("Searching… {:.0}%", job.progress.fraction() * 100.0),
            (None, Some(job), _) => format!("Counting… {:.0}%", job.progress() * 100.0),
            (None, None, Some(n)) if n >= HIGHLIGHT_MAX => format!("{n}+ matches  {}", search.status),
            (None, None, Some(n)) => format!("{n} matches  {}", search.status),
            (None, None, None) => search.status.clone(),
        }
    }

    pub fn show_find_bar_area(&mut self, ui: &mut egui::Ui) {
        if self.feat.find.is_open() {
            let status = self.find_status();
            let actions = show_find_bar(ui, &mut self.feat.find, &status);
            if let Some(job) = &self.feat.search.job {
                let fraction = job.progress.fraction();
                let cancel = ui
                    .horizontal(|ui| {
                        ui.add(egui::ProgressBar::new(fraction).desired_width(200.0).show_percentage());
                        ui.button("Cancel search").clicked()
                    })
                    .inner;
                if cancel {
                    job.progress.cancel();
                }
            }
            actions.into_iter().for_each(|a| self.find_action(a));
        }
    }

    fn find_action(&mut self, action: FindAction) {
        match action {
            FindAction::Next => self.start_step(true, false),
            FindAction::Prev => self.start_step(false, false),
            FindAction::ReplaceOne => self.replace_one(),
            FindAction::ReplaceAll if self.feat.find.scope != FindScope::Document => {
                self.toast_info("Replace all works on the current document; set the scope to \"Document\"")
            }
            FindAction::ReplaceAll => self.replace_all(),
            FindAction::FindAll => self.find_all(),
            FindAction::Close => self.feat.search.cancel_job(),
        }
    }

    pub fn show_results_tab(&mut self, ui: &mut egui::Ui) {
        let events = show_results(ui, &mut self.feat.results);
        if self.feat.results.is_running() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        events
            .into_iter()
            .for_each(|ResultsEvent::Open { path, offset, len }| self.open_at(path.as_deref(), offset, len));
    }
}

/// Search run on a worker thread with the compiled searcher and the job's progress.
type Work = Box<dyn FnOnce(&Searcher, &Progress) -> Result<Found, String> + Send>;

/// A replacement: matched range and the bytes that replace it.
type Edit = (Range<u64>, Vec<u8>);
