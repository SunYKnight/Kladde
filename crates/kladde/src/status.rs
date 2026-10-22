//! Status bar: position, selection, size, encoding, line ending, language, mode flags and job spinner.

use egui::Ui;
use kladde_core::{OpenMode, OpenOptions, TextEncoding};
use kladde_hex::{HexMode, TextColumn};
use kladde_syntax::Language;
use kladde_workspace::ViewKind;

use crate::state::{App, DocEntry, DocId, TabView};

enum StatusAction {
    Command(String),
    Reopen(DocId, TextEncoding),
    Language(&'static Language),
    ToggleInsert,
    HexMode(HexMode),
    HexText(TextColumn),
    ToggleInspector,
}

/// "1.4 MB" style size.
pub fn human_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let (value, unit) = units[1..].iter().fold((bytes as f64, units[0]), |(v, u), next| match v >= 1024.0 {
        true => (v / 1024.0, *next),
        false => (v, u),
    });
    match unit {
        "B" => format!("{bytes} B"),
        _ => format!("{value:.1} {unit}"),
    }
}

impl App {
    pub(crate) fn show_status_bar(&mut self, ctx: &egui::Context) {
        let mut actions: Vec<StatusAction> = Vec::new();
        egui::TopBottomPanel::bottom("kladde_status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                self.status_contents(ui, &mut actions);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kladde_workspace::is_portable() {
                        ui.weak("portable");
                    }
                    ui.weak(format!("Workspace: {}", self.ws.current().name));
                    if self.jobs_running() {
                        ui.spinner().on_hover_text("Background work is running");
                    }
                });
            });
        });
        actions.into_iter().for_each(|a| self.status_action(ctx, a));
    }

    fn jobs_running(&self) -> bool {
        self.docs.values().any(|e| e.load.is_some() || e.save.is_some() || e.doc.line_index_progress().is_some())
            || self.shell.backup_jobs > 0
            || self.shell.history_jobs > 0
            || self.feat.output.has_running()
    }

    fn status_contents(&self, ui: &mut Ui, actions: &mut Vec<StatusAction>) {
        let parts = self.active_tab().and_then(|(p, i)| {
            let tab = &self.panes[p].tabs[i];
            self.docs.get(&tab.doc).filter(|e| e.load.is_none()).map(|e| (e, tab))
        });
        match parts {
            None => {
                ui.weak("Ready");
            }
            Some((entry, tab)) => {
                let doc = &entry.doc;
                match tab.view {
                    ViewKind::Text => {
                        let (line, col) = tab.editor.caret_line_col(doc);
                        let pos = match (line, doc.line_index_progress()) {
                            (Some(l), _) => format!("Ln {}, Col {}", l + 1, col + 1),
                            (None, Some(f)) => format!("indexing {:.0}%", f * 100.0),
                            (None, None) => format!("Col {}", col + 1),
                        };
                        ui.label(pos);
                        let caret = tab.editor.caret();
                        ui.separator();
                        ui.label(format!("Offset {caret} (0x{caret:X})"));
                        let sel = tab.editor.primary_range();
                        if !sel.is_empty() {
                            ui.separator();
                            ui.label(format!("Sel {}", sel.end - sel.start));
                        }
                    }
                    ViewKind::Hex => {
                        let cursor = tab.hex.cursor();
                        ui.label(format!("Offset {cursor} (0x{cursor:X})"));
                        let sel = tab.hex.selection();
                        if !sel.is_empty() {
                            ui.separator();
                            ui.label(format!("Sel {} bytes", sel.end - sel.start));
                        }
                    }
                }
                ui.separator();
                ui.label(human_size(doc.len())).on_hover_text(format!("{} bytes", doc.len()));
                ui.separator();
                match tab.view {
                    ViewKind::Text => self.text_menus(ui, entry, tab, actions),
                    ViewKind::Hex => self.hex_menus(ui, tab, actions),
                }
                if doc.is_read_only() {
                    ui.separator();
                    ui.label("RO").on_hover_text("Read-only");
                }
                if entry.tail {
                    ui.separator();
                    ui.label("TAIL").on_hover_text("Follows appended data (tail -f)");
                }
            }
        }
    }

    fn text_menus(&self, ui: &mut Ui, entry: &DocEntry, tab: &TabView, actions: &mut Vec<StatusAction>) {
        let (id, doc) = (entry.id, &entry.doc);
        let encoding = if doc.is_binary() { "binary".to_string() } else { doc.encoding().name() };
        ui.menu_button(encoding, |ui| {
            ui.menu_button("Reopen with", |ui| {
                TextEncoding::choices().into_iter().for_each(|e| {
                    if ui.button(e.name()).clicked() {
                        actions.push(StatusAction::Reopen(id, e));
                        ui.close_menu();
                    }
                });
            });
            ui.menu_button("Convert to", |ui| {
                TextEncoding::choices().into_iter().for_each(|e| {
                    if ui.radio(doc.encoding() == e, e.name()).clicked() {
                        actions.push(StatusAction::Command(kladde_panels::encoding_command(e)));
                        ui.close_menu();
                    }
                });
            });
        });
        ui.separator();
        ui.menu_button(doc.line_ending().name(), |ui| {
            [("eol.crlf", "CRLF (Windows)"), ("eol.lf", "LF (Unix)"), ("eol.cr", "CR (old Mac)")].into_iter().for_each(
                |(cmd, label)| {
                    if ui.button(format!("Convert to {label}")).clicked() {
                        actions.push(StatusAction::Command(cmd.to_string()));
                        ui.close_menu();
                    }
                },
            );
        });
        ui.separator();
        let current = tab.editor.language();
        ui.menu_button(current.name, |ui| {
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                kladde_syntax::languages().iter().for_each(|l| {
                    if ui.radio(l.id == current.id, l.name).clicked() {
                        actions.push(StatusAction::Language(l));
                        ui.close_menu();
                    }
                });
            });
        });
    }

    fn hex_menus(&self, ui: &mut Ui, tab: &TabView, actions: &mut Vec<StatusAction>) {
        let mode = match self.shell.hex_mode {
            HexMode::Hex => "Hex",
            HexMode::Binary => "Bits",
        };
        ui.menu_button(mode, |ui| {
            [(HexMode::Hex, "Hex digits"), (HexMode::Binary, "Bits (binary)")].into_iter().for_each(|(m, label)| {
                if ui.radio(self.shell.hex_mode == m, label).clicked() {
                    actions.push(StatusAction::HexMode(m));
                    ui.close_menu();
                }
            });
        });
        ui.separator();
        let columns = [(TextColumn::Ascii, "ASCII"), (TextColumn::Latin1, "Latin-1"), (TextColumn::Utf8, "UTF-8")];
        let current = columns.iter().find(|(c, _)| *c == self.shell.hex_text).map_or("ASCII", |(_, l)| l);
        ui.menu_button(current, |ui| {
            columns.into_iter().for_each(|(c, label)| {
                if ui.radio(self.shell.hex_text == c, label).clicked() {
                    actions.push(StatusAction::HexText(c));
                    ui.close_menu();
                }
            });
        })
        .response
        .on_hover_text("Text column encoding");
        ui.separator();
        let ins = if tab.hex.insert_mode() { "INS" } else { "OVR" };
        if ui.button(ins).on_hover_text("Insert key toggles insert/overwrite").clicked() {
            actions.push(StatusAction::ToggleInsert);
        }
        ui.separator();
        if ui.selectable_label(self.shell.show_inspector, "Inspector").clicked() {
            actions.push(StatusAction::ToggleInspector);
        }
    }

    fn status_action(&mut self, ctx: &egui::Context, action: StatusAction) {
        match action {
            StatusAction::Command(id) => self.execute(ctx, &id),
            StatusAction::Reopen(id, encoding) => {
                let (read_only, no_lock) =
                    self.docs.get(&id).map_or((false, false), |e| (e.open_opts.read_only, e.open_opts.no_lock));
                let opts = OpenOptions { mode: OpenMode::Text { encoding: Some(encoding) }, read_only, no_lock };
                let dirty = self.docs.get(&id).is_some_and(|e| e.doc.is_dirty());
                if dirty {
                    self.toast_info("Save or undo your changes before reopening with another encoding");
                } else {
                    self.reopen_doc(id, opts);
                }
            }
            StatusAction::Language(l) => {
                if let Some((_, tab)) = self.active_parts() {
                    tab.editor.set_language(l);
                }
            }
            StatusAction::ToggleInsert => {
                if let Some((_, tab)) = self.active_parts() {
                    let on = tab.hex.insert_mode();
                    tab.hex.set_insert_mode(!on);
                }
            }
            StatusAction::HexMode(m) => self.shell.hex_mode = m,
            StatusAction::HexText(c) => self.shell.hex_text = c,
            StatusAction::ToggleInspector => self.shell.show_inspector = !self.shell.show_inspector,
        }
    }
}
