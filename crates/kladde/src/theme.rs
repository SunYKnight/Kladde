//! Light/dark appearance, widget themes and the window icon.

use egui::{Color32, ThemePreference};
use kladde_workspace::{Rgb, ThemeChoice};

use crate::state::App;

impl App {
    /// True when the current egui visuals are dark (System follows the OS).
    pub fn dark(&self, ctx: &egui::Context) -> bool {
        ctx.style().visuals.dark_mode
    }

    /// Applies the theme choice when it changed; egui then follows the OS for `System`.
    pub(crate) fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.shell.applied_theme != Some(self.settings.theme) {
            ctx.set_theme(match self.settings.theme {
                ThemeChoice::System => ThemePreference::System,
                ThemeChoice::Dark => ThemePreference::Dark,
                ThemeChoice::Light => ThemePreference::Light,
            });
            self.shell.applied_theme = Some(self.settings.theme);
        }
    }

    pub(crate) fn editor_theme(&self, ctx: &egui::Context) -> kladde_editor::EditorTheme {
        if self.dark(ctx) {
            kladde_editor::EditorTheme::dark()
        } else {
            kladde_editor::EditorTheme::light()
        }
    }

    pub(crate) fn hex_theme(&self, ctx: &egui::Context) -> kladde_hex::HexTheme {
        if self.dark(ctx) {
            kladde_hex::HexTheme::dark()
        } else {
            kladde_hex::HexTheme::light()
        }
    }
}

pub fn color32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Window icon; the same artwork is embedded in the Windows executable by `build.rs`.
pub fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/kladde.png")).expect("embedded icon is a valid PNG")
}
