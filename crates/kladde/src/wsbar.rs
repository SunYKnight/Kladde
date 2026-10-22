//! Workspace strip below the menu bar: every workspace as a tab group, folded to `▸ Name` or unfolded to
//! `▾ Name` followed by its file tabs (W8), plus the name, delete and file drop dialogs.

use std::path::PathBuf;

use egui::{Color32, Rect, RichText, Sense, Ui};

use crate::layout::TabAction;
use crate::state::App;

/// Dialog waiting for input about a workspace.
pub enum WsPrompt {
    /// Name of a new workspace (`rename: None`) or the new name of workspace `rename`.
    Name {
        rename: Option<usize>,
        input: String,
        focus: bool,
    },
    Delete(usize),
    /// Files or folders dropped on the window; a drop carries no position, so the user picks the target.
    Drop(Vec<PathBuf>),
}

impl WsPrompt {
    pub fn create() -> WsPrompt {
        WsPrompt::Name { rename: None, input: String::new(), focus: true }
    }

    pub fn rename(i: usize, current: &str) -> WsPrompt {
        WsPrompt::Name { rename: Some(i), input: current.to_string(), focus: true }
    }
}

enum StripAction {
    /// Header click: switch to an inactive workspace, fold or unfold the active one.
    Header(usize),
    /// Stored tab `tab` of inactive workspace `ws`.
    StoredTab {
        ws: usize,
        tab: usize,
    },
    Tab(TabAction),
    Prompt(WsPrompt),
}

/// Right-pointing (folded) or down-pointing (unfolded) triangle; egui's default fonts lack these glyphs.
fn paint_fold_arrow(ui: &Ui, rect: Rect, folded: bool, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.35;
    let points = if folded {
        vec![c + egui::vec2(-r * 0.6, -r), c + egui::vec2(-r * 0.6, r), c + egui::vec2(r * 0.8, 0.0)]
    } else {
        vec![c + egui::vec2(-r, -r * 0.6), c + egui::vec2(r, -r * 0.6), c + egui::vec2(0.0, r * 0.8)]
    };
    ui.painter().add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
}

impl App {
    pub(crate) fn show_workspace_strip(&mut self, ctx: &egui::Context) {
        let mut actions: Vec<StripAction> = Vec::new();
        egui::TopBottomPanel::top("kladde_ws_strip").show(ctx, |ui| {
            egui::ScrollArea::horizontal().id_salt("kladde_ws_strip_scroll").show(ui, |ui| {
                ui.horizontal(|ui| {
                    (0..self.ws.workspaces.len()).for_each(|i| self.workspace_group(ui, i, &mut actions));
                    if ui.small_button("+").on_hover_text("New workspace").clicked() {
                        actions.push(StripAction::Prompt(WsPrompt::create()));
                    }
                });
            });
        });
        actions.into_iter().for_each(|a| self.strip_action(a));
    }

