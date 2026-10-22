use egui::{Align2, Key, Modifiers, TextEdit};
use kladde_workspace::fuzzy_rank;

const SHOWN: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub id: String,
    pub label: String,
    pub detail: String,
}

pub struct PaletteState {
    open: bool,
    title: String,
    query: String,
    selected: usize,
    focus: bool,
}

impl Default for PaletteState {
    fn default() -> Self {
        PaletteState::new()
    }
}

impl PaletteState {
    pub fn new() -> Self {
        PaletteState { open: false, title: String::new(), query: String::new(), selected: 0, focus: false }
    }

    /// Opens with an empty query; `title` is the input hint ("Command", "Go to file", ...).
    pub fn open(&mut self, title: &str) {
        *self = PaletteState { open: true, title: title.to_string(), focus: true, ..PaletteState::new() };
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}

/// Indices into `items`, best first; empty query keeps the given order.
fn ranked(query: &str, items: &[PaletteItem]) -> Vec<usize> {
    if query.trim().is_empty() {
        (0..items.len().min(SHOWN)).collect()
    } else {
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        fuzzy_rank(query.trim(), &labels, SHOWN).into_iter().map(|(i, _)| i).collect()
    }
}

/// Modal list at top center, fuzzy ranked; Up/Down/Enter/Esc. Returns the chosen item id.
pub fn show_palette(ctx: &egui::Context, state: &mut PaletteState, items: &[PaletteItem]) -> Option<String> {
    let mut chosen = None;
    if state.open {
        let order = ranked(&state.query, items);
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let last = order.len().saturating_sub(1);
        state.selected = state.selected.min(last);
        if up {
            state.selected = state.selected.saturating_sub(1);
        }
        if down {
            state.selected = (state.selected + 1).min(last);
        }
        if enter {
            chosen = order.get(state.selected).map(|&i| items[i].id.clone());
        }
        let width = (ctx.screen_rect().width() * 0.6).clamp(300.0, 640.0);
        let shown = egui::Window::new("kladde_palette")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 40.0])
            .default_width(width)
            .show(ctx, |ui| {
                let before = state.query.clone();
                let edit = ui.add(TextEdit::singleline(&mut state.query).hint_text(&state.title).desired_width(width));
                if std::mem::take(&mut state.focus) || !edit.has_focus() {
                    edit.request_focus();
                }
                if state.query != before {
                    state.selected = 0;
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(400.0).auto_shrink([false, true]).show(ui, |ui| {
                    order.iter().enumerate().for_each(|(row, &i)| {
                        let item = &items[i];
                        let response = ui
                            .horizontal(|ui| {
                                let r = ui.selectable_label(row == state.selected, &item.label);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.weak(&item.detail);
                                });
                                r
                            })
                            .inner;
                        if row == state.selected && (up || down) {
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            chosen = Some(item.id.clone());
                        }
                    });
                    if order.is_empty() {
                        ui.weak("No matches");
                    }
                });
            });
        let clicked_outside = shown.is_some_and(|r| r.response.clicked_elsewhere());
        if esc || chosen.is_some() || clicked_outside {
            state.open = false;
        }
    }
    chosen
}
