//! Menu bar: commands grouped by category with their key chords; unavailable entries say why on hover.

use std::path::{Path, PathBuf};

use egui::Button;
use kladde_core::TextEncoding;
use kladde_panels::commands;

use crate::commands::{classify, command_label};
use crate::state::{App, DocId};

enum MenuAction {
    Command(&'static str),
    OpenFile(PathBuf),
    Reopen(DocId, TextEncoding),
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl App {
    fn command_item(&self, ui: &mut egui::Ui, id: &'static str, chosen: &mut Option<MenuAction>) {
        let cmd = classify(id);
        let reason = cmd.and_then(|c| self.unavailable(c));
        let label = match cmd.and_then(|c| self.checked(c)) {
            Some(true) => format!("✔ {}", command_label(id)),
            _ => command_label(id).to_string(),
        };
        let chord = self.keymap.chord_for(id).map(|c| c.to_string()).unwrap_or_default();
        let response = ui.add_enabled(reason.is_none(), Button::new(label).shortcut_text(chord));
        let response = match reason {
            Some(r) => response.on_disabled_hover_text(r),
            None => response,
        };
        if response.clicked() {
            *chosen = Some(MenuAction::Command(id));
            ui.close_menu();
        }
    }

    /// Every command of `category`, except `skip`.
    fn category_items(&self, ui: &mut egui::Ui, category: &str, skip: &[&str], chosen: &mut Option<MenuAction>) {
        commands()
            .iter()
            .filter(|c| c.category == category && !skip.contains(&c.id))
            .for_each(|c| self.command_item(ui, c.id, chosen));
    }

    fn recent_menu(&self, ui: &mut egui::Ui, chosen: &mut Option<MenuAction>) {
        let files = &self.settings.recent_files;
        ui.add_enabled_ui(!files.is_empty(), |ui| {
            ui.menu_button("Recent files", |ui| {
                files.iter().for_each(|p| {
                    if ui.button(file_label(p)).on_hover_text(p.display().to_string()).clicked() {
                        *chosen = Some(MenuAction::OpenFile(p.clone()));
                        ui.close_menu();
                    }
                });
            })
            .response
            .on_disabled_hover_text("No recent files");
        });
    }

    fn encoding_menu(&self, ui: &mut egui::Ui, chosen: &mut Option<MenuAction>) {
        ui.menu_button("Convert to (on save)", |ui| self.category_items(ui, "Encoding", &[], chosen));
        let target = self
            .focused_view()
            .filter(|(e, _)| e.load.is_none() && e.doc.path().is_some())
            .map(|(e, _)| (e.id, e.doc.encoding()));
        ui.add_enabled_ui(target.is_some(), |ui| {
            ui.menu_button("Reopen with", |ui| {
                if let Some((id, current)) = target {
                    TextEncoding::choices().into_iter().for_each(|enc| {
                        let mark = if enc == current { "✔ " } else { "" };
                        if ui.button(format!("{mark}{}", enc.name())).clicked() {
                            *chosen = Some(MenuAction::Reopen(id, enc));
                            ui.close_menu();
                        }
                    });
                }
            })
            .response
            .on_disabled_hover_text("Needs a document opened from a file");
        });
        ui.separator();
        self.category_items(ui, "Line endings", &[], chosen);
    }

    pub fn show_menu_bar(&mut self, ui: &mut egui::Ui) {
        let mut chosen = None;
        let c = &mut chosen;
        ui.menu_button("File", |ui| {
            self.category_items(ui, "File", &[], c);
            self.recent_menu(ui, c);
            ui.separator();
            self.category_items(ui, "App", &[], c);
        });
        ui.menu_button("Edit", |ui| {
            self.category_items(ui, "Edit", &[], c);
            ui.separator();
            ui.menu_button("Lines", |ui| self.category_items(ui, "Lines", &[], c));
            ui.menu_button("Convert selection", |ui| self.category_items(ui, "Tools", &["tools.hash"], c));
        });
        ui.menu_button("Search", |ui| {
            self.category_items(ui, "Search", &[], c);
            ui.separator();
            self.category_items(ui, "Bookmarks", &[], c);
        });
        ui.menu_button("View", |ui| {
            self.category_items(ui, "View", &[], c);
            ui.separator();
            self.category_items(ui, "Palette", &[], c);
        });
        ui.menu_button("Encoding", |ui| self.encoding_menu(ui, c));
        ui.menu_button("Workspace", |ui| self.category_items(ui, "Workspace", &[], c));
        ui.menu_button("Run", |ui| {
            self.category_items(ui, "Run", &[], c);
            ui.separator();
            self.category_items(ui, "Quarto", &[], c);
        });
        ui.menu_button("Tools", |ui| {
            self.command_item(ui, "tools.hash", c);
            ui.separator();
            self.category_items(ui, "History", &[], c);
            ui.separator();
            self.category_items(ui, "Macro", &[], c);
            ui.separator();
            self.category_items(ui, "Settings", &[], c);
        });
        ui.menu_button("Help", |ui| {
            self.category_items(ui, "Help", &[], c);
            ui.separator();
            ui.weak(format!("Kladde {}", env!("CARGO_PKG_VERSION")));
        });
        let ctx = ui.ctx().clone();
        match chosen {
            Some(MenuAction::Command(id)) => self.execute(&ctx, id),
            Some(MenuAction::OpenFile(path)) => {
                self.open_path(&path, crate::docs::auto_open(), None);
            }
            Some(MenuAction::Reopen(id, enc)) => self.reopen_with(id, enc),
            None => {}
        }
    }
}