    /// Header of workspace `i` and, when unfolded, its tabs: live ones for the active workspace (first pane),
    /// stored ones for the others.
    fn workspace_group(&self, ui: &mut Ui, i: usize, actions: &mut Vec<StripAction>) {
        let ws = &self.ws.workspaces[i];
        let active = i == self.ws.active;
        let height = ui.spacing().interact_size.y;
        let (arrow_rect, arrow) = ui.allocate_exact_size(egui::vec2(height * 0.5, height), Sense::click());
        let mut label = RichText::new(&ws.name).strong();
        if !active {
            label = label.weak();
        }
        let response = ui.selectable_label(active && ws.folded, label);
        let arrow_color = if active { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
        paint_fold_arrow(ui, arrow_rect, ws.folded, arrow_color);
        let response = response.union(arrow);
        if let Some(color) = ws.root.color.map(crate::theme::color32) {
            let r = response.rect;
            ui.painter().rect_filled(Rect::from_min_max(egui::pos2(r.min.x, r.max.y - 2.0), r.max), 0.0, color);
        }
        let hint = match (active, ws.folded) {
            (true, true) => "Show the tabs of this workspace",
            (true, false) => "Fold this workspace to its name",
            (false, _) => "Switch to this workspace",
        };
        let response = response.on_hover_text(hint);
        if response.clicked() {
            actions.push(StripAction::Header(i));
        }
        response.context_menu(|ui| {
            if ui.button("Rename…").clicked() {
                actions.push(StripAction::Prompt(WsPrompt::rename(i, &ws.name)));
                ui.close_menu();
            }
            let deletable = self.ws.workspaces.len() > 1;
            let delete = ui.add_enabled(deletable, egui::Button::new("Delete…"));
            if delete.on_disabled_hover_text("The last workspace cannot be deleted").clicked() {
                actions.push(StripAction::Prompt(WsPrompt::Delete(i)));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("New workspace…").clicked() {
                actions.push(StripAction::Prompt(WsPrompt::create()));
                ui.close_menu();
            }
        });
        if !ws.folded {
            match active {
                true => actions.extend(self.tab_chips(ui, 0).into_iter().map(StripAction::Tab)),
                false => ws.session.tabs.iter().enumerate().for_each(|(tab, t)| {
                    let response = ui.selectable_label(false, RichText::new(&t.title).weak());
                    let response = match &t.path {
                        Some(p) => response.on_hover_text(p.display().to_string()),
                        None => response,
                    };
                    if response.clicked() {
                        actions.push(StripAction::StoredTab { ws: i, tab });
                    }
                }),
            }
        }
        ui.separator();
    }

    fn strip_action(&mut self, action: StripAction) {
        match action {
            StripAction::Header(i) if i == self.ws.active => {
                let ws = self.ws.current_mut();
                ws.set_folded(!ws.folded);
            }
            StripAction::Header(i) => self.switch_workspace(i),
            StripAction::StoredTab { ws, tab } => self.switch_workspace_to_tab(ws, tab),
            StripAction::Tab(a) => self.tab_action(0, a),
            StripAction::Prompt(p) => self.shell.ws_prompt = Some(p),
        }
    }

    /// Name dialog for new/renamed workspaces, the delete confirmation and the drop choice.
    pub(crate) fn show_ws_prompt(&mut self, ctx: &egui::Context) {
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut done: Option<bool> = esc.then_some(false);
        let mut drop_choice = (false, false);
        match self.shell.ws_prompt.as_mut() {
            Some(WsPrompt::Name { rename, input, focus }) => {
                let title = if rename.is_some() { "Rename workspace" } else { "New workspace" };
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
                    .show(ctx, |ui| {
                        let edit = ui.add(egui::TextEdit::singleline(input).hint_text("Name").desired_width(260.0));
                        if std::mem::take(focus) {
                            edit.request_focus();
                        }
                        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            if ui.button(if rename.is_some() { "Rename" } else { "Create" }).clicked() || enter {
                                done = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                done = Some(false);
                            }
                        });
                    });
            }
            Some(WsPrompt::Delete(i)) => {
                let ws = self.ws.workspaces.get(*i);
                let name = ws.map_or(String::new(), |w| w.name.clone());
                let unsaved = match *i == self.ws.active {
                    true => self.docs.values().filter(|e| e.doc.is_dirty()).count(),
                    false => ws.map_or(0, |w| w.session.tabs.iter().filter(|t| t.backup.is_some()).count()),
                };
                let unsaved = match unsaved {
                    0 => String::new(),
                    n => format!(" Unsaved changes in {n} of its documents are discarded."),
                };
                egui::Window::new("Delete workspace")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!(
                            "Delete the workspace {name} with its tree and tab list? Files on disk stay.{unsaved}"
                        ));
                        ui.horizontal(|ui| {
                            if ui.button("Delete").clicked() {
                                done = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                done = Some(false);
                            }
                        });
                    });
            }
            Some(WsPrompt::Drop(paths)) => {
                let files = paths.iter().filter(|p| !p.is_dir()).count();
                let what = match (paths.len(), files) {
                    (1, 1) => format!("File {}", crate::commands::file_name(&paths[0])),
                    (1, _) => format!("Folder {}", crate::commands::file_name(&paths[0])),
                    (n, _) => format!("{n} items"),
                };
                let ws = &self.ws.current().name;
                egui::Window::new("Dropped files")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!("{what}: open, or add to workspace {ws}?"));
                        if files < paths.len() {
                            ui.weak("Folders are added as linked folders; only files open as tabs.");
                        }
                        ui.horizontal(|ui| {
                            let choices =
                                [("Open", true, false), ("Add to workspace", false, true), ("Both", true, true)];
                            choices.into_iter().for_each(|(label, open, add)| {
                                if ui.add_enabled(add || files > 0, egui::Button::new(label)).clicked() {
                                    drop_choice = (open, add);
                                    done = Some(true);
                                }
                            });
                            if ui.button("Cancel").clicked() {
                                done = Some(false);
                            }
                        });
                    });
            }
            None => done = None,
        }
        if let Some(confirmed) = done {
            match (confirmed, self.shell.ws_prompt.take()) {
                (true, Some(WsPrompt::Name { rename: None, input, .. })) => self.new_workspace(&input),
                (true, Some(WsPrompt::Name { rename: Some(i), input, .. })) => self.rename_workspace(i, &input),
                (true, Some(WsPrompt::Delete(i))) => self.delete_workspace(i),
                (true, Some(WsPrompt::Drop(paths))) => self.dropped_choice(&paths, drop_choice),
                _ => {}
            }
        }
    }

    /// Opens the dropped files and/or adds files and folders to the active workspace.
    fn dropped_choice(&mut self, paths: &[PathBuf], (open, add): (bool, bool)) {
        if add {
            let root = self.ws.current().root.id;
            self.add_to_workspace(root, paths);
        }
        if open {
            paths.iter().filter(|p| !p.is_dir()).for_each(|p| {
                self.open_path(p, crate::docs::auto_open(), None);
            });
        }
    }
}
