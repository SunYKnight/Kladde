//! Window layout: workspace side panel, bottom panel and the central split with views.

use std::path::{Path, PathBuf};

use egui::{Rect, Sense, Ui, UiBuilder};
use kladde_core::{OpenMode, OpenOptions};
use kladde_editor::EditorEvent;
use kladde_hex::HexEvent;
use kladde_workspace::{NodeId, NodeKind, SplitMode, ViewKind};
use kladde_wstree::{TreeContext, TreeEvent};

use crate::commands::BottomTab;
use crate::docs::{apply_position, auto_open, tab_position};
use crate::state::{App, DiskNotice, DocId};

/// Width (or height) of the draggable split separator.
const SPLITTER: f32 = 6.0;

pub(crate) enum TabAction {
    Focus(usize),
    Close(usize),
    CloseOthers(usize),
    CopyPath(PathBuf),
    Reveal(PathBuf),
    MoveToOther(usize),
    OpenInOther(usize),
    AddToWorkspace(PathBuf),
}

enum PaneAction {
    Reload(DocId),
    IgnoreDisk(DocId),
    CloseDoc(DocId),
    CancelLoad(DocId),
}

/// Folder that contains `path` (or `path` itself when it is a folder).
fn reveal_target(path: &Path) -> PathBuf {
    match path.is_dir() {
        true => path.to_path_buf(),
        false => path.parent().map_or_else(|| path.to_path_buf(), Path::to_path_buf),
    }
}

fn hex_open() -> OpenOptions {
    OpenOptions { mode: OpenMode::Binary, read_only: false, no_lock: false }
}

impl App {
    pub(crate) fn show_side_panel(&mut self, ctx: &egui::Context) {
        self.shell.tree_id = None;
        if self.shell.side_visible {
            egui::SidePanel::left("kladde_workspace").resizable(true).default_width(250.0).show(ctx, |ui| {
                ui.strong(&self.ws.current().name);
                ui.separator();
                self.show_tree(ctx, ui);
            });
        }
    }

    fn show_tree(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let open_files = self.open_files_set();
        let dirty_files = self.dirty_files_set();
        let active_file = self.active_tab().and_then(|(p, t)| self.doc_path(self.panes[p].tabs[t].doc));
        let dark = self.dark(ctx);
        self.shell.tree_id = Some(ui.id().with("kladde_wstree"));
        let tree_ctx = TreeContext {
            active_file: active_file.as_deref(),
            open_files: &open_files,
            dirty_files: &dirty_files,
            git: (!self.shell.git.is_empty()).then_some(&self.shell.git),
            quarto_available: self.quarto.is_some(),
            dark,
        };
        let events = kladde_wstree::show(ui, self.ws.current_mut(), &mut self.shell.tree, &tree_ctx);
        events.into_iter().for_each(|ev| self.tree_event(ctx, ev));
    }

    fn tree_event(&mut self, ctx: &egui::Context, ev: TreeEvent) {
        match ev {
            TreeEvent::Open(path) => {
                self.open_path(&path, auto_open(), None);
            }
            TreeEvent::OpenHex(path) => {
                self.open_path(&path, hex_open(), Some(ViewKind::Hex));
            }
            TreeEvent::AddFiles { parent } => {
                let files = rfd::FileDialog::new().set_title("Add files to the workspace").pick_files();
                self.add_to_workspace(parent, &files.unwrap_or_default());
            }
            TreeEvent::AddFolder { parent } => {
                if let Some(dir) = rfd::FileDialog::new().set_title("Link a folder").pick_folder() {
                    self.add_to_workspace(parent, &[dir]);
                }
            }
            TreeEvent::Relocate { node } => {
                let folder = self.ws.current().node(node).is_some_and(|n| matches!(n.kind, NodeKind::Folder { .. }));
                let dialog = rfd::FileDialog::new().set_title("Relocate");
                let picked = if folder { dialog.pick_folder() } else { dialog.pick_file() };
                if let Some(p) = picked {
                    self.ws.current_mut().relocate(node, &p);
                    self.refresh_git();
                }
            }
            TreeEvent::RevealInOs(path) => self.reveal(&path),
            TreeEvent::CopyPath(path) => ctx.copy_text(path.display().to_string()),
            TreeEvent::Error(e) => self.toast_error(e),
            TreeEvent::Renamed { old, new } => {
                self.docs_renamed(&old, &new);
                self.handle_tree_action(ctx, TreeEvent::Renamed { old, new });
                self.refresh_git();
            }
            other => self.handle_tree_action(ctx, other),
        }
    }

