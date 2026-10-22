//! Command dispatch, the Features sub-state, palettes and modal dialogs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use kladde_core::{LineEnding, OpenMode, OpenOptions, TextEncoding};
use kladde_editor::EditorState;
use kladde_hex::HexState;
use kladde_panels::{
    commands, encoding_for_command, line_tool_for_command, settings_capturing, show_palette, Chord, HistoryState,
    OutputState, PaletteItem, PaletteState, ResultsState,
};
use kladde_tools::{can_render, Interpreter, LineTool};
use kladde_workspace::{SplitMode, ThemeChoice, ViewKind};

use crate::docs::auto_open;
use crate::find::SearchState;
use crate::macros::MacroState;
use crate::run::{PendingCode, PendingSave};
use crate::state::{App, DocEntry, DocId, TabView};
use crate::tools::{Confirm, CsvCache, DiffWindow, GotoState, HashPanel, Transform};

pub const QUARTO_MISSING: &str = "Quarto not found on PATH";
pub const HISTORY_MISSING: &str = "File history unavailable (git not installed or history disabled)";

const ZOOM_MIN: f32 = 6.0;
const ZOOM_MAX: f32 = 48.0;
const ZOOM_DEFAULT: f32 = 14.0;
const PALETTE_FILES_MAX: usize = 50_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BottomTab {
    Results,
    Output,
    History,
    Hash,
}

/// Every command the app knows; `classify` maps command ids onto it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    New,
    Open,
    OpenHex,
    Save,
    SaveAs,
    SaveAll,
    Close,
    CloseAll,
    Reload,
    Recent,
    OpenReadOnly,
    Undo,
    Redo,
    EditCut,
    EditCopy,
    EditPaste,
    SelectAll,
    DuplicateLine,
    DeleteLine,
    ToggleComment,
    Find,
    Replace,
    FindNext,
    FindPrev,
    FindInFiles,
    GotoLine,
    GotoOffset,
    BookmarkToggle,
    BookmarkNext,
    BookmarkPrev,
    ToggleHex,
    WordWrap,
    Whitespace,
    LineNumbers,
    Split(SplitMode),
    Unsplit,
    FoldAll,
    UnfoldAll,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Tail,
    ThemeToggle,
    WorkspacePanel,
    OutputPanel,
    CsvTable,
    DiffFiles,
    PaletteCommands,
    PaletteFiles,
    WsNew,
    WsOpen,
    WsSave,
    WsClose,
    WsSwitch,
    WsAddFile,
    RunFence,
    RunSelection,
    RunFile,
    RunInTerminal,
    RunStop,
    /// Quarto render with an optional `--to` format.
    Quarto(Option<&'static str>),
    HistoryShow,
    HistorySnapshot,
    Hash,
    Transform(Transform),
    Lines(LineTool),
    Encoding(TextEncoding),
    Eol(LineEnding),
    Settings,
    MacroRecord,
    MacroPlay,
    Quit,
}

