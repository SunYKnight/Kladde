use std::collections::VecDeque;
use std::path::PathBuf;

use egui::{Color32, RichText, Sense};
use kladde_tools::{quarto_output_file, RunEvent, RunHandle};

const LINE_CAP: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
pub enum OutputEvent {
    QuartoDone { doc: PathBuf, output: Option<PathBuf> },
    OpenPath(PathBuf),
}

struct Line {
    err: bool,
    text: String,
}

struct Run {
    title: String,
    handle: Option<RunHandle>,
    lines: VecDeque<Line>,
    dropped: u64,
    exit: Option<Option<i32>>,
    quarto_doc: Option<PathBuf>,
    quarto_output: Option<PathBuf>,
}

impl Run {
    fn push(&mut self, err: bool, text: String) {
        if self.lines.len() >= LINE_CAP {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(Line { err, text });
    }

    fn running(&self) -> bool {
        self.handle.is_some() && self.exit.is_none()
    }

    fn log(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

/// Output of script runs and Quarto renders, one tab per run.
pub struct OutputState {
    runs: Vec<Run>,
    active: usize,
    autoscroll: bool,
}

impl Default for OutputState {
    fn default() -> Self {
        OutputState::new()
    }
}

impl OutputState {
    pub fn new() -> Self {
        OutputState { runs: Vec::new(), active: 0, autoscroll: true }
    }

    /// Adds a tab for a started process; `quarto_doc` marks a Quarto render of that document.
    pub fn add_run(&mut self, handle: RunHandle, quarto_doc: Option<PathBuf>) {
        let title = handle.title().to_string();
        self.runs.push(Run {
            title,
            handle: Some(handle),
            lines: VecDeque::new(),
            dropped: 0,
            exit: None,
            quarto_doc,
            quarto_output: None,
        });
        self.active = self.runs.len() - 1;
    }

    /// Adds a tab with fixed text, e.g. hashes or an error report.
    pub fn add_message(&mut self, title: &str, text: &str) {
        let mut run = Run {
            title: title.to_string(),
            handle: None,
            lines: VecDeque::new(),
            dropped: 0,
            exit: None,
            quarto_doc: None,
            quarto_output: None,
        };
        text.lines().for_each(|l| run.push(false, l.to_string()));
        self.runs.push(run);
        self.active = self.runs.len() - 1;
    }

    /// Drains process output; call every frame.
    pub fn poll(&mut self) -> Vec<OutputEvent> {
        self.runs
            .iter_mut()
            .filter_map(|run| {
                let events = run.handle.as_ref().map(RunHandle::poll).unwrap_or_default();
                let mut done = None;
                events.into_iter().for_each(|e| match e {
                    RunEvent::Stdout(s) => run.push(false, s),
                    RunEvent::Stderr(s) => run.push(true, s),
                    RunEvent::Exited(code) => {
                        run.exit = Some(code);
                        done = run.quarto_doc.clone();
                    }
                });
                done.map(|doc| {
                    let output = quarto_output_file(&doc, &run.log());
                    run.quarto_output.clone_from(&output);
                    OutputEvent::QuartoDone { doc, output }
                })
            })
            .collect()
    }

    pub fn has_running(&self) -> bool {
        self.runs.iter().any(Run::running)
    }

    /// Kills every running process; failures are reported in the run's tab.
    pub fn stop_all(&mut self) {
        self.runs.iter_mut().filter(|r| r.running()).for_each(stop);
    }
}

fn stop(run: &mut Run) {
    let failed = run.handle.as_ref().map(RunHandle::kill).and_then(Result::err);
    if let Some(e) = failed {
        run.push(true, format!("stop failed: {e}"));
    }
}

fn exit_badge(run: &Run, ui: &egui::Ui) -> RichText {
    match run.exit {
        _ if run.running() => RichText::new("●").color(Color32::from_rgb(80, 160, 255)),
        Some(Some(0)) => RichText::new("✔").color(Color32::from_rgb(60, 170, 80)),
        Some(Some(code)) => RichText::new(format!("✖ {code}")).color(ui.visuals().error_fg_color),
        Some(None) => RichText::new("✖ killed").color(ui.visuals().error_fg_color),
        None => RichText::new(""),
    }
}

/// Tabs per run, stderr tinted, exit code badge, Stop / Clear / Copy all, autoscroll.
pub fn show_output(ui: &mut egui::Ui, state: &mut OutputState) -> Vec<OutputEvent> {
    let mut events = Vec::new();
    let mut close: Option<usize> = None;
    ui.horizontal_wrapped(|ui| {
        let active = state.active;
        state.runs.iter().enumerate().for_each(|(i, run)| {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    if ui.selectable_label(i == active, &run.title).clicked() {
                        state.active = i;
                    }
                    ui.label(exit_badge(run, ui));
                    if !run.running() && ui.small_button("×").on_hover_text("Close").clicked() {
                        close = Some(i);
                    }
                });
            });
        });
    });
    if let Some(i) = close {
        state.runs.remove(i);
        state.active = state.active.min(state.runs.len().saturating_sub(1));
    }
    let active = state.active;
    let autoscroll = &mut state.autoscroll;
    match state.runs.get_mut(active) {
        None => {
            ui.weak("No output yet.");
        }
        Some(run) => {
            ui.horizontal(|ui| {
                if ui.add_enabled(run.running(), egui::Button::new("⏹ Stop")).clicked() {
                    stop(run);
                }
                if ui.button("Clear").clicked() {
                    run.lines.clear();
                    run.dropped = 0;
                }
                if ui.button("Copy all").clicked() {
                    ui.ctx().copy_text(run.log());
                }
                ui.checkbox(autoscroll, "Autoscroll");
                if run.dropped > 0 {
                    ui.weak(format!("{} older lines dropped", run.dropped));
                }
                if let Some(out) = &run.quarto_output {
                    let link = ui.link(format!("Open {}", out.display()));
                    if link.clicked() {
                        events.push(OutputEvent::OpenPath(out.clone()));
                    }
                }
            });
            ui.separator();
            let font = egui::TextStyle::Monospace.resolve(ui.style());
            let row_h = ui.fonts(|f| f.row_height(&font));
            let (fg, err) = (ui.visuals().text_color(), ui.visuals().error_fg_color);
            egui::ScrollArea::both()
                .id_salt(("kladde_output", active))
                .auto_shrink([false, false])
                .stick_to_bottom(*autoscroll)
                .show_rows(ui, row_h, run.lines.len(), |ui, range| {
                    run.lines.range(range).for_each(|line| {
                        let color = if line.err { err } else { fg };
                        let galley = ui.fonts(|f| f.layout_no_wrap(line.text.clone(), font.clone(), color));
                        let width = ui.available_width().max(galley.size().x);
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), Sense::hover());
                        ui.painter().galley(rect.left_top(), galley, color);
                    });
                });
            if run.running() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }
    events
}