    /// Adds files and folders (as linked folders) under `parent` of the active workspace; paths that are
    /// already in it are skipped, since a second entry for the same file is never wanted.
    pub(crate) fn add_to_workspace(&mut self, parent: NodeId, paths: &[PathBuf]) {
        let (present, new): (Vec<&PathBuf>, Vec<&PathBuf>) =
            paths.iter().partition(|p| self.ws.current().find_by_path(p).is_some());
        let results: Vec<Result<NodeId, String>> = new
            .iter()
            .map(|p| {
                let ws = self.ws.current_mut();
                let added = match p.is_dir() {
                    true => ws.add_folder(parent, p, Vec::new(), Vec::new()),
                    false => ws.add_file(parent, p),
                };
                added.map_err(|e| format!("Cannot add {}: {e}", p.display()))
            })
            .collect();
        let added = results.iter().filter(|r| r.is_ok()).count();
        results.into_iter().filter_map(Result::err).for_each(|e| self.toast_error(e));
        let name = self.ws.current().name.clone();
        match (added, new.as_slice()) {
            (0, _) => {}
            (1, [p]) => self.toast_info(format!("Added {} to workspace {name}", crate::commands::file_name(p))),
            (n, _) => self.toast_info(format!("Added {n} items to workspace {name}")),
        }
        match present.as_slice() {
            [] => {}
            [p] => self.toast_info(format!("{} is already in workspace {name}", crate::commands::file_name(p))),
            many => self.toast_info(format!("{} items are already in workspace {name}", many.len())),
        }
        if added > 0 {
            self.refresh_git();
        }
    }

    fn reveal(&mut self, path: &Path) {
        if let Err(e) = kladde_tools::open_with_os(&reveal_target(path)) {
            self.toast_error(format!("Cannot open the folder of {}: {e}", path.display()));
        }
    }

