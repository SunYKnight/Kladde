use egui::{DragValue, Event, Id, Key, Modifiers};
use kladde_workspace::{Settings, ThemeChoice};

use crate::commands::{commands, Chord, Keymap};

fn capture_id() -> Id {
    Id::new("kladde_settings_capture")
}

/// True while the settings dialog waits for a key chord; the app should skip its own shortcuts then.
pub fn settings_capturing(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<String>(capture_id())).is_some()
}

enum BindingEdit {
    Capture(&'static str),
    Clear(&'static str),
    Reset(&'static str),
}

/// Applies a captured chord; a chord equal to the default drops the override.
fn bind(settings: &mut Settings, id: &str, chord: Chord) {
    let default = commands().iter().find(|c| c.id == id).and_then(|c| c.default_chord).and_then(Chord::parse);
    if default == Some(chord) {
        settings.keybindings.remove(id);
    } else {
        settings.keybindings.insert(id.to_string(), chord.to_string());
    }
}

/// Takes the next key press while capturing; Esc without modifiers cancels.
fn poll_capture(ctx: &egui::Context, settings: &mut Settings) -> bool {
    let capturing: Option<String> = ctx.data(|d| d.get_temp(capture_id()));
    let pressed = capturing.as_ref().and_then(|_| {
        ctx.input_mut(|i| {
            let hit = i.events.iter().find_map(|e| match e {
                Event::Key { key, pressed: true, modifiers, .. } => Some((*modifiers, *key)),
                _ => None,
            });
            hit.inspect(|(m, k)| {
                i.consume_key(*m, *k);
            })
        })
    });
    match (capturing, pressed) {
        (Some(id), Some((m, k))) => {
            ctx.data_mut(|d| d.remove::<String>(capture_id()));
            let cancel = k == Key::Escape && m == Modifiers::NONE;
            if !cancel {
                bind(settings, &id, Chord::from_event(m, k));
            }
            !cancel
        }
        _ => false,
    }
}

fn editor_section(ui: &mut egui::Ui, s: &mut Settings) -> bool {
    let mut changed = false;
    egui::Grid::new("kladde_settings_editor").num_columns(2).show(ui, |ui| {
        ui.label("Font size");
        changed |= ui.add(DragValue::new(&mut s.font_size).range(6.0..=48.0).speed(0.25)).changed();
        ui.end_row();
        ui.label("Tab width");
        changed |= ui.add(DragValue::new(&mut s.tab_width).range(1..=16)).changed();
        ui.end_row();
        ui.label("");
        changed |= ui.checkbox(&mut s.insert_spaces, "Insert spaces instead of tabs").changed();
        ui.end_row();
        ui.label("");
        changed |= ui.checkbox(&mut s.show_whitespace, "Show whitespace").changed();
        ui.end_row();
        ui.label("");
        changed |= ui.checkbox(&mut s.word_wrap, "Word wrap").changed();
        ui.end_row();
        ui.label("");
        changed |= ui.checkbox(&mut s.show_line_numbers, "Show line numbers").changed();
        ui.end_row();
    });
    changed
}

fn large_files_section(ui: &mut egui::Ui, s: &mut Settings) -> bool {
    ui.horizontal(|ui| {
        ui.label("Plain mode (no highlighting, wrap, folds) above");
        let r = ui.add(DragValue::new(&mut s.large_file_mb).range(1..=65_536).suffix(" MB"));
        r.changed()
    })
    .inner
}

fn backups_section(ui: &mut egui::Ui, s: &mut Settings) -> bool {
    let mut changed = false;
    egui::Grid::new("kladde_settings_backup").num_columns(2).show(ui, |ui| {
        ui.label("Backup unsaved changes every");
        changed |= ui.add(DragValue::new(&mut s.backup_interval_secs).range(5..=3600).suffix(" s")).changed();
        ui.end_row();
        ui.label("");
        changed |= ui.checkbox(&mut s.history_enabled, "Keep file history on save").changed();
        ui.end_row();
        ui.label("History size limit");
        let limit = DragValue::new(&mut s.history_max_mb).range(1..=100_000).suffix(" MB");
        changed |= ui.add_enabled(s.history_enabled, limit).changed();
        ui.end_row();
    });
    changed
}

fn hex_section(ui: &mut egui::Ui, s: &mut Settings) -> bool {
    let mut changed = false;
    egui::Grid::new("kladde_settings_hex").num_columns(2).show(ui, |ui| {
        ui.label("Bytes per row");
        egui::ComboBox::from_id_salt("kladde_settings_hex_row").selected_text(s.hex_bytes_per_row.to_string()).show_ui(
            ui,
            |ui| {
                [8u8, 16, 24, 32, 48, 64].into_iter().for_each(|n| {
                    changed |= ui.selectable_value(&mut s.hex_bytes_per_row, n, n.to_string()).changed();
                });
            },
        );
        ui.end_row();
        ui.label("Group bytes by");
        egui::ComboBox::from_id_salt("kladde_settings_hex_group").selected_text(s.hex_group.to_string()).show_ui(
            ui,
            |ui| {
                [1u8, 2, 4, 8].into_iter().for_each(|n| {
                    changed |= ui.selectable_value(&mut s.hex_group, n, n.to_string()).changed();
                });
            },
        );
        ui.end_row();
    });
    changed
}

fn appearance_section(ui: &mut egui::Ui, s: &mut Settings) -> bool {
    ui.horizontal(|ui| {
        [(ThemeChoice::System, "System"), (ThemeChoice::Dark, "Dark"), (ThemeChoice::Light, "Light")]
            .into_iter()
            .fold(false, |changed, (t, label)| ui.radio_value(&mut s.theme, t, label).changed() || changed)
    })
    .inner
}

fn bindings_section(ui: &mut egui::Ui, s: &mut Settings) -> bool {
    let filter_id = Id::new("kladde_settings_filter");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    let capturing: Option<String> = ui.data(|d| d.get_temp(capture_id()));
    let keymap = Keymap::new(&s.keybindings);
    keymap.errors().iter().for_each(|e| {
        ui.colored_label(ui.visuals().error_fg_color, e);
    });
    ui.horizontal(|ui| {
        ui.label("Filter");
        ui.text_edit_singleline(&mut filter);
    });
    let needle = filter.to_lowercase();
    let mut edit = None;
    egui::Grid::new("kladde_settings_keys_grid").num_columns(4).striped(true).show(ui, |ui| {
        commands()
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.label.to_lowercase().contains(&needle)
                    || c.id.contains(&needle)
                    || c.category.to_lowercase().contains(&needle)
            })
            .for_each(|c| {
                ui.label(format!("{}: {}", c.category, c.label)).on_hover_text(c.id);
                let chord = keymap.chord_for(c.id);
                let conflict = chord.and_then(|ch| keymap.command_for(ch)).filter(|other| *other != c.id);
                ui.horizontal(|ui| {
                    ui.monospace(chord.map_or("—".to_string(), |ch| ch.to_string()));
                    if let Some(other) = conflict {
                        ui.colored_label(ui.visuals().warn_fg_color, "⚠")
                            .on_hover_text(format!("Also bound to {other}"));
                    }
                });
                let waiting = capturing.as_deref() == Some(c.id);
                let set_label = if waiting { "Press keys… (Esc cancels)" } else { "Set…" };
                if ui.selectable_label(waiting, set_label).clicked() {
                    edit = Some(BindingEdit::Capture(c.id));
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(chord.is_some(), egui::Button::new("Clear")).clicked() {
                        edit = Some(BindingEdit::Clear(c.id));
                    }
                    if ui.add_enabled(s.keybindings.contains_key(c.id), egui::Button::new("Reset")).clicked() {
                        edit = Some(BindingEdit::Reset(c.id));
                    }
                });
                ui.end_row();
            });
    });
    ui.data_mut(|d| d.insert_temp(filter_id, filter));
    match edit {
        Some(BindingEdit::Capture(id)) => {
            ui.data_mut(|d| d.insert_temp(capture_id(), id.to_string()));
            false
        }
        Some(BindingEdit::Clear(id)) => {
            s.keybindings.insert(id.to_string(), String::new());
            true
        }
        Some(BindingEdit::Reset(id)) => {
            s.keybindings.remove(id);
            true
        }
        None => false,
    }
}

/// Settings window; returns true if `settings` changed this frame.
pub fn show_settings(ctx: &egui::Context, open: &mut bool, settings: &mut Settings) -> bool {
    let mut changed = *open && poll_capture(ctx, settings);
    let section = |ui: &mut egui::Ui, title: &str, s: &mut Settings, f: fn(&mut egui::Ui, &mut Settings) -> bool| {
        egui::CollapsingHeader::new(title).default_open(true).show(ui, |ui| f(ui, s)).body_returned.unwrap_or(false)
    };
    egui::Window::new("Settings").open(open).default_size([620.0, 560.0]).vscroll(true).show(ctx, |ui| {
        changed |= section(ui, "Editor", settings, editor_section);
        changed |= section(ui, "Large files", settings, large_files_section);
        changed |= section(ui, "Backups & history", settings, backups_section);
        changed |= section(ui, "Hex", settings, hex_section);
        changed |= section(ui, "Appearance", settings, appearance_section);
        changed |= section(ui, "Key bindings", settings, bindings_section);
    });
    if !*open {
        ctx.data_mut(|d| d.remove::<String>(capture_id()));
    }
    changed
}
