//! Application shell: construction, per-frame order, background results, toasts and window close.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

use kladde_hex::{HexMode, TextColumn};
use kladde_panels::Keymap;
use kladde_tools::{GitStatus, History};
use kladde_workspace::{Settings, ThemeChoice, WsError};

use crate::commands::{BottomTab, FeatState};
use crate::docs::{DiskStamp, PendingLoad};
use crate::state::{App, DocId, Pane, Toast};

/// Toasts disappear after this time.
const TOAST_LIFETIME: Duration = Duration::from_secs(6);
/// Repaint cadence while background work runs.
const BUSY_REPAINT: Duration = Duration::from_millis(100);
/// Idle wake-up for the disk watch and periodic backups.
const IDLE_REPAINT: Duration = Duration::from_secs(2);

/// Results of shell worker threads.
pub(crate) enum Bg {
    History(Result<(), String>),
    HistoryOpened(Result<Option<Arc<History>>, String>),
    Backup { id: DocId, revision: u64, path: PathBuf, result: Result<(), String> },
    Git { generation: u64, result: Result<HashMap<PathBuf, GitStatus>, String> },
}

/// Shell-owned state inside `App`.
pub struct ShellState {
    pub ctx: egui::Context,
    pub side_visible: bool,
    pub bottom_visible: bool,
    pub bottom_tab: BottomTab,
    pub tree: kladde_wstree::TreeState,
    /// Id the workspace tree derives its widget ids from; None while the side panel is hidden.
    pub tree_id: Option<egui::Id>,
    pub git: HashMap<PathBuf, GitStatus>,
    pub git_available: bool,
    pub git_generation: u64,
    pub hex_mode: HexMode,
    pub hex_text: TextColumn,
    pub show_inspector: bool,
    pub inspector_big_endian: bool,
    /// An editor or hex view had keyboard focus this frame.
    pub editor_focused: bool,
    /// Give keyboard focus to the focused pane's view on the next frame.
    pub(crate) focus_view: bool,
    pub applied_theme: Option<ThemeChoice>,
    pub(crate) pending: HashMap<DocId, PendingLoad>,
    pub(crate) disk_ignored: HashMap<DocId, DiskStamp>,
    pub(crate) close_queue: Vec<DocId>,
    pub(crate) save_attempts: HashMap<DocId, u32>,
    pub(crate) quit_pending: bool,
    pub(crate) quit_ignore_errors: bool,
    pub(crate) allow_close: bool,
    pub(crate) last_disk_check: Instant,
    pub(crate) last_backup: Instant,
    /// Distinguishes backup file names of different program runs.
    pub(crate) run_stamp: u128,
    pub(crate) bg_tx: mpsc::Sender<Bg>,
    bg_rx: mpsc::Receiver<Bg>,
    pub(crate) history_jobs: usize,
    pub(crate) backup_jobs: usize,
    pub(crate) history_opening: bool,
    pub(crate) history_missing: bool,
    pub(crate) ws_prompt: Option<crate::wsbar::WsPrompt>,
    pub(crate) update: crate::update::UpdateState,
    /// Executable to start once the quit after an update has finished.
    pub(crate) relaunch: Option<PathBuf>,
}

