use std::collections::HashSet;
use std::path::PathBuf;

use egui::text::LayoutJob;
use egui::{Color32, FontId, Sense, TextFormat};

use crate::jobs::{FileMatch, FilesSearchJob};

#[derive(Clone, Debug, PartialEq)]
pub enum ResultsEvent {
    Open { path: Option<PathBuf>, offset: u64, len: u64 },
}

struct Group {
    path: PathBuf,
    first: usize,
    count: usize,
}

#[derive(Clone, Copy)]
enum Row {
    Header(usize),
    Match(usize),
}

/// Search results: either a running files search or the matches of one document.
pub struct ResultsState {
    title: String,
    job: Option<FilesSearchJob>,
    /// Set for document results; `None` inside means an untitled document.
    doc_path: Option<Option<PathBuf>>,
    matches: Vec<FileMatch>,
    groups: Vec<Group>,
    collapsed: HashSet<usize>,
    rows: Vec<Row>,
    rows_dirty: bool,
    selected: Option<usize>,
    errors: Vec<String>,
    show_errors: bool,
    max: usize,
}

impl Default for ResultsState {
    fn default() -> Self {
        ResultsState::new()
    }
}

impl ResultsState {
    pub fn new() -> Self {
        ResultsState {
            title: String::new(),
            job: None,
            doc_path: None,
            matches: Vec::new(),
            groups: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: false,
            selected: None,
            errors: Vec::new(),
            show_errors: false,
            max: 0,
        }
    }

    /// Replaces the results with a running files search; a previous search is cancelled.
    pub fn set_job(&mut self, title: String, job: FilesSearchJob) {
        self.reset(title);
        self.max = job.max();
        self.job = Some(job);
    }

    /// Replaces the results with matches inside one document (`path` None for untitled).
    pub fn set_doc_results(&mut self, path: Option<PathBuf>, title: String, matches: Vec<FileMatch>) {
        self.reset(title);
        self.doc_path = Some(path);
        self.append(matches);
    }

    pub fn is_running(&self) -> bool {
        self.job.as_ref().is_some_and(|j| !j.is_done())
    }

    fn reset(&mut self, title: String) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        *self = ResultsState { title, ..ResultsState::new() };
    }

    fn append(&mut self, found: Vec<FileMatch>) {
        found.into_iter().for_each(|m| {
            let idx = self.matches.len();
            match self.groups.last_mut() {
                Some(g) if g.path == m.path => g.count += 1,
                _ => self.groups.push(Group { path: m.path.clone(), first: idx, count: 1 }),
            }
            self.matches.push(m);
            self.rows_dirty = true;
        });
    }

    fn poll_job(&mut self) {
        let polled = self.job.as_mut().map(|job| {
            let found = job.drain();
            let errors = job.errors().to_vec();
            (found, errors, job.is_done())
        });
        if let Some((found, errors, done)) = polled {
            self.append(found);
            self.errors = errors;
            if done {
                self.job = None;
            }
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows = self
            .groups
            .iter()
            .enumerate()
            .flat_map(|(gi, g)| {
                let open = !self.collapsed.contains(&gi);
                std::iter::once(Row::Header(gi))
                    .chain((g.first..g.first + g.count).filter(move |_| open).map(Row::Match))
            })
            .collect();
        self.rows_dirty = false;
    }

    fn open_event(&self, idx: usize) -> ResultsEvent {
        let m = &self.matches[idx];
        let path = match &self.doc_path {
            Some(p) => p.clone(),
            None => Some(m.path.clone()),
        };
        ResultsEvent::Open { path, offset: m.offset, len: m.match_cols.len() as u64 }
    }
}

fn match_layout(m: &FileMatch, font: FontId, fg: Color32, line_no: Color32, hit_bg: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let fmt = |color: Color32| TextFormat { font_id: font.clone(), color, ..Default::default() };
    job.append(&format!("{:>7}  ", m.line + 1), 0.0, fmt(line_no));
    let text = &m.line_text;
    let cols = m.match_cols.start.min(text.len())..m.match_cols.end.min(text.len());
    let valid = text.is_char_boundary(cols.start) && text.is_char_boundary(cols.end);
    let cols = if valid { cols } else { 0..0 };
    job.append(&text[..cols.start], 0.0, fmt(fg));
    job.append(&text[cols.clone()], 0.0, TextFormat { background: hit_bg, ..fmt(fg) });
    job.append(&text[cols.end..], 0.0, fmt(fg));
    job
}

