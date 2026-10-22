#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod docs;
mod find;
mod layout;
mod macros;
mod menus;
mod run;
mod session;
mod state;
mod status;
mod theme;
mod tools;
mod update;
mod wsbar;

use std::path::PathBuf;

use kladde_workspace::Settings;

/// Initial window size when the settings have none.
const DEFAULT_WINDOW: [f32; 2] = [1000.0, 650.0];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = run();
    if let Err(e) = &result {
        // Release builds have no console; the dialog is the only place the user sees why startup failed.
        rfd::MessageDialog::new()
            .set_title("Kladde")
            .set_description(format!("Kladde could not start: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    result
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = kladde_workspace::data_dir()?;
    let settings = Settings::load(&data_dir);
    let size = settings.as_ref().ok().and_then(|s| s.window_size).unwrap_or(DEFAULT_WINDOW);
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let viewport = egui::ViewportBuilder::default()
        .with_title("Kladde")
        .with_inner_size(size)
        .with_min_inner_size([400.0, 300.0])
        .with_icon(theme::app_icon())
        .with_drag_and_drop(true);
    let options =
        eframe::NativeOptions { viewport, event_loop_builder: Some(Box::new(prefer_x11)), ..Default::default() };
    eframe::run_native(
        "Kladde",
        options,
        Box::new(move |cc| Ok(Box::new(state::App::new(cc, data_dir, settings, args)) as Box<dyn eframe::App>)),
    )?;
    Ok(())
}

/// winit 0.30 has no file drag & drop on Wayland; XWayland provides it. Pure Wayland sessions keep Wayland.
#[cfg(target_os = "linux")]
fn prefer_x11(builder: &mut eframe::EventLoopBuilder<eframe::UserEvent>) {
    use winit::platform::x11::EventLoopBuilderExtX11;
    if std::env::var_os("DISPLAY").is_some() {
        builder.with_x11();
    }
}

#[cfg(not(target_os = "linux"))]
fn prefer_x11(_builder: &mut eframe::EventLoopBuilder<eframe::UserEvent>) {}
