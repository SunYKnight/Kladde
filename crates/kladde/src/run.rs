//! Running code blocks, selections and files (R1–R4), Quarto renders (Q1–Q3) and the Output tab.

use std::path::{Path, PathBuf};

use kladde_editor::EditorEvent;
use kladde_panels::{show_output, OutputEvent};
use kladde_tools::{find_fences, open_with_os, quarto_render, run_file, run_in_terminal, run_script, Interpreter};
use kladde_workspace::ViewKind;

use crate::commands::{BottomTab, PaletteKind, QUARTO_MISSING};
use crate::state::{App, DocId};

/// Fences are only scanned in documents up to this size (like the editor's ▶ markers).
const FENCE_SCAN_MAX: u64 = 8 << 20;

/// Code waiting for the user to pick an interpreter.
pub struct PendingCode {
    pub code: String,
    pub cwd: PathBuf,
    pub in_terminal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterSave {
    RunFile,
    Quarto(Option<&'static str>),
}

/// Action that runs once document `doc` has been saved.
pub struct PendingSave {
    pub doc: DocId,
    pub action: AfterSave,
}

/// Interpreter for an editor language id.
fn interpreter_for_language(id: &str) -> Option<Interpreter> {
    match id {
        "powershell" => Some(Interpreter::PowerShell),
        "batch" => Some(Interpreter::Cmd),
        "shell" => Some(Interpreter::Bash),
        "python" => Some(Interpreter::Python),
        "javascript" => Some(Interpreter::Node),
        "r" => Some(Interpreter::R),
        _ => None,
    }
}

/// Body and language of the fenced block whose header, body or closing line contains `caret`.
fn fence_at(text: &str, caret: usize) -> Option<(String, String)> {
    let line_starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    find_fences(text).into_iter().find_map(|f| {
        let start = line_starts.get(f.header_line).copied().unwrap_or(0);
        let after = text[f.content.end..].find('\n').map_or(text.len(), |p| f.content.end + p);
        let end = if f.closed { after } else { text.len() };
        (start <= caret && caret <= end).then(|| (f.lang, text[f.content].to_string()))
    })
}

impl App {
    /// Working directory for runs: the document's folder, else the workspace folder, else the temp dir.
    fn run_dir(&self, doc: Option<DocId>) -> PathBuf {
        let doc_dir = doc.and_then(|id| self.doc_path(id)).and_then(|p| p.parent().map(Path::to_path_buf));
        let ws_dir = self
            .active_ws
            .and_then(|i| self.workspaces.get(i))
            .and_then(|ws| ws.file_path())
            .and_then(|p| p.parent().map(Path::to_path_buf));
        doc_dir.or(ws_dir).unwrap_or_else(std::env::temp_dir)
    }

    pub fn start_run(&mut self, interp: Interpreter, code: &str, cwd: &Path, in_terminal: bool) {
        match (interp.program(), in_terminal) {
            (None, _) => self.toast_error(format!("{} is not installed (not found on PATH)", interp.label())),
            (Some(_), true) => match run_in_terminal(interp, code, cwd) {
                Ok(()) => self.toast_info(format!("Started {} in a terminal", interp.label())),
                Err(e) => self.toast_error(format!("Run in terminal failed: {e}")),
            },
            (Some(_), false) => match run_script(interp, code, cwd) {
                Ok(handle) => {
                    self.feat.output.add_run(handle, None);
                    self.feat.show_bottom = Some(BottomTab::Output);
                }
                Err(e) => self.toast_error(format!("Run failed: {e}")),
            },
        }
    }

    /// Runs `code` with the interpreter of the editor language, or asks for one.
    fn run_code(&mut self, code: String, language: &str, in_terminal: bool) {
        let cwd = self.run_dir(self.focused_doc());
        match interpreter_for_language(language) {
            Some(interp) => self.start_run(interp, &code, &cwd, in_terminal),
            None => {
                self.feat.pending_code = Some(PendingCode { code, cwd, in_terminal });
                self.open_palette(PaletteKind::Interpreter);
            }
        }
    }

    /// Fenced block at the caret as (language, code); errors explain why there is none.
    fn fence_at_caret(&self) -> Result<(String, String), String> {
        let (entry, tab) = self.focused_view().ok_or("no document open")?;
        if entry.doc.len() > FENCE_SCAN_MAX {
            Err("code blocks are only detected in documents up to 8 MiB".to_string())
        } else {
            let text = entry.doc.read_string_lossy(0..entry.doc.len());
            let caret = (tab.editor.caret() as usize).min(text.len());
            fence_at(&text, caret).ok_or_else(|| "the caret is not inside a fenced code block".to_string())
        }
    }

    pub fn run_fence_at_caret(&mut self, in_terminal: bool) {
        match self.fence_at_caret() {
            Err(e) => self.toast_info(format!("Run code block: {e}")),
            Ok((lang, code)) => match Interpreter::from_lang(&lang) {
                Some(interp) => {
                    let cwd = self.run_dir(self.focused_doc());
                    self.start_run(interp, &code, &cwd, in_terminal);
                }
                None if lang.is_empty() => self.run_code(code, "", in_terminal),
                None => self.toast_info(format!("Run code block: no interpreter for \"{lang}\"")),
            },
        }
    }

    fn selection_code(&self) -> Option<(String, &'static str)> {
        self.focused_view()
            .filter(|(_, t)| t.view == ViewKind::Text && !t.editor.primary_range().is_empty())
            .map(|(e, t)| (t.editor.selected_text(&e.doc), t.editor.language().id))
    }

    /// Runs the selection; without one, the code block at the caret.
    pub fn run_selection(&mut self, in_terminal: bool) {
        match self.selection_code() {
            Some((code, lang)) => self.run_code(code, lang, in_terminal),
            None if self.fence_at_caret().is_ok() => self.run_fence_at_caret(in_terminal),
            None => self.toast_info("Run: select code or place the caret inside a fenced code block"),
        }
    }

    pub fn run_in_terminal_cmd(&mut self) {
        self.run_selection(true);
    }

    /// Saves `doc` first when it is dirty or untitled, then runs `action`.
    fn after_saved(&mut self, doc: DocId, action: AfterSave) {
        let state = self.docs.get(&doc).map(|e| (e.doc.is_dirty() || e.doc.path().is_none(), e.doc.path().is_none()));
        match state {
            None => {}
            Some((true, untitled)) => {
                self.save_doc(doc, untitled);
                self.feat.after_save.push(PendingSave { doc, action });
            }
            Some((false, _)) => self.do_after_save(doc, action),
        }
    }

    fn do_after_save(&mut self, doc: DocId, action: AfterSave) {
        match (self.doc_path(doc), action) {
            (None, _) => self.toast_info("The document has no file on disk"),
            (Some(path), AfterSave::RunFile) => self.run_saved_file(&path),
            (Some(path), AfterSave::Quarto(format)) => self.quarto_path_cmd(&path, format),
        }
    }

    pub fn run_file_cmd(&mut self, doc: DocId) {
        self.after_saved(doc, AfterSave::RunFile);
    }

    pub fn quarto_cmd(&mut self, doc: DocId, format: Option<&'static str>) {
        self.after_saved(doc, AfterSave::Quarto(format));
    }

    fn run_saved_file(&mut self, path: &Path) {
        match run_file(path) {
            Ok(handle) => {
                self.feat.output.add_run(handle, None);
                self.feat.show_bottom = Some(BottomTab::Output);
            }
            Err(e) => self.toast_error(format!("Run file failed: {e}")),
        }
    }

    /// Workspace tree "Run file": saves an open, modified copy first.
    pub fn run_path(&mut self, _ctx: &egui::Context, path: &Path) {
        match self.doc_by_path(path) {
            Some(doc) => self.run_file_cmd(doc),
            None => self.run_saved_file(path),
        }
    }

    pub fn quarto_path_cmd(&mut self, path: &Path, format: Option<&'static str>) {
        if self.quarto.is_none() {
            self.toast_info(format!("Render: {QUARTO_MISSING}"));
        } else {
            let open = self.doc_by_path(path).filter(|d| self.docs.get(d).is_some_and(|e| e.doc.is_dirty()));
            match open {
                Some(doc) => self.quarto_cmd(doc, format),
                None => match quarto_render(path, format) {
                    Ok(handle) => {
                        self.feat.output.add_run(handle, Some(path.to_path_buf()));
                        self.feat.show_bottom = Some(BottomTab::Output);
                    }
                    Err(e) => self.toast_error(format!("Quarto render failed: {e}")),
                },
            }
        }
    }

    /// Runs actions whose document finished saving; drops them when the save failed or was cancelled.
    pub fn poll_after_save(&mut self) {
        let pending = std::mem::take(&mut self.feat.after_save);
        let (waiting, ready): (Vec<PendingSave>, Vec<PendingSave>) =
            pending.into_iter().partition(|p| self.docs.get(&p.doc).is_some_and(|e| e.save.is_some()));
        self.feat.after_save = waiting;
        ready.into_iter().for_each(|p| {
            let saved = self.docs.get(&p.doc).is_some_and(|e| !e.doc.is_dirty() && e.doc.path().is_some());
            if saved {
                self.do_after_save(p.doc, p.action);
            } else {
                self.toast_info("Not saved; run/render cancelled");
            }
        });
    }

    fn handle_output_events(&mut self, events: Vec<OutputEvent>) {
        events.into_iter().for_each(|e| match e {
            OutputEvent::QuartoDone { output: Some(file), .. } | OutputEvent::OpenPath(file) => {
                if let Err(err) = open_with_os(&file) {
                    self.toast_error(format!("Could not open {}: {err}", file.display()));
                }
            }
            OutputEvent::QuartoDone { doc, output: None } => {
                self.toast_info(format!("Quarto finished {} without reporting an output file", doc.display()))
            }
        });
    }

    pub fn poll_output(&mut self) {
        let events = self.feat.output.poll();
        self.handle_output_events(events);
    }

    pub fn show_output_tab(&mut self, ui: &mut egui::Ui) {
        let events = show_output(ui, &mut self.feat.output);
        if self.feat.output.has_running() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.handle_output_events(events);
    }

    pub fn handle_editor_events(&mut self, _ctx: &egui::Context, events: Vec<EditorEvent>) {
        events.into_iter().for_each(|e| match e {
            EditorEvent::Run { interp, code, in_terminal } => {
                let cwd = self.run_dir(self.focused_doc());
                self.start_run(interp, &code, &cwd, in_terminal);
            }
            EditorEvent::Error(msg) => self.toast_error(msg),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::fence_at;

    #[test]
    fn fence_at_covers_header_body_and_closing_line_only() {
        let text = "intro\n```python\nprint(1)\n```\nafter\n";
        let header = text.find("```python").expect("header present");
        let closing = text.rfind("```").expect("closing present");
        assert_eq!(fence_at(text, header), Some(("python".to_string(), "print(1)\n".to_string())));
        assert!(fence_at(text, closing + 3).is_some());
        assert_eq!(fence_at(text, 0), None);
        assert_eq!(fence_at(text, text.find("after").expect("tail present")), None);
    }
}
