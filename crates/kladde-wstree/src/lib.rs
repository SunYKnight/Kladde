//! Workspace tree panel: groups, files and linked folders of a [`Workspace`] with folding, colors, renaming,
//! drag & drop and a fuzzy filter.

mod dnd;
mod rows;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::color_picker::{color_picker_color32, Alpha};
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat, TextWrapping};
use egui::{
    pos2, vec2, Align, Align2, Button, Color32, CursorIcon, DragAndDrop, FontId, Id, Key, Layout, Modifiers, Painter,
    Pos2, Rect, Response, ScrollArea, Sense, Shape, Stroke, TextEdit, TextStyle, Ui, UiBuilder,
};
use kladde_tools::{can_render, GitStatus, Interpreter};
use kladde_workspace::{rename_path_on_disk, Node, NodeId, Rgb, Workspace, PALETTE};

use dnd::{drop_position, drop_target, DropPos};
use rows::{Lister, Listing, Row, RowKey, RowKind};

/// Missing-file checks stat every node, so they run on a timer rather than per frame.
const MISSING_RECHECK_SECS: f64 = 2.0;
const INDENT: f32 = 14.0;
const STRIPE: f32 = 3.0;
const ICON_W: f32 = 18.0;
const GIT_W: f32 = 16.0;
const DIRTY_W: f32 = 12.0;
const SWATCHES_PER_LINE: f32 = 6.0;
const NEW_GROUP_NAME: &str = "New group";
#[cfg(windows)]
const REVEAL_LABEL: &str = "Reveal in Explorer";
#[cfg(not(windows))]
const REVEAL_LABEL: &str = "Reveal in file manager";

/// Editor state the tree reflects.
pub struct TreeContext<'a> {
    pub active_file: Option<&'a Path>,
    pub open_files: &'a HashSet<PathBuf>,
    pub dirty_files: &'a HashSet<PathBuf>,
    pub git: Option<&'a HashMap<PathBuf, GitStatus>>,
    pub quarto_available: bool,
    pub dark: bool,
}

/// Requests for the application; workspace edits (alias, color, fold, move, remove, rename) are already applied.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeEvent {
    Open(PathBuf),
    OpenHex(PathBuf),
    AddFiles { parent: NodeId },
    AddFolder { parent: NodeId },
    Relocate { node: NodeId },
    Renamed { old: PathBuf, new: PathBuf },
    RevealInOs(PathBuf),
    RunFile(PathBuf),
    QuartoRender(PathBuf),
    ShowHistory(PathBuf),
    CopyPath(PathBuf),
    Error(String),
}

struct RenameEdit {
    key: RowKey,
    on_disk: bool,
    text: String,
    started: bool,
}

/// Per-panel UI state: selection, filter, inline rename, linked-folder listings and their open subfolders.
pub struct TreeState {
    selected: Option<RowKey>,
    filter: String,
    rename: Option<RenameEdit>,
    rename_requested: bool,
    listings: HashMap<PathBuf, Listing>,
    open_dirs: HashSet<PathBuf>,
    missing: HashSet<NodeId>,
    missing_checked: Option<f64>,
}

impl TreeState {
    pub fn new() -> TreeState {
        TreeState {
            selected: None,
            filter: String::new(),
            rename: None,
            rename_requested: false,
            listings: HashMap::new(),
            open_dirs: HashSet::new(),
            missing: HashSet::new(),
            missing_checked: None,
        }
    }

    /// Starts inline renaming of the selection with the next `show`: the alias of workspace nodes, the name on
    /// disk of entries inside linked folders.
    pub fn start_rename_selected(&mut self) {
        self.rename_requested = true;
    }
}

impl Default for TreeState {
    fn default() -> Self {
        TreeState::new()
    }
}

enum Action {
    Select(RowKey),
    Toggle(RowKey),
    Emit(TreeEvent),
    StartRename { key: RowKey, on_disk: bool },
    CommitRename,
    CancelRename,
    SetColor(NodeId, Option<Rgb>),
    NewGroup(NodeId),
    Remove(NodeId),
    Refresh(PathBuf),
    RefreshAll,
    Move { node: NodeId, parent: NodeId, index: usize, expand: bool },
}

