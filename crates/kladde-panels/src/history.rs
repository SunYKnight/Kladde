use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

use egui::Sense;
use kladde_tools::{diff_text, DiffLine, History, Version};

use crate::diff_view::{show_diff, DiffTheme};

/// Texts above this size are not diffed (line diff cost grows quickly).
const DIFF_LIMIT: usize = 8 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum HistoryEvent {
    Restore { file: PathBuf, bytes: Vec<u8> },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Preview,
    Diff,
}

struct Pending<T> {
    rx: Receiver<Result<T, String>>,
}

impl<T: Send + 'static> Pending<T> {
    fn spawn(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // The receiver is gone when another file or version was chosen meanwhile.
            drop(tx.send(work()));
        });
        Pending { rx }
    }

    fn poll(&self) -> Option<Result<T, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("history worker stopped unexpectedly".to_string())),
        }
    }
}

struct Blob {
    version: usize,
    bytes: Vec<u8>,
    lines: Vec<String>,
}

/// Version list of one file with preview, diff against the current text and restore.
pub struct HistoryState {
    history: Option<Arc<History>>,
    file: Option<PathBuf>,
    versions: Vec<Version>,
    loading_versions: Option<Pending<Vec<Version>>>,
    selected: Option<usize>,
    loading_blob: Option<(usize, Pending<Vec<u8>>)>,
    blob: Option<Blob>,
    view: View,
    diff: Option<(usize, u64, Vec<DiffLine>)>,
    error: Option<String>,
}

impl Default for HistoryState {
    fn default() -> Self {
        HistoryState::new()
    }
}

impl HistoryState {
    pub fn new() -> Self {
        HistoryState {
            history: None,
            file: None,
            versions: Vec::new(),
            loading_versions: None,
            selected: None,
            loading_blob: None,
            blob: None,
            view: View::Preview,
            diff: None,
            error: None,
        }
    }

    /// Shows the versions of `file`, loading them on a worker thread.
    pub fn show_file(&mut self, history: Arc<History>, file: PathBuf) {
        *self = HistoryState::new();
        let (h, f) = (Arc::clone(&history), file.clone());
        self.loading_versions = Some(Pending::spawn(move || h.versions(&f).map_err(|e| e.to_string())));
        self.history = Some(history);
        self.file = Some(file);
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    fn select(&mut self, idx: usize) {
        self.selected = Some(idx);
        self.blob = None;
        self.diff = None;
        if let (Some(history), Some(v)) = (&self.history, self.versions.get(idx)) {
            let (h, v) = (Arc::clone(history), v.clone());
            self.loading_blob = Some((idx, Pending::spawn(move || h.read(&v).map_err(|e| e.to_string()))));
        }
    }

    fn poll(&mut self) {
        if let Some(r) = self.loading_versions.as_ref().and_then(Pending::poll) {
            self.loading_versions = None;
            match r {
                Ok(v) => {
                    self.versions = v;
                    if !self.versions.is_empty() {
                        self.select(0);
                    }
                }
                Err(e) => self.error = Some(e),
            }
        }
        let blob = self.loading_blob.as_ref().and_then(|(i, p)| p.poll().map(|r| (*i, r)));
        if let Some((version, r)) = blob {
            self.loading_blob = None;
            match r {
                Ok(bytes) => {
                    let lines = String::from_utf8_lossy(&bytes).lines().map(|l| l.replace('\t', "    ")).collect();
                    self.blob = Some(Blob { version, bytes, lines });
                }
                Err(e) => self.error = Some(e),
            }
        }
    }

    fn loading(&self) -> bool {
        self.loading_versions.is_some() || self.loading_blob.is_some()
    }
}

/// "YYYY-MM-DD HH:MM:SS UTC" for Unix seconds (civil-from-days, proleptic Gregorian).
pub fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", tod / 3600, tod % 3600 / 60, tod % 60)
}

