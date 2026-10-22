//! Shared application state. Owned by the integrator; ShellCore and Features extend only their own sub-states
//! (`ShellState` in `app.rs`, `FeatState` in `commands.rs`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use kladde_core::{CoreError, Document, OpenOptions, PreparedSave, Progress};
use kladde_editor::EditorState;
use kladde_hex::HexState;
use kladde_panels::Keymap;
use kladde_tools::History;
use kladde_workspace::{Settings, SplitMode, ViewKind, Workspace};

pub type DocId = u64;

/// Document being opened on a worker thread.
pub struct LoadJob {
    pub progress: Arc<Progress>,
    pub rx: mpsc::Receiver<Result<Document, CoreError>>,
}

/// Save running on a worker thread; `finish_save` happens on the UI thread.
pub struct SaveTask {
    pub progress: Arc<Progress>,
    pub rx: mpsc::Receiver<Result<PreparedSave, CoreError>>,
    pub target: PathBuf,
    /// Close the document (and possibly quit) once the save succeeded.
    pub close_after: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskNotice {
    Changed,
    Deleted,
}

/// One open document, shared by every tab view that shows it (E9).
pub struct DocEntry {
    pub id: DocId,
    pub doc: Document,
    /// Title for untitled documents ("new 1"); file name otherwise.
    pub title: String,
    pub open_opts: OpenOptions,
    pub load: Option<LoadJob>,
    pub save: Option<SaveTask>,
    pub tail: bool,
    pub disk_notice: Option<DiskNotice>,
    /// Hot-exit / periodic backup file in `<data>/backups`.
    pub backup: Option<PathBuf>,
    pub backup_revision: Option<u64>,
}

/// A tab: one view onto a document with its own caret/scroll state.
pub struct TabView {
    pub doc: DocId,
    pub view: ViewKind,
    pub editor: EditorState,
    pub hex: HexState,
    /// X10: show the CSV table instead of the editor.
    pub csv_mode: bool,
}

#[derive(Default)]
pub struct Pane {
    pub tabs: Vec<TabView>,
    pub active: usize,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct App {
    pub data_dir: PathBuf,
    pub settings: Settings,
    pub keymap: Keymap,
    pub workspaces: Vec<Workspace>,
    pub active_ws: Option<usize>,
    pub docs: HashMap<DocId, DocEntry>,
    pub next_doc_id: DocId,
    pub untitled_counter: u32,
    pub panes: [Pane; 2],
    pub split: SplitMode,
    pub split_ratio: f32,
    /// Pane that receives commands (0 or 1).
    pub focused_pane: usize,
    pub toasts: Vec<Toast>,
    /// None when git is not installed or history is disabled (V1).
    pub history: Option<Arc<History>>,
    pub quarto: Option<PathBuf>,
    pub shell: crate::app::ShellState,
    pub feat: crate::commands::FeatState,
}