struct TreeTheme {
    missing: Color32,
    dirty: Color32,
    active_bg: Color32,
    drop: Color32,
    modified: Color32,
    added: Color32,
    deleted: Color32,
    renamed: Color32,
    untracked: Color32,
    conflicted: Color32,
}

impl TreeTheme {
    fn dark() -> Self {
        TreeTheme {
            missing: Color32::from_rgb(235, 95, 95),
            dirty: Color32::from_rgb(235, 175, 70),
            active_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 16),
            drop: Color32::from_rgb(90, 160, 255),
            modified: Color32::from_rgb(225, 175, 70),
            added: Color32::from_rgb(110, 200, 110),
            deleted: Color32::from_rgb(235, 95, 95),
            renamed: Color32::from_rgb(100, 160, 235),
            untracked: Color32::from_rgb(150, 150, 150),
            conflicted: Color32::from_rgb(240, 90, 200),
        }
    }

    fn light() -> Self {
        TreeTheme {
            missing: Color32::from_rgb(190, 30, 30),
            dirty: Color32::from_rgb(200, 120, 0),
            active_bg: Color32::from_rgba_unmultiplied(0, 0, 0, 14),
            drop: Color32::from_rgb(20, 100, 220),
            modified: Color32::from_rgb(180, 120, 0),
            added: Color32::from_rgb(30, 140, 30),
            deleted: Color32::from_rgb(190, 30, 30),
            renamed: Color32::from_rgb(20, 100, 200),
            untracked: Color32::from_rgb(110, 110, 110),
            conflicted: Color32::from_rgb(190, 30, 150),
        }
    }

    fn git(&self, status: GitStatus) -> Color32 {
        match status {
            GitStatus::Modified => self.modified,
            GitStatus::Added => self.added,
            GitStatus::Deleted => self.deleted,
            GitStatus::Renamed => self.renamed,
            GitStatus::Untracked => self.untracked,
            GitStatus::Conflicted => self.conflicted,
        }
    }
}

/// Shows the tree of `ws` and applies the user's workspace edits directly.
pub fn show(ui: &mut Ui, ws: &mut Workspace, state: &mut TreeState, ctx: &TreeContext) -> Vec<TreeEvent> {
    let tree_id = ui.id().with("kladde_wstree");
    let mut events = Vec::new();
    let mut actions = toolbar(ui, state);
    actions.extend(keyboard(ui, tree_id, state));
    apply_all(ws, state, actions, &mut events);
    refresh_missing(ui, ws, state);
    let rows = collect_rows(ws, state, &mut events);
    let actions = show_tree(ui, tree_id, ws, state, ctx, &rows);
    apply_all(ws, state, actions, &mut events);
    events
}

fn toolbar(ui: &mut Ui, state: &mut TreeState) -> Vec<Action> {
    let size = vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), |ui| {
        let refresh = ui.button("🔄").on_hover_text("Refresh linked folders").clicked();
        let clear = !state.filter.is_empty() && ui.button("✖").on_hover_text("Clear filter").clicked();
        let edit = TextEdit::singleline(&mut state.filter).hint_text("Filter").desired_width(ui.available_width());
        let edit = ui.add(edit);
        if clear || (edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape))) {
            state.filter.clear();
        }
        refresh.then_some(Action::RefreshAll).into_iter().collect()
    })
    .inner
}

fn keyboard(ui: &Ui, tree_id: Id, state: &mut TreeState) -> Vec<Action> {
    let focused = ui.memory(|m| m.has_focus(tree_id)) && state.rename.is_none();
    let f2 = focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2));
    let delete = focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete));
    let requested = std::mem::take(&mut state.rename_requested) || f2;
    let rename = state
        .selected
        .clone()
        .filter(|_| requested)
        .map(|key| Action::StartRename { on_disk: !matches!(key, RowKey::Node(_)), key });
    let remove = match (&state.selected, delete) {
        (Some(RowKey::Node(id)), true) => Some(Action::Remove(*id)),
        _ => None,
    };
    rename.into_iter().chain(remove).collect()
}