fn format_size(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

fn text_key(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Version list (time, size), preview, diff against `current_text` and Restore.
pub fn show_history(ui: &mut egui::Ui, state: &mut HistoryState, current_text: Option<&str>) -> Vec<HistoryEvent> {
    state.poll();
    if state.loading() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
    let mut events = Vec::new();
    match state.file.clone() {
        None => {
            ui.weak("No file selected.");
        }
        Some(file) => {
            ui.horizontal(|ui| {
                ui.strong(format!("History of {}", file.display()));
                if state.loading() {
                    ui.spinner();
                }
            });
            if let Some(e) = &state.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let mut clicked = None;
            egui::SidePanel::left(ui.id().with("kladde_history_list"))
                .resizable(true)
                .default_width(240.0)
                .show_inside(ui, |ui| {
                    if state.versions.is_empty() && state.loading_versions.is_none() {
                        ui.weak("No stored versions.");
                    }
                    egui::ScrollArea::vertical().id_salt("kladde_history_versions").show(ui, |ui| {
                        state.versions.iter().enumerate().for_each(|(i, v)| {
                            let text = format!("{}   {}", format_utc(v.time), format_size(v.size));
                            if ui.selectable_label(state.selected == Some(i), text).clicked() {
                                clicked = Some(i);
                            }
                        });
                    });
                });
            if let Some(i) = clicked {
                state.select(i);
            }
            egui::CentralPanel::default()
                .show_inside(ui, |ui| show_version(ui, state, &file, current_text, &mut events));
        }
    }
    events
}

fn show_version(
    ui: &mut egui::Ui,
    state: &mut HistoryState,
    file: &Path,
    current_text: Option<&str>,
    events: &mut Vec<HistoryEvent>,
) {
    let current = state.blob.as_ref().filter(|b| Some(b.version) == state.selected);
    match current {
        None => {
            ui.weak(if state.loading() { "Loading…" } else { "Select a version." });
        }
        Some(blob) => {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.view, View::Preview, "Preview");
                ui.add_enabled_ui(current_text.is_some(), |ui| {
                    ui.selectable_value(&mut state.view, View::Diff, "Diff with current");
                });
                if ui.button("Restore").on_hover_text("Replace the editor content with this version").clicked() {
                    events.push(HistoryEvent::Restore { file: file.to_path_buf(), bytes: blob.bytes.clone() });
                }
            });
            ui.separator();
            match (state.view, current_text) {
                (View::Diff, Some(text)) if text.len() <= DIFF_LIMIT && blob.bytes.len() <= DIFF_LIMIT => {
                    let key = text_key(text);
                    let fresh = state.diff.as_ref().is_some_and(|(v, k, _)| *v == blob.version && *k == key);
                    if !fresh {
                        let old = String::from_utf8_lossy(&blob.bytes);
                        state.diff = Some((blob.version, key, diff_text(&old, text)));
                    }
                    let theme = if ui.visuals().dark_mode { DiffTheme::dark() } else { DiffTheme::light() };
                    if let Some((_, _, lines)) = &state.diff {
                        show_diff(ui, lines, &theme);
                    }
                }
                (View::Diff, _) => {
                    ui.weak("Too large to diff.");
                }
                (View::Preview, _) => preview(ui, &blob.lines),
            }
        }
    }
}

fn preview(ui: &mut egui::Ui, lines: &[String]) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts(|f| f.row_height(&font));
    let color = ui.visuals().text_color();
    egui::ScrollArea::both().id_salt("kladde_history_preview").auto_shrink([false, false]).show_rows(
        ui,
        row_h,
        lines.len(),
        |ui, range| {
            lines[range].iter().for_each(|l| {
                let galley = ui.fonts(|f| f.layout_no_wrap(l.clone(), font.clone(), color));
                let width = ui.available_width().max(galley.size().x);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), Sense::hover());
                ui.painter().galley(rect.left_top(), galley, color);
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(format_utc(-1), "1969-12-31 23:59:59 UTC");
    }
}
