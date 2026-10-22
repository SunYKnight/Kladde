//! Light/dark appearance, widget themes and the window icon.

use egui::{Color32, ThemePreference};
use kladde_workspace::{Rgb, ThemeChoice};

use crate::state::App;

/// Window icon edge length in pixels.
const ICON_SIZE: u32 = 32;

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

/// A notepad page with ruled lines and a dark binding strip, drawn in code so no image file ships.
pub fn app_icon() -> egui::IconData {
    let n = ICON_SIZE;
    let rgba = (0..n * n)
        .flat_map(|i| {
            let (x, y) = (i % n, i / n);
            let inside = (3..29).contains(&x) && (2..30).contains(&y);
            let pixel: [u8; 4] = if !inside {
                [0, 0, 0, 0]
            } else if x < 8 {
                [0x2B, 0x4C, 0x7E, 0xFF]
            } else if x == 3 || x == 28 || y == 2 || y == 29 {
                [0x40, 0x40, 0x40, 0xFF]
            } else if y >= 8 && y % 5 == 3 && x > 10 && x < 26 {
                [0x56, 0x9C, 0xD6, 0xFF]
            } else {
                [0xF5, 0xF1, 0xE6, 0xFF]
            };
            pixel
        })
        .collect();
    egui::IconData { rgba, width: n, height: n }
}