fn refresh_missing(ui: &Ui, ws: &Workspace, state: &mut TreeState) {
    let now = ui.input(|i| i.time);
    if state.missing_checked.filter(|t| now - t < MISSING_RECHECK_SECS).is_none() {
        state.missing = ws.missing().into_iter().collect();
        state.missing_checked = Some(now);
    }
}

fn collect_rows(ws: &Workspace, state: &mut TreeState, events: &mut Vec<TreeEvent>) -> Vec<Row> {
    let pattern = state.filter.trim();
    let lister = Lister {
        listings: &mut state.listings,
        open_dirs: &state.open_dirs,
        missing: &state.missing,
        events,
        all: !pattern.is_empty(),
        rows: Vec::new(),
    };
    let rows = rows::collect(ws, lister);
    if pattern.is_empty() {
        rows
    } else {
        rows::apply_filter(rows, pattern)
    }
}

struct View<'a> {
    ws: &'a Workspace,
    ctx: &'a TreeContext<'a>,
    theme: TreeTheme,
    tree_id: Id,
    row_h: f32,
}

struct RowsOut {
    actions: Vec<Action>,
    pointer_on_row: bool,
}

fn show_tree(
    ui: &mut Ui,
    tree_id: Id,
    ws: &Workspace,
    state: &mut TreeState,
    ctx: &TreeContext,
    rows: &[Row],
) -> Vec<Action> {
    // Registered before the rows so rows win hits; it carries keyboard focus, root drops and the root menu.
    let background = ui.interact(ui.available_rect_before_wrap(), tree_id, Sense::click());
    let view = View {
        ws,
        ctx,
        theme: if ctx.dark { TreeTheme::dark() } else { TreeTheme::light() },
        tree_id,
        row_h: ui.text_style_height(&TextStyle::Body) + 6.0,
    };
    let mut out = RowsOut { actions: Vec::new(), pointer_on_row: false };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ScrollArea::vertical().auto_shrink([false, false]).drag_to_scroll(false).show_rows(
            ui,
            view.row_h,
            rows.len(),
            |ui, range| {
                rows[range]
                    .iter()
                    .for_each(|row| show_row(ui, &view, row, &state.selected, &mut state.rename, &mut out));
            },
        );
    });
    let root = ws.root.id;
    if background.clicked() {
        out.actions.push(Action::CancelRename);
        ui.memory_mut(|m| m.request_focus(tree_id));
    }
    let released = background.dnd_release_payload::<NodeId>().filter(|_| !out.pointer_on_row);
    if let Some((node, (parent, index))) = released.and_then(|n| Some((*n, drop_target(ws, *n, root, DropPos::Into)?)))
    {
        out.actions.push(Action::Move { node, parent, index, expand: false });
    }
    background.context_menu(|ui| add_items(ui, root, &mut out.actions));
    drag_preview(ui, ws, tree_id);
    out.actions
}

fn drag_preview(ui: &Ui, ws: &Workspace, tree_id: Id) {
    let dragged = DragAndDrop::payload::<NodeId>(ui.ctx()).and_then(|id| ws.node(*id));
    if let Some(node) = dragged {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), tree_id.with("drag"), |ui| {
            ui.label(node.display_name())
        });
    }
}

fn show_row(
    ui: &mut Ui,
    view: &View,
    row: &Row,
    selected: &Option<RowKey>,
    rename: &mut Option<RenameEdit>,
    out: &mut RowsOut,
) {
    let (_, rect) = ui.allocate_space(vec2(ui.available_width(), view.row_h));
    let response = ui.interact(rect, view.tree_id.with(&row.key), Sense::click_and_drag());
    out.pointer_on_row |= response.contains_pointer();
    let indent_x = rect.left() + STRIPE + row.depth as f32 * INDENT;
    let arrow = Rect::from_min_size(pos2(indent_x, rect.top()), vec2(INDENT, rect.height()));
    let label_x = arrow.right() + ICON_W + 2.0;
    handle_clicks(ui, view, row, &response, arrow, out);
    paint_row(ui, view, row, &response, selected.as_ref() == Some(&row.key), arrow);
    match rename.as_mut().filter(|edit| edit.key == row.key) {
        Some(edit) => {
            let edit_rect =
                Rect::from_min_max(pos2(label_x, rect.top() + 1.0), pos2(rect.right() - 2.0, rect.bottom() - 1.0));
            rename_editor(ui, view, edit, edit_rect, out);
        }
        None => paint_label(ui, view, row, rect, label_x),
    }
    if let RowKey::Node(id) = row.key {
        response.dnd_set_drag_payload(id);
        drop_zone(ui, view, row, id, &response, indent_x, out);
    }
    if row.kind != RowKind::Note {
        let tip = row.path.as_ref().map(|p| p.display().to_string());
        let response = match tip {
            Some(tip) => response.on_hover_text(tip),
            None => response,
        };
        response.context_menu(|ui| row_menu(ui, view, row, &mut out.actions));
    }
}