impl ShellState {
    fn new(ctx: egui::Context) -> ShellState {
        let (bg_tx, bg_rx) = mpsc::channel();
        // A clock before 1970 only makes backup names less unique.
        let run_stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        ShellState {
            ctx,
            side_visible: true,
            bottom_visible: false,
            bottom_tab: BottomTab::Output,
            tree: kladde_wstree::TreeState::new(),
            tree_id: None,
            git: HashMap::new(),
            git_available: kladde_tools::find_program("git").is_some(),
            git_generation: 0,
            hex_mode: HexMode::Hex,
            hex_text: TextColumn::Ascii,
            show_inspector: false,
            inspector_big_endian: false,
            editor_focused: false,
            focus_view: false,
            applied_theme: None,
            pending: HashMap::new(),
            disk_ignored: HashMap::new(),
            close_queue: Vec::new(),
            save_attempts: HashMap::new(),
            quit_pending: false,
            quit_ignore_errors: false,
            allow_close: false,
            last_disk_check: Instant::now(),
            last_backup: Instant::now(),
            run_stamp,
            bg_tx,
            bg_rx,
            history_jobs: 0,
            backup_jobs: 0,
            history_opening: false,
            history_missing: false,
            ws_prompt: None,
            update: crate::update::UpdateState::Idle,
            relaunch: None,
        }
    }
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        data_dir: PathBuf,
        settings: Result<Settings, WsError>,
        args: Vec<PathBuf>,
    ) -> App {
        let (settings, settings_error) = match settings {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(format!("Cannot read settings, using defaults: {e}"))),
        };
        let keymap = Keymap::new(&settings.keybindings);
        let keymap_errors = keymap.errors().to_vec();
        let mut app = App {
            data_dir,
            settings,
            keymap,
            ws: kladde_workspace::WorkspaceStore::default(),
            docs: HashMap::new(),
            next_doc_id: 0,
            untitled_counter: 0,
            panes: [Pane::default(), Pane::default()],
            split: kladde_workspace::SplitMode::None,
            split_ratio: 0.5,
            focused_pane: 0,
            toasts: Vec::new(),
            history: None,
            quarto: kladde_tools::quarto_path(),
            shell: ShellState::new(cc.egui_ctx.clone()),
            feat: FeatState::default(),
        };
        settings_error.into_iter().chain(keymap_errors).for_each(|e| app.toast_error(e));
        app.apply_theme(&cc.egui_ctx);
        app.startup(args);
        app.start_updates();
        app
    }

    pub fn toast_info(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: false, at: Instant::now() });
        self.shell.ctx.request_repaint();
    }

    pub fn toast_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.feat.output.add_message("Log", &text);
        self.toasts.push(Toast { text, error: true, at: Instant::now() });
        self.shell.ctx.request_repaint();
    }

    fn poll_shell_jobs(&mut self) {
        let results: Vec<Bg> = self.shell.bg_rx.try_iter().collect();
        results.into_iter().for_each(|bg| match bg {
            Bg::History(result) => {
                self.shell.history_jobs = self.shell.history_jobs.saturating_sub(1);
                if let Err(e) = result {
                    self.toast_error(e);
                }
            }
            Bg::HistoryOpened(result) => {
                self.shell.history_opening = false;
                match result {
                    Ok(Some(h)) => self.history = self.settings.history_enabled.then_some(h),
                    Ok(None) => self.shell.history_missing = true,
                    Err(e) => {
                        self.shell.history_missing = true;
                        self.toast_error(e);
                    }
                }
            }
            Bg::Backup { id, revision, path, result } => self.backup_done(id, revision, path, result),
            Bg::Git { generation, result } => match result {
                Ok(map) if generation == self.shell.git_generation => self.shell.git = map,
                Ok(_) => {}
                Err(e) => self.toast_error(e),
            },
        });
    }

    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        dropped.iter().for_each(|p| {
            self.open_path(p, crate::docs::auto_open(), None);
        });
    }

    fn background_busy(&self) -> bool {
        self.docs.values().any(|e| e.load.is_some() || e.save.is_some() || e.doc.line_index_progress().is_some())
            || self.shell.backup_jobs > 0
            || self.shell.history_jobs > 0
            || self.shell.history_opening
            || self.feat.output.has_running()
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.at.elapsed() < TOAST_LIFETIME);
        let dark = self.dark(ctx);
        let mut dismissed: Option<usize> = None;
        if !self.toasts.is_empty() {
            egui::Area::new(egui::Id::new("kladde_toasts"))
                .anchor(egui::Align2::RIGHT_TOP, [-12.0, 36.0])
                .order(egui::Order::Foreground)
                .interactable(true)
                .show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    self.toasts.iter().enumerate().for_each(|(i, t)| {
                        let left = TOAST_LIFETIME.saturating_sub(t.at.elapsed()).as_secs_f32();
                        let alpha = left.min(1.0);
                        let fill = match (t.error, dark) {
                            (true, true) => egui::Color32::from_rgb(0x6E, 0x1E, 0x1E),
                            (true, false) => egui::Color32::from_rgb(0xFD, 0xE2, 0xE2),
                            (false, true) => egui::Color32::from_rgb(0x2D, 0x3A, 0x4A),
                            (false, false) => egui::Color32::from_rgb(0xE3, 0xEE, 0xFA),
                        };
                        let response = egui::Frame::popup(ui.style())
                            .fill(fill.gamma_multiply(alpha))
                            .multiply_with_opacity(alpha)
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(&t.text).wrap());
                            })
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_text("Click to dismiss");
                        if response.clicked() {
                            dismissed = Some(i);
                        }
                    });
                });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if let Some(i) = dismissed {
            self.toasts.remove(i);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        self.shell.editor_focused = false;
        self.features_begin_frame(ctx);
        self.poll_shell_jobs();
        self.poll_history_setting();
        self.poll_loads();
        self.poll_saves();
        self.poll_documents();
        self.poll_backups();
        self.poll_update();
        self.handle_dropped(ctx);
        if let Some(tab) = self.feat.show_bottom.take() {
            self.shell.bottom_visible = true;
            self.shell.bottom_tab = tab;
        }
        egui::TopBottomPanel::top("kladde_menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| self.show_menu_bar(ui));
        });
        self.show_workspace_strip(ctx);
        self.show_side_panel(ctx);
        self.show_status_bar(ctx);
        self.show_bottom_panel(ctx);
        self.show_central(ctx);
        self.show_modals(ctx);
        self.show_ws_prompt(ctx);
        self.show_update_dialog(ctx);
        self.show_close_prompt(ctx);
        self.macro_record_events(ctx);
        self.show_toasts(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && !self.shell.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_quit(ctx);
            ctx.request_repaint();
        }
        self.continue_quit(ctx);
        let wake = if self.background_busy() { BUSY_REPAINT } else { IDLE_REPAINT };
        ctx.request_repaint_after(wake);
    }
}