/// The command an id stands for; None for unknown ids.
pub fn classify(id: &str) -> Option<Cmd> {
    let fixed = match id {
        "file.new" => Some(Cmd::New),
        "file.open" => Some(Cmd::Open),
        "file.open_hex" => Some(Cmd::OpenHex),
        "file.save" => Some(Cmd::Save),
        "file.save_as" => Some(Cmd::SaveAs),
        "file.save_all" => Some(Cmd::SaveAll),
        "file.close" => Some(Cmd::Close),
        "file.close_all" => Some(Cmd::CloseAll),
        "file.reload" => Some(Cmd::Reload),
        "file.recent" => Some(Cmd::Recent),
        "file.open_readonly" => Some(Cmd::OpenReadOnly),
        "edit.undo" => Some(Cmd::Undo),
        "edit.redo" => Some(Cmd::Redo),
        "edit.cut" => Some(Cmd::EditCut),
        "edit.copy" => Some(Cmd::EditCopy),
        "edit.paste" => Some(Cmd::EditPaste),
        "edit.select_all" => Some(Cmd::SelectAll),
        "edit.duplicate_line" => Some(Cmd::DuplicateLine),
        "edit.delete_line" => Some(Cmd::DeleteLine),
        "edit.toggle_comment" => Some(Cmd::ToggleComment),
        "search.find" => Some(Cmd::Find),
        "search.replace" => Some(Cmd::Replace),
        "search.find_next" => Some(Cmd::FindNext),
        "search.find_prev" => Some(Cmd::FindPrev),
        "search.find_in_files" => Some(Cmd::FindInFiles),
        "search.goto" => Some(Cmd::GotoLine),
        "search.goto_offset" => Some(Cmd::GotoOffset),
        "bookmark.toggle" => Some(Cmd::BookmarkToggle),
        "bookmark.next" => Some(Cmd::BookmarkNext),
        "bookmark.prev" => Some(Cmd::BookmarkPrev),
        "view.toggle_hex" => Some(Cmd::ToggleHex),
        "view.word_wrap" => Some(Cmd::WordWrap),
        "view.whitespace" => Some(Cmd::Whitespace),
        "view.line_numbers" => Some(Cmd::LineNumbers),
        "view.split_vertical" => Some(Cmd::Split(SplitMode::Vertical)),
        "view.split_horizontal" => Some(Cmd::Split(SplitMode::Horizontal)),
        "view.unsplit" => Some(Cmd::Unsplit),
        "view.fold_all" => Some(Cmd::FoldAll),
        "view.unfold_all" => Some(Cmd::UnfoldAll),
        "view.zoom_in" => Some(Cmd::ZoomIn),
        "view.zoom_out" => Some(Cmd::ZoomOut),
        "view.zoom_reset" => Some(Cmd::ZoomReset),
        "view.tail" => Some(Cmd::Tail),
        "view.theme_toggle" => Some(Cmd::ThemeToggle),
        "view.workspace_panel" => Some(Cmd::WorkspacePanel),
        "view.output_panel" => Some(Cmd::OutputPanel),
        "view.csv_table" => Some(Cmd::CsvTable),
        "view.diff_files" => Some(Cmd::DiffFiles),
        "palette.commands" => Some(Cmd::PaletteCommands),
        "palette.files" => Some(Cmd::PaletteFiles),
        "workspace.new" => Some(Cmd::WsNew),
        "workspace.open" => Some(Cmd::WsOpen),
        "workspace.save" => Some(Cmd::WsSave),
        "workspace.close" => Some(Cmd::WsClose),
        "workspace.switch" => Some(Cmd::WsSwitch),
        "workspace.add_current_file" => Some(Cmd::WsAddFile),
        "run.fence_at_caret" => Some(Cmd::RunFence),
        "run.selection" => Some(Cmd::RunSelection),
        "run.file" => Some(Cmd::RunFile),
        "run.in_terminal" => Some(Cmd::RunInTerminal),
        "run.stop" => Some(Cmd::RunStop),
        "quarto.render" => Some(Cmd::Quarto(None)),
        "quarto.render_html" => Some(Cmd::Quarto(Some("html"))),
        "quarto.render_pdf" => Some(Cmd::Quarto(Some("pdf"))),
        "quarto.render_docx" => Some(Cmd::Quarto(Some("docx"))),
        "history.show" => Some(Cmd::HistoryShow),
        "history.snapshot_now" => Some(Cmd::HistorySnapshot),
        "tools.hash" => Some(Cmd::Hash),
        "tools.json_pretty" => Some(Cmd::Transform(Transform::JsonPretty)),
        "tools.json_minify" => Some(Cmd::Transform(Transform::JsonMinify)),
        "tools.xml_pretty" => Some(Cmd::Transform(Transform::XmlPretty)),
        "tools.xml_minify" => Some(Cmd::Transform(Transform::XmlMinify)),
        "tools.base64_encode" => Some(Cmd::Transform(Transform::Base64Encode)),
        "tools.base64_decode" => Some(Cmd::Transform(Transform::Base64Decode)),
        "tools.url_encode" => Some(Cmd::Transform(Transform::UrlEncode)),
        "tools.url_decode" => Some(Cmd::Transform(Transform::UrlDecode)),
        "tools.hex_encode" => Some(Cmd::Transform(Transform::HexEncode)),
        "tools.hex_decode" => Some(Cmd::Transform(Transform::HexDecode)),
        "eol.lf" => Some(Cmd::Eol(LineEnding::Lf)),
        "eol.crlf" => Some(Cmd::Eol(LineEnding::CrLf)),
        "eol.cr" => Some(Cmd::Eol(LineEnding::Cr)),
        "settings.open" => Some(Cmd::Settings),
        "macro.record" => Some(Cmd::MacroRecord),
        "macro.play" => Some(Cmd::MacroPlay),
        "app.quit" => Some(Cmd::Quit),
        _ => None,
    };
    fixed.or_else(|| line_tool_for_command(id).map(Cmd::Lines)).or_else(|| encoding_for_command(id).map(Cmd::Encoding))
}

/// Menu/palette label of a command id.
pub fn command_label(id: &str) -> &'static str {
    commands().iter().find(|c| c.id == id).map_or("Command", |c| c.label)
}

const NEED_DOC: u8 = 1;
const NEED_TEXT: u8 = 2;
const NEED_WRITE: u8 = 4;
const NEED_PATH: u8 = 8;