fn handle_clicks(ui: &Ui, view: &View, row: &Row, response: &Response, arrow: Rect, out: &mut RowsOut) {
    let on_arrow = response.interact_pointer_pos().is_some_and(|p| arrow.contains(p));
    if response.clicked() || response.secondary_clicked() {
        out.actions.push(Action::Select(row.key.clone()));
        ui.memory_mut(|m| m.request_focus(view.tree_id));
    }
    if response.clicked() && on_arrow && row.kind.expandable() {
        out.actions.push(Action::Toggle(row.key.clone()));
    }
    if response.double_clicked() && !on_arrow {
        let action = match (&row.key, &row.path) {
            (key, _) if row.kind.expandable() => Some(Action::Toggle(key.clone())),
            (RowKey::Node(id), _) if row.missing => Some(Action::Emit(TreeEvent::Relocate { node: *id })),
            (_, Some(path)) if row.kind.is_file() => Some(Action::Emit(TreeEvent::Open(path.clone()))),
            _ => None,
        };
        out.actions.extend(action);
    }
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

fn icon(row: &Row) -> &'static str {
    match row.kind {
        RowKind::Group if row.expanded => "🗁",
        RowKind::Group => "🗀",
        RowKind::Folder => "🔗",
        RowKind::Dir if row.expanded => "📂",
        RowKind::Dir => "📁",
        RowKind::File | RowKind::EntryFile => "📄",
        RowKind::Note => "⚠",
    }
}

fn git_status_for(git: &HashMap<PathBuf, GitStatus>, path: &Path) -> Option<GitStatus> {
    // Git reports an untracked folder once, not each file inside it.
    let untracked_parent =
        || path.ancestors().skip(1).find_map(|a| git.get(a).copied().filter(|s| *s == GitStatus::Untracked));
    git.get(path).copied().or_else(untracked_parent)
}

fn git_letter(status: GitStatus) -> &'static str {
    match status {
        GitStatus::Modified => "M",
        GitStatus::Added => "A",
        GitStatus::Deleted => "D",
        GitStatus::Renamed => "R",
        GitStatus::Untracked => "U",
        GitStatus::Conflicted => "!",
    }
}

fn paint_arrow(painter: &Painter, c: Pos2, expanded: bool, color: Color32) {
    let s = 4.0;
    let points = if expanded {
        vec![pos2(c.x - s, c.y - s * 0.5), pos2(c.x + s, c.y - s * 0.5), pos2(c.x, c.y + s * 0.7)]
    } else {
        vec![pos2(c.x - s * 0.5, c.y - s), pos2(c.x + s * 0.7, c.y), pos2(c.x - s * 0.5, c.y + s)]
    };
    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
}