/// Grouped by file, match highlighted, virtualized; click a match to open it.
pub fn show_results(ui: &mut egui::Ui, state: &mut ResultsState) -> Vec<ResultsEvent> {
    state.poll_job();
    if state.rows_dirty {
        state.rebuild_rows();
    }
    let mut events = Vec::new();
    let running = state.job.is_some();
    ui.horizontal(|ui| {
        ui.strong(&state.title);
        let limit = if state.max > 0 && state.matches.len() >= state.max { " (limit reached)" } else { "" };
        ui.label(format!("{} matches in {} files{limit}", state.matches.len(), state.groups.len()));
        if let Some(job) = &state.job {
            ui.add(egui::ProgressBar::new(job.progress()).desired_width(140.0).show_percentage());
            if ui.button("Cancel").clicked() {
                job.cancel();
            }
            ui.ctx().request_repaint();
        }
        if !state.errors.is_empty() {
            let text = format!("⚠ {} files failed", state.errors.len());
            if ui.selectable_label(state.show_errors, text).clicked() {
                state.show_errors = !state.show_errors;
            }
        }
        if !running && !state.groups.is_empty() {
            if ui.small_button("Collapse all").clicked() {
                state.collapsed = (0..state.groups.len()).collect();
                state.rows_dirty = true;
            }
            if ui.small_button("Expand all").clicked() {
                state.collapsed.clear();
                state.rows_dirty = true;
            }
        }
    });
    if state.show_errors {
        egui::ScrollArea::vertical().id_salt("kladde_results_errors").max_height(80.0).show(ui, |ui| {
            state.errors.iter().for_each(|e| {
                ui.colored_label(ui.visuals().error_fg_color, e);
            });
        });
    }
    ui.separator();
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts(|f| f.row_height(&font)) + 2.0;
    let visuals = ui.visuals().clone();
    let hit_bg = visuals.selection.bg_fill.gamma_multiply(0.6);
    let mut clicked: Option<Row> = None;
    egui::ScrollArea::both().id_salt("kladde_results").auto_shrink([false, false]).show_rows(
        ui,
        row_h,
        state.rows.len(),
        |ui, range| {
            state.rows[range].iter().for_each(|row| {
                let selected = matches!(row, Row::Match(i) if state.selected == Some(*i));
                let job = match *row {
                    Row::Header(gi) => {
                        let g = &state.groups[gi];
                        let arrow = if state.collapsed.contains(&gi) { "⏵" } else { "⏷" };
                        let name = match &state.doc_path {
                            Some(Some(p)) => p.display().to_string(),
                            Some(None) => "untitled".to_string(),
                            None => g.path.display().to_string(),
                        };
                        let mut job = LayoutJob::default();
                        let text = format!("{arrow} {name}  ({})", g.count);
                        let color = visuals.strong_text_color();
                        job.append(&text, 0.0, TextFormat { font_id: font.clone(), color, ..Default::default() });
                        job
                    }
                    Row::Match(i) => match_layout(
                        &state.matches[i],
                        font.clone(),
                        visuals.text_color(),
                        visuals.weak_text_color(),
                        hit_bg,
                    ),
                };
                let galley = ui.fonts(|f| f.layout_job(job));
                let width = ui.available_width().max(galley.size().x);
                let (rect, response) = ui.allocate_exact_size(egui::vec2(width, row_h), Sense::click());
                if selected {
                    ui.painter().rect_filled(rect, 0.0, visuals.selection.bg_fill.gamma_multiply(0.4));
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 0.0, visuals.widgets.hovered.weak_bg_fill);
                }
                ui.painter().galley(rect.left_top() + egui::vec2(2.0, 1.0), galley, visuals.text_color());
                if response.clicked() {
                    clicked = Some(*row);
                }
            });
        },
    );
    match clicked {
        Some(Row::Header(gi)) => {
            if !state.collapsed.remove(&gi) {
                state.collapsed.insert(gi);
            }
            state.rows_dirty = true;
        }
        Some(Row::Match(i)) => {
            state.selected = Some(i);
            events.push(state.open_event(i));
        }
        None => {}
    }
    events
}