/// What a command needs from the active document.
fn needs(cmd: Cmd) -> u8 {
    match cmd {
        Cmd::Save
        | Cmd::SaveAs
        | Cmd::Close
        | Cmd::Undo
        | Cmd::Redo
        | Cmd::EditCopy
        | Cmd::SelectAll
        | Cmd::Find
        | Cmd::Replace
        | Cmd::FindNext
        | Cmd::FindPrev
        | Cmd::GotoOffset
        | Cmd::ToggleHex
        | Cmd::Hash
        | Cmd::Quarto(_)
        | Cmd::RunFile => NEED_DOC,
        Cmd::Reload | Cmd::Tail | Cmd::WsAddFile | Cmd::HistoryShow | Cmd::HistorySnapshot => NEED_DOC | NEED_PATH,
        Cmd::EditCut | Cmd::EditPaste => NEED_DOC | NEED_WRITE,
        Cmd::DuplicateLine | Cmd::DeleteLine | Cmd::ToggleComment | Cmd::Transform(_) | Cmd::Lines(_) | Cmd::Eol(_) => {
            NEED_DOC | NEED_TEXT | NEED_WRITE
        }
        Cmd::GotoLine
        | Cmd::BookmarkToggle
        | Cmd::BookmarkNext
        | Cmd::BookmarkPrev
        | Cmd::FoldAll
        | Cmd::UnfoldAll
        | Cmd::RunFence
        | Cmd::RunSelection
        | Cmd::RunInTerminal
        | Cmd::CsvTable
        | Cmd::Encoding(_) => NEED_DOC | NEED_TEXT,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    Commands,
    Files,
    RecentFiles,
    Workspaces,
    Interpreter,
    DiffFirst,
    DiffSecond,
}

/// State of everything the Features agent draws or runs.
#[derive(Default)]
pub struct FeatState {
    /// Bottom tab to reveal; consumed by the shell each frame.
    pub show_bottom: Option<BottomTab>,
    pub output: OutputState,
    pub results: ResultsState,
    pub history_panel: HistoryState,
    pub find: kladde_panels::FindState,
    pub search: SearchState,
    pub hash: HashPanel,
    pub macros: MacroState,
    pub palette: PaletteState,
    pub palette_kind: Option<PaletteKind>,
    pub palette_items: Vec<PaletteItem>,
    pub pending_code: Option<PendingCode>,
    pub diff_first: Option<DocId>,
    pub diff: Option<DiffWindow>,
    pub goto: Option<GotoState>,
    pub confirm: Option<Confirm>,
    pub settings_open: bool,
    pub settings_dirty: bool,
    pub csv: HashMap<DocId, CsvCache>,
    pub after_save: Vec<PendingSave>,
    /// Set by edit.paste until the requested clipboard text arrives.
    pub pending_paste: Option<std::time::Instant>,
    /// History restore waiting for its document to finish loading.
    pub pending_restore: Option<(PathBuf, Vec<u8>, std::time::Instant)>,
    /// Text of the document shown in the History tab: (doc, revision, text).
    pub history_text: Option<(DocId, u64, String)>,
    /// Background tasks reporting a message (Ok = info, Err = error).
    pub background: Vec<Receiver<Result<String, String>>>,
    /// An editor had keyboard focus at the end of the previous frame.
    pub editor_had_focus: bool,
}

/// Runs `work` on a worker thread; the receiver yields its result once.
pub fn spawn_task<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // The receiver is gone when the job was dropped; nobody wants the result then.
        drop(tx.send(work()));
    });
    rx
}