fn paint_row(ui: &Ui, view: &View, row: &Row, response: &Response, selected: bool, arrow: Rect) {
    let painter = ui.painter();
    let visuals = ui.visuals();
    let rect = response.rect;
    let active = row.kind.is_file() && view.ctx.active_file.is_some_and(|a| row.path.as_deref() == Some(a));
    let background = if selected {
        Some(visuals.selection.bg_fill)
    } else if active {
        Some(view.theme.active_bg)
    } else {
        response.hovered().then_some(visuals.widgets.hovered.weak_bg_fill)
    };
    if let Some(fill) = background {
        painter.rect_filled(rect, 0.0, fill);
    }
    if let Some(c) = row.color {
        painter.rect_filled(Rect::from_min_size(rect.min, vec2(STRIPE, rect.height())), 0.0, rgb(c));
    }
    if row.kind.expandable() {
        paint_arrow(painter, arrow.center(), row.expanded, visuals.weak_text_color());
    }
    let font = TextStyle::Body.resolve(ui.style());
    let tint = match row.kind {
        RowKind::Note => view.theme.missing,
        _ => row.color.map_or(visuals.weak_text_color(), rgb),
    };
    let icon_pos = pos2(arrow.right() + ICON_W / 2.0, rect.center().y);
    painter.text(icon_pos, Align2::CENTER_CENTER, icon(row), font.clone(), tint);
    let status = view.ctx.git.zip(row.path.as_deref()).and_then(|(git, p)| git_status_for(git, p));
    if let Some(status) = status {
        let pos = pos2(rect.right() - 4.0, rect.center().y);
        let mono = FontId::monospace(font.size);
        painter.text(pos, Align2::RIGHT_CENTER, git_letter(status), mono, view.theme.git(status));
    }
}

fn paint_label(ui: &Ui, view: &View, row: &Row, rect: Rect, label_x: f32) {
    let path = row.path.as_deref();
    let open = path.is_some_and(|p| view.ctx.open_files.contains(p));
    let dirty = path.is_some_and(|p| view.ctx.dirty_files.contains(p));
    let visuals = ui.visuals();
    let color = if row.missing || row.kind == RowKind::Note {
        view.theme.missing
    } else if open {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    let strike = if row.missing { Stroke::new(1.0_f32, color) } else { Stroke::NONE };
    let format =
        TextFormat { font_id: TextStyle::Body.resolve(ui.style()), color, strikethrough: strike, ..Default::default() };
    let mut job = LayoutJob::single_section(row.label.clone(), format);
    let right = rect.right() - GIT_W - if dirty { DIRTY_W } else { 0.0 };
    job.wrap = TextWrapping::truncate_at_width((right - label_x).max(0.0));
    let galley = ui.fonts(|f| f.layout_job(job));
    let pos = pos2(label_x, rect.center().y - galley.size().y / 2.0);
    let width = galley.size().x;
    let painter = ui.painter();
    if open {
        // Default egui fonts have no bold face; a doubled glyph run reads as bold.
        painter.galley(pos + vec2(0.6, 0.0), galley.clone(), color);
    }
    painter.galley(pos, galley, color);
    if dirty {
        painter.circle_filled(pos2(label_x + width + DIRTY_W / 2.0 + 1.0, rect.center().y), 3.0, view.theme.dirty);
    }
}

fn stem_chars(name: &str) -> usize {
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[..dot].chars().count(),
        _ => name.chars().count(),
    }
}

fn rename_editor(ui: &mut Ui, view: &View, edit: &mut RenameEdit, rect: Rect, out: &mut RowsOut) {
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    let mut output = TextEdit::singleline(&mut edit.text)
        .id(view.tree_id.with("rename"))
        .desired_width(rect.width())
        .show(&mut child);
    if !edit.started {
        edit.started = true;
        output.response.request_focus();
        // Like file managers: renaming on disk preselects the name without extension.
        let end = if edit.on_disk { stem_chars(&edit.text) } else { edit.text.chars().count() };
        let range = CCursorRange::two(CCursor::new(0), CCursor::new(end));
        output.state.cursor.set_char_range(Some(range));
        output.state.store(ui.ctx(), output.response.id);
    }
    if output.response.lost_focus() {
        let cancel = ui.input(|i| i.key_pressed(Key::Escape));
        out.actions.push(if cancel { Action::CancelRename } else { Action::CommitRename });
        ui.memory_mut(|m| m.request_focus(view.tree_id));
    }
}

fn paint_drop_indicator(painter: &Painter, rect: Rect, x: f32, pos: DropPos, color: Color32) {
    let stroke = Stroke::new(2.0_f32, color);
    let shape = match pos {
        DropPos::Before => Shape::hline(x..=rect.right(), rect.top() + 1.0, stroke),
        DropPos::After => Shape::hline(x..=rect.right(), rect.bottom() - 1.0, stroke),
        DropPos::Into => Shape::rect_stroke(rect.shrink(1.0), 2.0, stroke),
    };
    painter.add(shape);
}