    pub(crate) fn show_bottom_panel(&mut self, ctx: &egui::Context) {
        if self.shell.bottom_visible {
            egui::TopBottomPanel::bottom("kladde_bottom").resizable(true).default_height(220.0).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let tabs = [
                        (BottomTab::Results, "Results"),
                        (BottomTab::Output, "Output"),
                        (BottomTab::History, "History"),
                        (BottomTab::Hash, "Hash"),
                    ];
                    tabs.into_iter().for_each(|(tab, label)| {
                        ui.selectable_value(&mut self.shell.bottom_tab, tab, label);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Hide panel").clicked() {
                            self.shell.bottom_visible = false;
                        }
                    });
                });
                ui.separator();
                let tab = self.shell.bottom_tab;
                self.show_bottom_tab(ui, tab);
            });
        }
    }

    pub(crate) fn show_central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::central_panel(&ctx.style()).inner_margin(0.0)).show(
            ctx,
            |ui| {
                let rect = ui.available_rect_before_wrap();
                match self.split {
                    SplitMode::None => self.pane_in(ctx, ui, 0, rect),
                    mode => {
                        let vertical = mode == SplitMode::Vertical;
                        let extent = if vertical { rect.width() } else { rect.height() };
                        let first = (extent - SPLITTER) * self.split_ratio;
                        let (r0, sep, r1) = if vertical {
                            let a = Rect::from_min_max(rect.min, egui::pos2(rect.min.x + first, rect.max.y));
                            let s = Rect::from_min_max(
                                egui::pos2(a.max.x, rect.min.y),
                                egui::pos2(a.max.x + SPLITTER, rect.max.y),
                            );
                            (a, s, Rect::from_min_max(egui::pos2(s.max.x, rect.min.y), rect.max))
                        } else {
                            let a = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + first));
                            let s = Rect::from_min_max(
                                egui::pos2(rect.min.x, a.max.y),
                                egui::pos2(rect.max.x, a.max.y + SPLITTER),
                            );
                            (a, s, Rect::from_min_max(egui::pos2(rect.min.x, s.max.y), rect.max))
                        };
                        self.pane_in(ctx, ui, 0, r0);
                        self.splitter(ui, sep, rect, vertical);
                        self.pane_in(ctx, ui, 1, r1);
                    }
                }
            },
        );
    }

    fn splitter(&mut self, ui: &mut Ui, sep: Rect, whole: Rect, vertical: bool) {
        let response = ui.interact(sep, ui.id().with("kladde_splitter"), Sense::drag());
        let icon = if vertical { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical };
        let response = response.on_hover_cursor(icon);
        if let Some(pos) = response.interact_pointer_pos().filter(|_| response.dragged()) {
            let ratio = if vertical {
                (pos.x - whole.min.x) / whole.width().max(1.0)
            } else {
                (pos.y - whole.min.y) / whole.height().max(1.0)
            };
            self.split_ratio = ratio.clamp(0.1, 0.9);
        }
        let color = if response.hovered() || response.dragged() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        };
        ui.painter().rect_filled(sep.shrink(1.0), 0.0, color);
    }

    fn pane_in(&mut self, ctx: &egui::Context, ui: &mut Ui, pane: usize, rect: Rect) {
        let clicked_here = ui.input(|i| i.pointer.any_pressed()) && ui.rect_contains_pointer(rect);
        if clicked_here {
            self.focused_pane = pane;
        }
        ui.allocate_new_ui(UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(rect);
            self.show_pane(ctx, ui, pane);
        });
    }

    fn show_pane(&mut self, ctx: &egui::Context, ui: &mut Ui, pane: usize) {
        // Pane 0's tabs sit in the workspace strip below the menu bar.
        if pane == 1 {
            let actions = self.pane_tab_bar(ui, pane);
            actions.into_iter().for_each(|a| self.tab_action(pane, a));
        }
        let current = self.panes[pane].tabs.get(self.panes[pane].active).map(|t| t.doc);
        match current {
            None => {
                ui.centered_and_justified(|ui| {
                    ui.weak("Empty pane — open a file or move a tab here");
                });
            }
            Some(id) => {
                self.file_header(ui, pane, id);
                if pane == self.focused_pane {
                    self.show_find_bar_area(ui);
                }
                let loading = self.docs.get(&id).is_some_and(|e| e.load.is_some());
                let action = if loading { self.loading_view(ui, id) } else { self.disk_bar(ui, id) };
                if !loading {
                    self.show_view(ctx, ui, pane);
                }
                if let Some(a) = action {
                    self.pane_action(a);
                }
            }
        }
    }

    /// Name of the pane's active document above its content, so every pane shows what it holds even when
    /// pane 0's tabs are folded away in the workspace strip.
    fn file_header(&self, ui: &mut Ui, pane: usize, id: DocId) {
        let path = self.doc_path(id);
        let title = match self.docs.get(&id) {
            Some(e) if e.doc.is_dirty() => format!("• {}", e.title),
            Some(e) => e.title.clone(),
            None => String::new(),
        };
        let focused = pane == self.focused_pane && self.split != SplitMode::None;
        egui::Frame::none().inner_margin(egui::Margin::symmetric(6.0, 2.0)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let name = egui::RichText::new(title);
                let response = ui.label(if focused { name.strong() } else { name });
                if let Some(dir) = path.as_deref().and_then(Path::parent) {
                    ui.weak(dir.display().to_string());
                }
                if let Some(p) = &path {
                    response.on_hover_text(p.display().to_string());
                }
            });
        });
        ui.separator();
    }

    /// Tab strip of the second pane.
    fn pane_tab_bar(&mut self, ui: &mut Ui, pane: usize) -> Vec<TabAction> {
        let mut actions = Vec::new();
        egui::Frame::none().inner_margin(egui::Margin::symmetric(4.0, 2.0)).show(ui, |ui| {
            egui::ScrollArea::horizontal().id_salt(("kladde_tabs", pane)).show(ui, |ui| {
                ui.horizontal(|ui| actions = self.tab_chips(ui, pane));
            });
        });
        ui.separator();
        actions
    }

    /// Tabs of `pane` with color accent, dirty dot, close button and context menu, laid out in `ui`.
    pub(crate) fn tab_chips(&self, ui: &mut Ui, pane: usize) -> Vec<TabAction> {
        let mut actions = Vec::new();
        let focused = pane == self.focused_pane && self.split != SplitMode::None;
        let ws = self.ws.current();
        let active = self.panes[pane].active;
        self.panes[pane].tabs.iter().enumerate().for_each(|(i, t)| {
            let entry = self.docs.get(&t.doc);
            let title = entry.map_or(String::new(), |e| e.title.clone());
            let dirty = entry.is_some_and(|e| e.doc.is_dirty());
            let path = self.doc_path(t.doc);
            let accent = path.as_deref().and_then(|p| ws.color_for_path(p)).map(crate::theme::color32);
            let text = if dirty { format!("• {title}") } else { title };
            let selected = i == active;
            let mut rich = egui::RichText::new(text);
            if selected && focused {
                rich = rich.strong();
            }
            let response = ui.selectable_label(selected, rich);
            if let Some(color) = accent {
                let r = response.rect;
                let bar = Rect::from_min_max(egui::pos2(r.min.x, r.max.y - 2.0), r.max);
                ui.painter().rect_filled(bar, 0.0, color);
            }
            let response = match &path {
                Some(p) => response.on_hover_text(p.display().to_string()),
                None => response,
            };
            if response.clicked() {
                actions.push(TabAction::Focus(i));
            }
            if response.middle_clicked() {
                actions.push(TabAction::Close(i));
            }
            response.context_menu(|ui| {
                if ui.button("Close").clicked() {
                    actions.push(TabAction::Close(i));
                    ui.close_menu();
                }
                if ui.button("Close others").clicked() {
                    actions.push(TabAction::CloseOthers(i));
                    ui.close_menu();
                }
                ui.separator();
                if let Some(p) = &path {
                    if ui.button("Copy path").clicked() {
                        actions.push(TabAction::CopyPath(p.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Reveal in file manager").clicked() {
                        actions.push(TabAction::Reveal(p.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Add to workspace").clicked() {
                        actions.push(TabAction::AddToWorkspace(p.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                }
                if ui.button("Move to other pane").clicked() {
                    actions.push(TabAction::MoveToOther(i));
                    ui.close_menu();
                }
                if ui.button("Open in other pane").clicked() {
                    actions.push(TabAction::OpenInOther(i));
                    ui.close_menu();
                }
            });
            if ui.small_button("×").on_hover_text("Close").clicked() {
                actions.push(TabAction::Close(i));
            }
            ui.add_space(6.0);
        });
        actions
    }

    pub(crate) fn tab_action(&mut self, pane: usize, action: TabAction) {
        match action {
            TabAction::Focus(i) => self.focus_tab(pane, i),
            TabAction::Close(i) => self.close_tab(pane, i),
            TabAction::CloseOthers(i) => {
                let keep = self.panes[pane].tabs.get(i).map(|t| t.doc);
                let others: Vec<DocId> =
                    self.panes[pane].tabs.iter().map(|t| t.doc).filter(|d| Some(*d) != keep).collect();
                others.into_iter().for_each(|d| {
                    if let Some(idx) = self.tab_index(pane, d) {
                        self.close_tab(pane, idx);
                    }
                });
            }
            TabAction::CopyPath(p) => self.shell.ctx.copy_text(p.display().to_string()),
            TabAction::Reveal(p) => self.reveal(&p),
            TabAction::AddToWorkspace(p) => {
                let root = self.ws.current().root.id;
                self.add_to_workspace(root, &[p]);
            }
            TabAction::MoveToOther(i) => {
                if i < self.panes[pane].tabs.len() {
                    let tab = self.panes[pane].tabs.remove(i);
                    let len = self.panes[pane].tabs.len();
                    self.panes[pane].active = self.panes[pane].active.min(len.saturating_sub(1));
                    let other = 1 - pane;
                    let existing = self.tab_index(other, tab.doc);
                    let idx = match existing {
                        Some(idx) => idx,
                        None => {
                            self.panes[other].tabs.push(tab);
                            self.panes[other].tabs.len() - 1
                        }
                    };
                    self.ensure_split();
                    self.focus_tab(other, idx);
                }
            }
            TabAction::OpenInOther(i) => {
                if let Some((id, view, pos)) = self.panes[pane].tabs.get(i).map(|t| (t.doc, t.view, tab_position(t))) {
                    let other = 1 - pane;
                    let idx = match self.tab_index(other, id) {
                        Some(idx) => idx,
                        None => {
                            let idx = self.add_tab(other, id, view);
                            if let Some(entry) = self.docs.get(&id).filter(|e| e.load.is_none()) {
                                apply_position(&mut self.panes[other].tabs[idx], &entry.doc, pos);
                            }
                            idx
                        }
                    };
                    self.ensure_split();
                    self.focus_tab(other, idx);
                }
            }
        }
    }

    fn ensure_split(&mut self) {
        if self.split == SplitMode::None {
            self.split = SplitMode::Vertical;
        }
    }

    /// Progress and Cancel while a document is opened on a worker thread.
    fn loading_view(&self, ui: &mut Ui, id: DocId) -> Option<PaneAction> {
        let entry = self.docs.get(&id)?;
        let fraction = entry.load.as_ref().map_or(0.0, |job| job.progress.fraction());
        let mut action = None;
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(format!("Opening {} …", entry.title));
            ui.add(egui::ProgressBar::new(fraction).show_percentage().desired_width(320.0));
            if ui.button("Cancel").clicked() {
                action = Some(PaneAction::CancelLoad(id));
            }
        });
        action
    }

    /// E8: "changed on disk" / "deleted" notice above the view.
    fn disk_bar(&self, ui: &mut Ui, id: DocId) -> Option<PaneAction> {
        let entry = self.docs.get(&id)?;
        let notice = entry.disk_notice?;
        let mut action = None;
        let fill = ui.visuals().warn_fg_color.gamma_multiply(0.15);
        egui::Frame::none().fill(fill).inner_margin(6.0).show(ui, |ui| {
            ui.horizontal(|ui| match notice {
                DiskNotice::Changed => {
                    let extra = if entry.doc.is_dirty() { " (reloading discards your unsaved changes)" } else { "" };
                    ui.label(format!("{} changed on disk{extra}.", entry.title));
                    if ui.button("Reload").clicked() {
                        action = Some(PaneAction::Reload(id));
                    }
                    if ui.button("Ignore").clicked() {
                        action = Some(PaneAction::IgnoreDisk(id));
                    }
                }
                DiskNotice::Deleted => {
                    ui.label(format!("{} was deleted on disk.", entry.title));
                    if ui.button("Keep").on_hover_text("Keep the text; saving recreates the file").clicked() {
                        action = Some(PaneAction::IgnoreDisk(id));
                    }
                    if ui.button("Close").clicked() {
                        action = Some(PaneAction::CloseDoc(id));
                    }
                }
            });
        });
        action
    }

    fn pane_action(&mut self, action: PaneAction) {
        match action {
            PaneAction::Reload(id) => self.reload_doc(id),
            PaneAction::IgnoreDisk(id) => self.ignore_disk_change(id),
            PaneAction::CloseDoc(id) => self.close_doc(id),
            PaneAction::CancelLoad(id) => {
                if let Some(job) = self.docs.get(&id).and_then(|e| e.load.as_ref()) {
                    job.progress.cancel();
                }
                self.remove_doc(id);
            }
        }
    }

    /// Editor, hex view (with optional inspector) or CSV table of the pane's active tab.
    fn show_view(&mut self, ctx: &egui::Context, ui: &mut Ui, pane: usize) {
        let idx = self.panes[pane].active;
        if let Some((id, view, csv)) = self.panes[pane].tabs.get(idx).map(|t| (t.doc, t.view, t.csv_mode)) {
            let widget_id = egui::Id::new(("kladde_view", pane, id));
            if self.shell.focus_view && pane == self.focused_pane {
                self.shell.focus_view = false;
                ctx.memory_mut(|m| m.request_focus(widget_id));
            }
            let mut editor_events: Vec<EditorEvent> = Vec::new();
            let mut hex_errors: Vec<String> = Vec::new();
            let mut focused = false;
            match (csv, view) {
                (true, _) => self.show_csv_view(ui, id),
                (false, ViewKind::Text) => {
                    let (cfg, theme) = (self.editor_config(), self.editor_theme(ctx));
                    if let (Some(entry), Some(tab)) = (self.docs.get_mut(&id), self.panes[pane].tabs.get_mut(idx)) {
                        let out = kladde_editor::show(ui, widget_id, &mut entry.doc, &mut tab.editor, &cfg, &theme);
                        editor_events = out.events;
                        focused = out.focused;
                    }
                }
                (false, ViewKind::Hex) => {
                    let (cfg, theme) = (self.hex_config(), self.hex_theme(ctx));
                    let inspector = self.shell.show_inspector;
                    let big_endian = &mut self.shell.inspector_big_endian;
                    if let (Some(entry), Some(tab)) = (self.docs.get_mut(&id), self.panes[pane].tabs.get_mut(idx)) {
                        if inspector {
                            egui::SidePanel::right(egui::Id::new(("kladde_inspector", pane)))
                                .resizable(true)
                                .default_width(240.0)
                                .show_inside(ui, |ui| {
                                    ui.heading("Inspector");
                                    kladde_hex::show_inspector(ui, &entry.doc, &tab.hex, big_endian);
                                });
                        }
                        let out = kladde_hex::show(ui, widget_id, &mut entry.doc, &mut tab.hex, &cfg, &theme);
                        hex_errors = out.events.into_iter().map(|HexEvent::Error(e)| e).collect();
                        focused = out.focused;
                    }
                }
            }
            if focused {
                self.shell.editor_focused = true;
                self.focused_pane = pane;
            }
            hex_errors.into_iter().for_each(|e| self.toast_error(e));
            if !editor_events.is_empty() {
                self.handle_editor_events(ctx, editor_events);
            }
        }
    }
}