/// Result of a one-shot worker: None while running, Err if the worker died.
pub fn poll_task<T>(rx: &Receiver<T>, what: &str) -> Option<Result<T, String>> {
    match rx.try_recv() {
        Ok(v) => Some(Ok(v)),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err(format!("{what} stopped unexpectedly"))),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn parent_text(path: &Path) -> String {
    path.parent().map(|p| p.display().to_string()).unwrap_or_default()
}

impl App {
    /// Active tab and its document, read-only.
    pub fn focused_view(&self) -> Option<(&DocEntry, &TabView)> {
        let (pane, idx) = self.active_tab()?;
        let tab = self.panes.get(pane)?.tabs.get(idx)?;
        Some((self.docs.get(&tab.doc)?, tab))
    }

    pub fn focused_doc(&self) -> Option<DocId> {
        self.focused_view().map(|(e, _)| e.id)
    }

    /// Every tab view showing document `id`.
    pub fn views_of(&mut self, id: DocId) -> impl Iterator<Item = &mut TabView> + '_ {
        self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).filter(move |t| t.doc == id)
    }

    /// Loaded document with `path`, if open.
    pub fn doc_by_path(&self, path: &Path) -> Option<DocId> {
        let same = |e: &&DocEntry| e.doc.path().is_some_and(|p| crate::docs::same_path(p, path));
        self.docs.values().filter(|e| e.load.is_none()).find(same).map(|e| e.id)
    }

    pub fn store_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.data_dir) {
            self.toast_error(format!("Could not save settings: {e}"));
        }
    }

    /// Why `cmd` cannot run right now; None if it can.
    pub fn unavailable(&self, cmd: Cmd) -> Option<String> {
        let view = self.focused_view();
        let need = needs(cmd);
        let doc_reason = match view {
            _ if need & NEED_DOC == 0 => None,
            None => Some("No document open"),
            Some((e, _)) if e.load.is_some() => Some("Document is still loading"),
            Some((e, _)) if need & NEED_PATH != 0 && e.doc.path().is_none() => {
                Some("Document is not saved to a file yet")
            }
            Some((_, t)) if need & NEED_TEXT != 0 && t.view == ViewKind::Hex => {
                Some("Not available in hex view (toggle with View → Toggle hex view)")
            }
            Some((e, _)) if need & NEED_WRITE != 0 && e.doc.is_read_only() => Some("Document is read-only"),
            Some(_) => None,
        };
        doc_reason.map(str::to_string).or_else(|| self.tool_reason(cmd, view))
    }

    fn tool_reason(&self, cmd: Cmd, view: Option<(&DocEntry, &TabView)>) -> Option<String> {
        let path = view.and_then(|(e, _)| e.doc.path());
        let binary = view.is_some_and(|(e, _)| e.doc.is_binary());
        match cmd {
            Cmd::Quarto(_) if self.quarto.is_none() => Some(QUARTO_MISSING.to_string()),
            Cmd::Quarto(_) => {
                path.filter(|p| !can_render(p)).map(|_| "Quarto renders .qmd, .md, .ipynb and .Rmd files".to_string())
            }
            Cmd::HistoryShow | Cmd::HistorySnapshot if self.history.is_none() => Some(HISTORY_MISSING.to_string()),
            Cmd::RunStop if !self.feat.output.has_running() => Some("No process is running".to_string()),
            Cmd::RunFile => path.and_then(|p| {
                let ext = p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                Interpreter::from_extension(&ext).is_none().then(|| format!("No interpreter runs .{ext} files"))
            }),
            Cmd::Encoding(_) | Cmd::Eol(_) if binary => Some("Binary documents are saved byte for byte".to_string()),
            Cmd::WsSave | Cmd::WsClose | Cmd::WsAddFile if self.active_ws.is_none() => {
                Some("No workspace open".to_string())
            }
            Cmd::WsSwitch if self.workspaces.is_empty() && self.settings.recent_workspaces.is_empty() => {
                Some("No open or recent workspaces".to_string())
            }
            Cmd::DiffFiles if self.docs.len() < 2 => Some("Open two documents to compare".to_string()),
            Cmd::Unsplit if self.split == SplitMode::None => Some("The view is not split".to_string()),
            Cmd::MacroPlay if self.feat.macros.recording => Some("Stop recording first".to_string()),
            Cmd::MacroPlay if self.feat.macros.recorded.is_empty() => Some("No macro recorded".to_string()),
            Cmd::Recent if self.settings.recent_files.is_empty() => Some("No recent files".to_string()),
            Cmd::SaveAll if !self.docs.values().any(|e| e.doc.is_dirty()) => Some("No unsaved documents".to_string()),
            Cmd::CloseAll if self.panes.iter().all(|p| p.tabs.is_empty()) => Some("No document open".to_string()),
            _ => None,
        }
    }

    /// On/off state of toggle commands for menu check marks.
    pub fn checked(&self, cmd: Cmd) -> Option<bool> {
        let view = self.focused_view();
        match cmd {
            Cmd::WordWrap => Some(self.settings.word_wrap),
            Cmd::Whitespace => Some(self.settings.show_whitespace),
            Cmd::LineNumbers => Some(self.settings.show_line_numbers),
            Cmd::WorkspacePanel => Some(self.shell.side_visible),
            Cmd::OutputPanel => Some(self.shell.bottom_visible),
            Cmd::Tail => Some(view.is_some_and(|(e, _)| e.tail)),
            Cmd::CsvTable => Some(view.is_some_and(|(_, t)| t.csv_mode)),
            Cmd::ToggleHex => Some(view.is_some_and(|(_, t)| t.view == ViewKind::Hex)),
            Cmd::MacroRecord => Some(self.feat.macros.recording),
            Cmd::Split(mode) => Some(self.split == mode),
            _ => None,
        }
    }

    pub fn execute(&mut self, ctx: &egui::Context, id: &str) {
        if self.feat.macros.recording && !id.starts_with("macro.") {
            self.feat.macros.record_command(id);
        }
        match classify(id) {
            None => self.toast_error(format!("Unknown command: {id}")),
            Some(cmd) => match self.unavailable(cmd) {
                Some(reason) => self.toast_info(format!("{}: {reason}", command_label(id))),
                None => self.run_command(ctx, cmd),
            },
        }
    }

    fn run_command(&mut self, ctx: &egui::Context, cmd: Cmd) {
        let doc = self.focused_doc();
        match cmd {
            Cmd::New => {
                self.new_document();
            }
            Cmd::Open => self.pick_and_open(auto_open(), None),
            Cmd::OpenHex => {
                self.pick_and_open(OpenOptions { mode: OpenMode::Binary, ..auto_open() }, Some(ViewKind::Hex))
            }
            Cmd::OpenReadOnly => {
                self.pick_and_open(OpenOptions { read_only: true, no_lock: true, ..auto_open() }, None)
            }
            Cmd::Save => doc.into_iter().for_each(|id| self.save_doc(id, false)),
            Cmd::SaveAs => doc.into_iter().for_each(|id| self.save_doc(id, true)),
            Cmd::SaveAll => self.save_all(),
            Cmd::Close => self.active_tab().into_iter().for_each(|(p, i)| self.close_tab(p, i)),
            Cmd::CloseAll => self.close_all(),
            Cmd::Reload => doc.into_iter().for_each(|id| self.confirm_reload(id)),
            Cmd::Recent => self.open_palette(PaletteKind::RecentFiles),
            Cmd::Undo
            | Cmd::Redo
            | Cmd::EditCut
            | Cmd::EditCopy
            | Cmd::EditPaste
            | Cmd::SelectAll
            | Cmd::DuplicateLine
            | Cmd::DeleteLine
            | Cmd::ToggleComment
            | Cmd::BookmarkToggle
            | Cmd::BookmarkNext
            | Cmd::BookmarkPrev => self.edit_command(ctx, cmd),
            Cmd::Find => self.open_find(false),
            Cmd::Replace => self.open_find(true),
            Cmd::FindNext => self.find_step(true),
            Cmd::FindPrev => self.find_step(false),
            Cmd::FindInFiles => self.open_find_in_files(),
            Cmd::GotoLine => self.feat.goto = Some(GotoState::new(false)),
            Cmd::GotoOffset => self.feat.goto = Some(GotoState::new(true)),
            Cmd::ToggleHex => self.toggle_hex(),
            Cmd::WordWrap => self.toggle_setting(|s| &mut s.word_wrap),
            Cmd::Whitespace => self.toggle_setting(|s| &mut s.show_whitespace),
            Cmd::LineNumbers => self.toggle_setting(|s| &mut s.show_line_numbers),
            Cmd::Split(mode) => self.split_view(mode),
            Cmd::Unsplit => self.unsplit_view(),
            Cmd::FoldAll => self.with_editor(|ed| ed.fold_all()),
            Cmd::UnfoldAll => self.with_editor(|ed| ed.unfold_all()),
            Cmd::ZoomIn => self.zoom(self.settings.font_size + 1.0),
            Cmd::ZoomOut => self.zoom(self.settings.font_size - 1.0),
            Cmd::ZoomReset => self.zoom(ZOOM_DEFAULT),
            Cmd::Tail => self.toggle_tail(),
            Cmd::ThemeToggle => {
                self.settings.theme = if self.dark(ctx) { ThemeChoice::Light } else { ThemeChoice::Dark };
                self.store_settings();
            }
            Cmd::WorkspacePanel => self.shell.side_visible = !self.shell.side_visible,
            Cmd::OutputPanel => {
                if self.shell.bottom_visible {
                    self.shell.bottom_visible = false;
                } else {
                    self.feat.show_bottom = Some(BottomTab::Output);
                }
            }
            Cmd::CsvTable => self.toggle_csv(),
            Cmd::DiffFiles => self.open_palette(PaletteKind::DiffFirst),
            Cmd::PaletteCommands => self.open_palette(PaletteKind::Commands),
            Cmd::PaletteFiles => self.open_palette(PaletteKind::Files),
            Cmd::WsNew => self.new_workspace(),
            Cmd::WsOpen => self.pick_workspace(),
            Cmd::WsSave => self.save_workspace(),
            Cmd::WsClose => self.close_workspace(),
            Cmd::WsSwitch => self.open_palette(PaletteKind::Workspaces),
            Cmd::WsAddFile => self.add_current_file_to_workspace(),
            Cmd::RunFence => self.run_fence_at_caret(false),
            Cmd::RunSelection => self.run_selection(false),
            Cmd::RunInTerminal => self.run_in_terminal_cmd(),
            Cmd::RunFile => doc.into_iter().for_each(|id| self.run_file_cmd(id)),
            Cmd::RunStop => {
                self.feat.output.stop_all();
                self.toast_info("Stopping running processes");
            }
            Cmd::Quarto(format) => doc.into_iter().for_each(|id| self.quarto_cmd(id, format)),
            Cmd::HistoryShow => {
                doc.and_then(|id| self.doc_path(id)).into_iter().for_each(|p| self.show_history_for(p));
            }
            Cmd::HistorySnapshot => doc.into_iter().for_each(|id| self.snapshot_now(id)),
            Cmd::Hash => self.start_hash(),
            Cmd::Transform(t) => self.transform_text(t.label(), |text| t.apply(text)),
            Cmd::Lines(tool) => self.transform_text(tool.label(), |text| Ok(kladde_tools::apply_line_tool(text, tool))),
            Cmd::Encoding(enc) => self.convert_encoding(enc),
            Cmd::Eol(le) => self.convert_eol(le),
            Cmd::Settings => self.feat.settings_open = true,
            Cmd::MacroRecord => self.toggle_macro_recording(),
            Cmd::MacroPlay => self.play_macro(),
            Cmd::Quit => self.request_quit(ctx),
        }
    }

    fn pick_and_open(&mut self, opts: OpenOptions, view: Option<ViewKind>) {
        let dir = self.focused_doc().and_then(|id| self.doc_path(id)).and_then(|p| p.parent().map(Path::to_path_buf));
        let dialog = dir.map_or_else(rfd::FileDialog::new, |d| rfd::FileDialog::new().set_directory(d));
        let files = dialog.set_title("Open").pick_files().unwrap_or_default();
        files.iter().for_each(|p| {
            self.open_path(p, opts.clone(), view);
        });
    }

    fn pick_workspace(&mut self) {
        let picked = rfd::FileDialog::new().add_filter("Kladde workspace", &["kladde"]).pick_file();
        if let Some(path) = picked {
            self.open_workspace(&path);
        }
    }

    fn add_current_file_to_workspace(&mut self) {
        let path = self.focused_doc().and_then(|id| self.doc_path(id));
        let added = path.clone().and_then(|p| {
            self.active_workspace().map(|ws| {
                let root = ws.root.id;
                ws.add_file(root, &p)
            })
        });
        match (added, path) {
            (Some(Ok(_)), Some(p)) => self.toast_info(format!("Added {} to the workspace", file_name(&p))),
            (Some(Err(e)), _) => self.toast_error(format!("Could not add file: {e}")),
            _ => self.toast_info("No workspace open"),
        }
    }

    fn confirm_reload(&mut self, id: DocId) {
        let dirty = self.docs.get(&id).is_some_and(|e| e.doc.is_dirty());
        if dirty {
            self.feat.confirm = Some(Confirm::Reload(id));
        } else {
            self.reload_doc(id);
        }
    }

    fn toggle_setting(&mut self, field: impl FnOnce(&mut kladde_workspace::Settings) -> &mut bool) {
        let value = field(&mut self.settings);
        *value = !*value;
        self.store_settings();
    }

    fn zoom(&mut self, size: f32) {
        self.settings.font_size = size.clamp(ZOOM_MIN, ZOOM_MAX);
        self.store_settings();
    }

    /// Runs `f` on the active tab's editor state.
    fn with_editor(&mut self, f: impl FnOnce(&mut EditorState)) {
        if let Some((_, tab)) = self.active_parts() {
            f(&mut tab.editor);
        }
    }

    fn toggle_hex(&mut self) {
        if let Some((entry, tab)) = self.active_parts() {
            match tab.view {
                ViewKind::Text => {
                    let range = tab.editor.primary_range();
                    tab.hex.select(&entry.doc, range);
                    tab.view = ViewKind::Hex;
                    tab.csv_mode = false;
                }
                ViewKind::Hex => {
                    let range = tab.hex.selection();
                    let caret = tab.hex.cursor();
                    if range.is_empty() {
                        tab.editor.set_caret(&entry.doc, caret);
                    } else {
                        tab.editor.select(&entry.doc, range);
                    }
                    tab.view = ViewKind::Text;
                }
            }
        }
    }

    fn toggle_tail(&mut self) {
        let state = self.active_parts().map(|(entry, _)| {
            entry.tail = !entry.tail;
            (entry.tail, entry.doc.is_dirty())
        });
        if let Some((true, true)) = state {
            self.toast_info("Tail mode follows the file once the unsaved changes are saved or reloaded");
        }
    }

    fn split_view(&mut self, mode: SplitMode) {
        self.split = mode;
        let source = self.focused_view().map(|(e, t)| (e.id, t.view, e.doc.path().map(Path::to_path_buf)));
        if let (true, Some((doc, view, path))) = (self.panes[1].tabs.is_empty(), source) {
            self.panes[1].tabs.push(TabView {
                doc,
                view,
                editor: EditorState::new(path.as_deref()),
                hex: HexState::new(),
                csv_mode: false,
            });
            self.panes[1].active = 0;
        }
    }

    fn unsplit_view(&mut self) {
        let moved = std::mem::take(&mut self.panes[1].tabs);
        self.panes[0].tabs.extend(moved);
        self.panes[1].active = 0;
        self.split = SplitMode::None;
        self.focused_pane = 0;
    }

    /// Selects the first tab showing `id`.
    fn focus_doc_tab(&mut self, id: DocId) {
        let found = self
            .panes
            .iter()
            .enumerate()
            .find_map(|(p, pane)| pane.tabs.iter().position(|t| t.doc == id).map(|i| (p, i)));
        if let Some((p, i)) = found {
            self.panes[p].active = i;
            self.focused_pane = p;
        }
    }

    /// Cancels and drops background jobs that hold a snapshot of document `id` (so saving can replace the file).
    pub fn cancel_doc_jobs(&mut self, id: DocId) {
        self.feat.search.cancel_doc(id);
        self.feat.hash.cancel_doc(id);
    }

    pub fn open_palette(&mut self, kind: PaletteKind) {
        let (title, items) = match kind {
            PaletteKind::Commands => ("Command", self.command_items()),
            PaletteKind::Files => ("Go to file", self.file_items(true)),
            PaletteKind::RecentFiles => ("Recent file", self.file_items(false)),
            PaletteKind::Workspaces => ("Switch workspace", self.workspace_items()),
            PaletteKind::Interpreter => ("Run with interpreter", interpreter_items()),
            PaletteKind::DiffFirst => ("Compare: first document", self.doc_items(None)),
            PaletteKind::DiffSecond => ("Compare with", self.doc_items(self.feat.diff_first)),
        };
        if items.is_empty() && kind == PaletteKind::Interpreter {
            self.feat.pending_code = None;
            self.toast_error("No interpreter installed (PowerShell, cmd, bash, python, node, R)");
        } else {
            self.feat.palette_items = items;
            self.feat.palette_kind = Some(kind);
            self.feat.palette.open(title);
        }
    }

    fn command_items(&self) -> Vec<PaletteItem> {
        commands()
            .iter()
            .map(|c| PaletteItem {
                id: c.id.to_string(),
                label: format!("{}: {}", c.category, c.label),
                detail: self.keymap.chord_for(c.id).map(|k| k.to_string()).unwrap_or_default(),
            })
            .collect()
    }

    /// Open documents, workspace files and recent files (`all`), or only recent files.
    fn file_items(&mut self, all: bool) -> Vec<PaletteItem> {
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        let path_item =
            |p: &Path, detail: String| PaletteItem { id: format!("path:{}", p.display()), label: file_name(p), detail };
        if all {
            let mut open: Vec<&DocEntry> = self.docs.values().collect();
            open.sort_by_key(|e| e.id);
            open.iter().for_each(|e| match e.doc.path() {
                Some(p) => {
                    if seen.insert(p.to_path_buf()) {
                        items.push(path_item(p, format!("open · {}", parent_text(p))));
                    }
                }
                None => items.push(PaletteItem {
                    id: format!("doc:{}", e.id),
                    label: e.title.clone(),
                    detail: "open · untitled".to_string(),
                }),
            });
            let ws_files = self.active_workspace().map(|ws| ws.all_files(PALETTE_FILES_MAX)).unwrap_or_default();
            ws_files
                .iter()
                .filter(|p| seen.insert(p.to_path_buf()))
                .for_each(|p| items.push(path_item(p.as_path(), parent_text(p))));
        }
        let recent = self.settings.recent_files.clone();
        recent
            .iter()
            .filter(|p| seen.insert(p.to_path_buf()))
            .for_each(|p| items.push(path_item(p.as_path(), format!("recent · {}", parent_text(p)))));
        items
    }

    fn workspace_items(&self) -> Vec<PaletteItem> {
        let open = self.workspaces.iter().enumerate().map(|(i, ws)| PaletteItem {
            id: format!("ws:{i}"),
            label: ws.name.clone(),
            detail: if Some(i) == self.active_ws { "active".to_string() } else { "open".to_string() },
        });
        let open_paths: HashSet<PathBuf> =
            self.workspaces.iter().filter_map(|ws| ws.file_path().map(Path::to_path_buf)).collect();
        let recent = self.settings.recent_workspaces.iter().filter(|p| !open_paths.contains(*p)).map(|p| PaletteItem {
            id: format!("path:{}", p.display()),
            label: file_name(p),
            detail: format!("recent · {}", parent_text(p)),
        });
        open.chain(recent).collect()
    }

    fn doc_items(&self, except: Option<DocId>) -> Vec<PaletteItem> {
        let mut docs: Vec<&DocEntry> = self.docs.values().filter(|e| Some(e.id) != except).collect();
        docs.sort_by_key(|e| e.id);
        docs.iter()
            .map(|e| PaletteItem {
                id: e.id.to_string(),
                label: e.doc.path().map_or_else(|| e.title.clone(), file_name),
                detail: e.doc.path().map(parent_text).unwrap_or_default(),
            })
            .collect()
    }

    fn palette_chosen(&mut self, ctx: &egui::Context, kind: PaletteKind, id: &str) {
        match kind {
            PaletteKind::Commands => self.execute(ctx, id),
            PaletteKind::Files | PaletteKind::RecentFiles => {
                match (id.strip_prefix("doc:"), id.strip_prefix("path:")) {
                    (Some(doc), _) => doc.parse().into_iter().for_each(|d| self.focus_doc_tab(d)),
                    (_, Some(path)) => {
                        self.open_path(Path::new(path), auto_open(), None);
                    }
                    _ => {}
                }
            }
            PaletteKind::Workspaces => match (id.strip_prefix("ws:"), id.strip_prefix("path:")) {
                (Some(i), _) => i.parse().into_iter().for_each(|i| self.switch_workspace(i)),
                (_, Some(path)) => self.open_workspace(Path::new(path)),
                _ => {}
            },
            PaletteKind::Interpreter => {
                let interp = Interpreter::all().iter().copied().find(|i| i.label() == id);
                if let (Some(interp), Some(code)) = (interp, self.feat.pending_code.take()) {
                    self.start_run(interp, &code.code, &code.cwd, code.in_terminal);
                }
            }
            PaletteKind::DiffFirst => {
                self.feat.diff_first = id.parse().ok();
                self.open_palette(PaletteKind::DiffSecond);
            }
            PaletteKind::DiffSecond => {
                let pair = self.feat.diff_first.take().zip(id.parse().ok());
                if let Some((a, b)) = pair {
                    self.open_diff(a, b);
                }
            }
        }
    }

    fn show_palette_modal(&mut self, ctx: &egui::Context) {
        if self.feat.palette.is_open() {
            let chosen = show_palette(ctx, &mut self.feat.palette, &self.feat.palette_items);
            let kind = self.feat.palette_kind;
            if !self.feat.palette.is_open() {
                self.feat.palette_kind = None;
                self.feat.palette_items.clear();
            }
            match (chosen, kind) {
                (Some(id), Some(kind)) => self.palette_chosen(ctx, kind, &id),
                (None, Some(_)) if !self.feat.palette.is_open() => {
                    self.feat.pending_code = None;
                    self.feat.diff_first = None;
                }
                _ => {}
            }
        }
    }

    /// Commands whose chords were pressed. F2/Delete stay with the focused workspace tree, and a focused text
    /// field other than the editor keeps the keys of edit commands (its own undo and clipboard).
    fn triggered_commands(&mut self, ctx: &egui::Context) -> Vec<&'static str> {
        let modal = self.feat.palette.is_open() || self.feat.goto.is_some() || self.feat.confirm.is_some();
        if modal || settings_capturing(ctx) {
            Vec::new()
        } else {
            let tree_focused = self.shell.tree_id.is_some_and(|id| ctx.memory(|m| m.has_focus(id)));
            let field_focused = !tree_focused && ctx.wants_keyboard_input() && !self.feat.editor_had_focus;
            let keymap = &self.keymap;
            let held_back = |e: &egui::Event| match e {
                egui::Event::Key { key, modifiers, .. } => {
                    let tree_key = matches!(key, egui::Key::F2 | egui::Key::Delete) && modifiers.is_none();
                    let edit_key = || {
                        keymap
                            .command_for(Chord::from_event(*modifiers, *key))
                            .is_some_and(|id| id.starts_with("edit."))
                    };
                    (tree_focused && tree_key) || (field_focused && edit_key())
                }
                _ => false,
            };
            let held: Vec<egui::Event> = ctx.input_mut(|i| {
                let (held, rest): (Vec<egui::Event>, Vec<egui::Event>) =
                    std::mem::take(&mut i.events).into_iter().partition(held_back);
                i.events = rest;
                held
            });
            let ids = self.keymap.take_triggered(ctx);
            ctx.input_mut(|i| i.events.extend(held));
            ids.into_iter().filter(|id| !(tree_focused && id.starts_with("bookmark."))).collect()
        }
    }

    pub fn features_begin_frame(&mut self, ctx: &egui::Context) {
        self.macro_playback_step(ctx);
        self.apply_pending_paste(ctx);
        let ids = self.triggered_commands(ctx);
        ids.into_iter().for_each(|id| self.execute(ctx, id));
        self.poll_search();
        self.poll_hash();
        self.poll_output();
        self.poll_after_save();
        self.poll_background();
        self.poll_pending_restore();
        if self.feat.search.busy() || self.feat.hash.busy() || !self.feat.background.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn poll_background(&mut self) {
        let mut messages = Vec::new();
        self.feat.background.retain(|rx| match poll_task(rx, "background task") {
            None => true,
            Some(Ok(msg)) => {
                messages.push(msg);
                false
            }
            Some(Err(e)) => {
                messages.push(Err(e));
                false
            }
        });
        messages.into_iter().for_each(|m| match m {
            Ok(info) => self.toast_info(info),
            Err(e) => self.toast_error(e),
        });
    }

    pub fn show_modals(&mut self, ctx: &egui::Context) {
        self.show_palette_modal(ctx);
        self.show_goto(ctx);
        self.show_confirm(ctx);
        self.show_diff_window(ctx);
        self.show_settings_dialog(ctx);
    }

    pub fn show_bottom_tab(&mut self, ui: &mut egui::Ui, tab: BottomTab) {
        match tab {
            BottomTab::Results => self.show_results_tab(ui),
            BottomTab::Output => self.show_output_tab(ui),
            BottomTab::History => self.show_history_tab(ui),
            BottomTab::Hash => self.show_hash_tab(ui),
        }
    }

    pub fn handle_tree_action(&mut self, ctx: &egui::Context, ev: kladde_wstree::TreeEvent) {
        match ev {
            kladde_wstree::TreeEvent::RunFile(path) => self.run_path(ctx, &path),
            kladde_wstree::TreeEvent::QuartoRender(path) => self.quarto_path_cmd(&path, None),
            kladde_wstree::TreeEvent::ShowHistory(path) => self.show_history_for(path),
            kladde_wstree::TreeEvent::Renamed { old, new } => self.history_rename(old, new),
            _ => {}
        }
    }
}

fn interpreter_items() -> Vec<PaletteItem> {
    Interpreter::all()
        .iter()
        .filter_map(|i| {
            i.program().map(|p| PaletteItem {
                id: i.label().to_string(),
                label: i.label().to_string(),
                detail: p.display().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `execute` dispatches every classified id through an exhaustive match on `Cmd`.
    fn is_handled(id: &str) -> bool {
        classify(id).is_some()
    }

    #[test]
    fn every_command_has_a_handler() {
        let missing: Vec<&str> = commands().iter().map(|c| c.id).filter(|id| !is_handled(id)).collect();
        assert!(missing.is_empty(), "commands without handler: {missing:?}");
    }

    #[test]
    fn unknown_ids_are_not_handled() {
        assert!(!is_handled("file.nonexistent"));
        assert!(!is_handled("lines.no_such_tool"));
        assert!(!is_handled("encoding.klingon"));
    }
}