fn drop_zone(ui: &Ui, view: &View, row: &Row, target: NodeId, response: &Response, x: f32, out: &mut RowsOut) {
    let hovering = response.dnd_hover_payload::<NodeId>().zip(ui.ctx().pointer_hover_pos());
    if let Some((dragged, pointer)) = hovering {
        let rect = response.rect;
        let pos = drop_position(rect.top(), rect.bottom(), pointer.y, row.kind == RowKind::Group);
        if let Some((parent, index)) = drop_target(view.ws, *dragged, target, pos) {
            paint_drop_indicator(ui.painter(), rect, x, pos, view.theme.drop);
            if response.dnd_release_payload::<NodeId>().is_some() {
                out.actions.push(Action::Move { node: *dragged, parent, index, expand: pos == DropPos::Into });
            }
        }
    }
}

fn item(ui: &mut Ui, label: &str, action: Action, actions: &mut Vec<Action>) {
    if ui.button(label).clicked() {
        actions.push(action);
        ui.close_menu();
    }
}

fn add_items(ui: &mut Ui, group: NodeId, actions: &mut Vec<Action>) {
    item(ui, "New group", Action::NewGroup(group), actions);
    item(ui, "Add files…", Action::Emit(TreeEvent::AddFiles { parent: group }), actions);
    item(ui, "Add folder…", Action::Emit(TreeEvent::AddFolder { parent: group }), actions);
}

fn row_menu(ui: &mut Ui, view: &View, row: &Row, actions: &mut Vec<Action>) {
    let node = match row.key {
        RowKey::Node(id) => Some(id),
        _ => None,
    };
    let live = row.path.clone().filter(|_| !row.missing);
    let live_file = live.clone().filter(|_| row.kind.is_file());
    if let Some(id) = node.filter(|_| row.missing) {
        item(ui, "Relocate…", Action::Emit(TreeEvent::Relocate { node: id }), actions);
    }
    if let Some(p) = &live_file {
        item(ui, "Open", Action::Emit(TreeEvent::Open(p.clone())), actions);
        item(ui, "Open as hex", Action::Emit(TreeEvent::OpenHex(p.clone())), actions);
    }
    if let Some(p) = live.clone().filter(|_| matches!(row.kind, RowKind::Folder | RowKind::Dir)) {
        item(ui, "Refresh", Action::Refresh(p), actions);
    }
    ui.separator();
    if node.is_some() {
        item(ui, "Rename", Action::StartRename { key: row.key.clone(), on_disk: false }, actions);
    }
    if live.is_some() {
        item(ui, "Rename on disk…", Action::StartRename { key: row.key.clone(), on_disk: true }, actions);
    }
    if let Some(id) = node {
        ui.menu_button("Color", |ui| color_menu(ui, id, row.own_color, actions));
    }
    ui.separator();
    add_items(ui, row.group, actions);
    if let Some(id) = node {
        item(ui, "Remove from workspace", Action::Remove(id), actions);
    }
    ui.separator();
    if let Some(p) = &live {
        item(ui, REVEAL_LABEL, Action::Emit(TreeEvent::RevealInOs(p.clone())), actions);
    }
    if let Some(p) = &live_file {
        let runnable = p.extension().and_then(|e| e.to_str()).and_then(Interpreter::from_extension);
        if runnable.is_some() {
            item(ui, "Run file", Action::Emit(TreeEvent::RunFile(p.clone())), actions);
        }
        if view.ctx.quarto_available && can_render(p) {
            item(ui, "Render with Quarto", Action::Emit(TreeEvent::QuartoRender(p.clone())), actions);
        }
        item(ui, "Show versions", Action::Emit(TreeEvent::ShowHistory(p.clone())), actions);
    }
    if let Some(p) = &row.path {
        item(ui, "Copy path", Action::Emit(TreeEvent::CopyPath(p.clone())), actions);
    }
}

