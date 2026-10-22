//! Self-update from GitHub releases: check (manual or at start when enabled), confirm, download, restart.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use kladde_update::Release;

use crate::commands::{poll_task, spawn_task};
use crate::state::App;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Points the updater at another manifest, e.g. a local test server.
const MANIFEST_ENV: &str = "KLADDE_UPDATE_MANIFEST";

/// Download progress shared with the worker thread.
#[derive(Default)]
pub struct DownloadProgress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

#[derive(Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking {
        rx: Receiver<Result<Option<Release>, String>>,
        manual: bool,
    },
    Available(Release),
    Installing {
        rx: Receiver<Result<PathBuf, String>>,
        progress: Arc<DownloadProgress>,
        version: String,
    },
}

fn manifest_url() -> String {
    std::env::var(MANIFEST_ENV).unwrap_or_else(|_| kladde_update::manifest_url())
}

impl App {
    pub fn update_busy(&self) -> bool {
        matches!(self.shell.update, UpdateState::Checking { .. } | UpdateState::Installing { .. })
    }

    /// At startup: remove what a previous update left behind, then check if the user enabled it.
    pub(crate) fn start_updates(&mut self) {
        if let Err(e) = kladde_update::cleanup_previous() {
            self.feat.output.add_message("Log", &format!("Cannot remove the previous version's file: {e}"));
        }
        if self.settings.check_updates_on_start {
            self.check_for_updates(false);
        }
    }

    /// `manual` checks report "up to date" and errors as toasts; checks at start only log failures.
    pub fn check_for_updates(&mut self, manual: bool) {
        if !self.update_busy() {
            let url = manifest_url();
            let rx = spawn_task(move || kladde_update::check(&url, VERSION).map_err(|e| e.to_string()));
            self.shell.update = UpdateState::Checking { rx, manual };
            if manual {
                self.toast_info("Checking for updates …");
            }
        }
    }

    pub(crate) fn poll_update(&mut self) {
        match std::mem::take(&mut self.shell.update) {
            UpdateState::Checking { rx, manual } => match (poll_task(&rx, "The update check"), manual) {
                (None, _) => self.shell.update = UpdateState::Checking { rx, manual },
                (Some(Ok(Ok(Some(release)))), _) => self.shell.update = UpdateState::Available(release),
                (Some(Ok(Ok(None))), true) => self.toast_info(format!("Kladde {VERSION} is up to date")),
                (Some(Ok(Ok(None))), false) => {}
                (Some(Ok(Err(e)) | Err(e)), true) => self.toast_error(format!("Update check failed: {e}")),
                (Some(Ok(Err(e)) | Err(e)), false) => {
                    self.feat.output.add_message("Log", &format!("Update check failed: {e}"));
                }
            },
            UpdateState::Installing { rx, progress, version } => match poll_task(&rx, "The update download") {
                None => self.shell.update = UpdateState::Installing { rx, progress, version },
                Some(Ok(Ok(exe))) => {
                    self.toast_info(format!("Kladde {version} is installed; restarting"));
                    self.shell.relaunch = Some(exe);
                    let ctx = self.shell.ctx.clone();
                    self.request_quit(&ctx);
                }
                Some(Ok(Err(e)) | Err(e)) if progress.cancel.load(Ordering::Relaxed) => {
                    self.toast_info(format!("Update cancelled ({e})"))
                }
                Some(Ok(Err(e)) | Err(e)) => self.toast_error(format!("Update failed: {e}")),
            },
            other => self.shell.update = other,
        }
    }

    fn install_update(&mut self, release: Release) {
        let progress = Arc::new(DownloadProgress::default());
        let shared = Arc::clone(&progress);
        let ctx = self.shell.ctx.clone();
        let version = release.version.clone();
        let rx = spawn_task(move || {
            let report = |done: u64, total: Option<u64>| {
                shared.done.store(done, Ordering::Relaxed);
                shared.total.store(total.unwrap_or(0), Ordering::Relaxed);
                ctx.request_repaint();
                !shared.cancel.load(Ordering::Relaxed)
            };
            kladde_update::install(&release, &report).map_err(|e| e.to_string())
        });
        self.shell.update = UpdateState::Installing { rx, progress, version };
    }

    /// Starts the new executable once the quit after an update has saved everything.
    pub(crate) fn relaunch_after_update(&mut self) {
        if let Some(exe) = self.shell.relaunch.take() {
            if let Err(e) = kladde_update::relaunch(&exe, &[]) {
                // The window is closing; the dialog is the only place the user can still read this.
                rfd::MessageDialog::new()
                    .set_title("Kladde")
                    .set_description(format!("Kladde was updated but could not restart: {e}. Start it again."))
                    .set_level(rfd::MessageLevel::Warning)
                    .show();
            }
        }
    }

    pub(crate) fn show_update_dialog(&mut self, ctx: &egui::Context) {
        let mut install = None;
        let mut later = false;
        match &self.shell.update {
            UpdateState::Available(release) => {
                egui::Window::new("Update available")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!("Kladde {} is available; you have {VERSION}.", release.version));
                        if !release.notes.trim().is_empty() {
                            ui.separator();
                            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                                ui.label(release.notes.trim());
                            });
                        }
                        ui.separator();
                        ui.weak("Installing saves your session, replaces this program file and restarts Kladde.");
                        ui.horizontal(|ui| {
                            if ui.button("Install and restart").clicked() {
                                install = Some(release.clone());
                            }
                            later = ui.button("Later").clicked();
                        });
                    });
            }
            UpdateState::Installing { progress, version, .. } => {
                let (done, total) = (progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
                egui::Window::new("Updating")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!("Downloading Kladde {version} …"));
                        let bar = match total {
                            0 => egui::ProgressBar::new(0.0).text(format!("{} KiB", done / 1024)).animate(true),
                            t => egui::ProgressBar::new(done as f32 / t as f32).show_percentage(),
                        };
                        ui.add(bar.desired_width(320.0));
                        if ui.button("Cancel").clicked() {
                            progress.cancel.store(true, Ordering::Relaxed);
                        }
                    });
            }
            UpdateState::Idle | UpdateState::Checking { .. } => {}
        }
        if let Some(release) = install {
            self.install_update(release);
        } else if later {
            self.shell.update = UpdateState::Idle;
        }
    }
}