fn color_menu(ui: &mut Ui, id: NodeId, current: Option<Rgb>, actions: &mut Vec<Action>) {
    let swatch_size = vec2(20.0, 20.0);
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(SWATCHES_PER_LINE * (swatch_size.x + ui.spacing().item_spacing.x));
        PALETTE.iter().for_each(|&c| {
            let outline =
                if current == Some(c) { Stroke::new(2.0_f32, ui.visuals().strong_text_color()) } else { Stroke::NONE };
            let swatch = Button::new("").fill(rgb(c)).stroke(outline).min_size(swatch_size);
            if ui.add(swatch).clicked() {
                actions.push(Action::SetColor(id, Some(c)));
                ui.close_menu();
            }
        });
    });
    ui.menu_button("Custom…", |ui| {
        let mut color = rgb(current.unwrap_or(PALETTE[0]));
        if color_picker_color32(ui, &mut color, Alpha::Opaque) {
            actions.push(Action::SetColor(id, Some(Rgb(color.r(), color.g(), color.b()))));
        }
    });
    item(ui, "None", Action::SetColor(id, None), actions);
}

fn apply_all(ws: &mut Workspace, state: &mut TreeState, actions: Vec<Action>, events: &mut Vec<TreeEvent>) {
    actions.into_iter().for_each(|a| apply(ws, state, a, events));
}

fn apply(ws: &mut Workspace, state: &mut TreeState, action: Action, events: &mut Vec<TreeEvent>) {
    match action {
        Action::Select(key) => state.selected = Some(key),
        Action::Toggle(RowKey::Node(id)) => {
            let expanded = ws.node(id).is_some_and(|n| n.expanded);
            ws.set_expanded(id, !expanded);
        }
        Action::Toggle(RowKey::Entry(path)) => {
            if !state.open_dirs.remove(&path) {
                state.open_dirs.insert(path);
            }
        }
        Action::Toggle(RowKey::Note(_)) => {}
        Action::Emit(event) => events.push(event),
        Action::StartRename { key, on_disk } => {
            state.rename = rename_text(ws, &key, on_disk).map(|text| RenameEdit { key, on_disk, text, started: false });
        }
        Action::CommitRename => {
            if let Some(edit) = state.rename.take() {
                commit_rename(ws, state, edit, events);
            }
        }
        Action::CancelRename => state.rename = None,
        Action::SetColor(id, color) => ws.set_color(id, color),
        Action::NewGroup(parent) => new_group(ws, state, parent, events),
        Action::Remove(id) => {
            if ws.remove(id).is_some() {
                state.selected = state.selected.take().filter(|k| *k != RowKey::Node(id));
            }
        }
        Action::Refresh(path) => forget_listings(state, &path),
        Action::RefreshAll => {
            state.listings.clear();
            state.missing_checked = None;
        }
        Action::Move { node, parent, index, expand } => match ws.move_node(node, parent, index) {
            Ok(()) if expand => ws.set_expanded(parent, true),
            Ok(()) => {}
            Err(e) => events.push(TreeEvent::Error(e.to_string())),
        },
    }
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

fn rename_text(ws: &Workspace, key: &RowKey, on_disk: bool) -> Option<String> {
    match key {
        RowKey::Node(id) => {
            let node = ws.node(*id)?;
            if on_disk {
                node.path().and_then(file_name).map(str::to_string)
            } else {
                Some(node.display_name())
            }
        }
        RowKey::Entry(path) if on_disk => file_name(path).map(str::to_string),
        _ => None,
    }
}

fn new_group(ws: &mut Workspace, state: &mut TreeState, parent: NodeId, events: &mut Vec<TreeEvent>) {
    match ws.add_group(parent, NEW_GROUP_NAME) {
        Ok(id) => {
            ws.set_expanded(parent, true);
            state.selected = Some(RowKey::Node(id));
            state.rename = Some(RenameEdit {
                key: RowKey::Node(id),
                on_disk: false,
                text: NEW_GROUP_NAME.to_string(),
                started: false,
            });
        }
        Err(e) => events.push(TreeEvent::Error(e.to_string())),
    }
}

/// An alias equal to the file name is dropped so later renames on disk show the new name.
fn set_alias(ws: &mut Workspace, id: NodeId, name: &str) {
    let change = ws.node(id).map(|n: &Node| {
        let plain = name.is_empty() || n.path().and_then(file_name) == Some(name);
        (n.alias.clone(), (!plain).then(|| name.to_string()))
    });
    if let Some((_, alias)) = change.filter(|(old, new)| old != new) {
        ws.set_alias(id, alias);
    }
}

fn commit_rename(ws: &mut Workspace, state: &mut TreeState, edit: RenameEdit, events: &mut Vec<TreeEvent>) {
    let name = edit.text.trim();
    match (&edit.key, edit.on_disk) {
        (RowKey::Node(id), false) => set_alias(ws, *id, name),
        (RowKey::Node(id), true) => {
            let unchanged = ws.node(*id).and_then(Node::path).and_then(file_name) == Some(name);
            if !unchanged {
                match ws.rename_on_disk(*id, name) {
                    Ok((old, new)) => renamed(state, old, new, events),
                    Err(e) => events.push(TreeEvent::Error(e.to_string())),
                }
            }
        }
        (RowKey::Entry(path), true) if file_name(path) != Some(name) => match rename_path_on_disk(path, name) {
            Ok(new) => {
                ws.path_renamed(path, &new);
                renamed(state, path.clone(), new, events);
            }
            Err(e) => events.push(TreeEvent::Error(e.to_string())),
        },
        _ => {}
    }
}

fn forget_listings(state: &mut TreeState, path: &Path) {
    state.listings.retain(|dir, _| !dir.starts_with(path));
}

fn moved(path: &Path, old: &Path, new: &Path) -> PathBuf {
    match path.strip_prefix(old) {
        Ok(rest) if rest.as_os_str().is_empty() => new.to_path_buf(),
        Ok(rest) => new.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

fn renamed(state: &mut TreeState, old: PathBuf, new: PathBuf, events: &mut Vec<TreeEvent>) {
    if let Some(parent) = old.parent() {
        state.listings.remove(parent);
    }
    forget_listings(state, &old);
    state.open_dirs = std::mem::take(&mut state.open_dirs).into_iter().map(|p| moved(&p, &old, &new)).collect();
    if let Some(RowKey::Entry(p)) = &mut state.selected {
        *p = moved(p, &old, &new);
    }
    state.missing_checked = None;
    events.push(TreeEvent::Renamed { old, new });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renaming_a_linked_entry_updates_disk_nodes_and_open_subfolders() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(sub.join("deep")).expect("mkdir");
        let mut ws = Workspace::new("t");
        let root = ws.root.id;
        ws.add_folder(root, dir.path(), vec![], vec![]).expect("add folder");
        let file = ws.add_file(root, &sub.join("deep").join("x.txt")).expect("add file");
        let mut state = TreeState::new();
        state.open_dirs.insert(sub.join("deep"));
        let edit = RenameEdit { key: RowKey::Entry(sub.clone()), on_disk: true, text: "renamed".into(), started: true };
        let mut events = Vec::new();
        commit_rename(&mut ws, &mut state, edit, &mut events);
        let new = dir.path().join("renamed");
        assert!(new.join("deep").is_dir());
        assert_eq!(events, [TreeEvent::Renamed { old: sub, new: new.clone() }]);
        let node_path = ws.node(file).and_then(Node::path).map(Path::to_path_buf);
        assert_eq!(node_path, Some(new.join("deep").join("x.txt")));
        assert!(state.open_dirs.contains(&new.join("deep")));
    }

    #[test]
    fn failed_rename_on_disk_becomes_error_event() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        std::fs::write(dir.path().join("b.txt"), "b").expect("write");
        let mut ws = Workspace::new("t");
        let root = ws.root.id;
        let id = ws.add_file(root, &dir.path().join("a.txt")).expect("add file");
        let mut state = TreeState::new();
        let edit = RenameEdit { key: RowKey::Node(id), on_disk: true, text: "b.txt".into(), started: true };
        let mut events = Vec::new();
        commit_rename(&mut ws, &mut state, edit, &mut events);
        assert!(matches!(events.as_slice(), [TreeEvent::Error(_)]));
        assert!(dir.path().join("a.txt").is_file());
    }
}
