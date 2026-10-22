use egui::{Key, RichText, TextEdit};
use kladde_core::SearchPattern;

const HISTORY_MAX: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindScope {
    Document,
    OpenFiles,
    Workspace,
}

impl FindScope {
    fn label(self) -> &'static str {
        match self {
            FindScope::Document => "Document",
            FindScope::OpenFiles => "Open files",
            FindScope::Workspace => "Workspace",
        }
    }
}

pub struct FindState {
    pub query: String,
    pub replacement: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub bytes: bool,
    pub scope: FindScope,
    pub show_replace: bool,
    open: bool,
    focus_query: bool,
    history: Vec<String>,
}

impl Default for FindState {
    fn default() -> Self {
        FindState::new()
    }
}

impl FindState {
    pub fn new() -> Self {
        FindState {
            query: String::new(),
            replacement: String::new(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            bytes: false,
            scope: FindScope::Document,
            show_replace: false,
            open: false,
            focus_query: false,
            history: Vec::new(),
        }
    }

    /// Shows the bar and focuses the query; a single-line `seed` (the selection) replaces the query.
    pub fn open(&mut self, replace: bool, seed: &str) {
        self.open = true;
        self.show_replace = replace;
        self.focus_query = true;
        if !seed.is_empty() && !seed.contains(['\n', '\r']) {
            self.query = seed.to_string();
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Recent queries, newest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// The search described by the bar; `bytes` parses hex such as "DE AD", "dead" or "0xDE 0xAD".
    pub fn pattern(&self) -> Result<SearchPattern, String> {
        if self.query.is_empty() {
            Err("empty search".to_string())
        } else if self.bytes {
            parse_hex(&self.query).map(SearchPattern::Bytes)
        } else {
            Ok(SearchPattern::Text {
                pattern: self.query.clone(),
                regex: self.regex,
                case_sensitive: self.case_sensitive,
                whole_word: self.whole_word,
            })
        }
    }

    fn remember_query(&mut self) {
        if !self.query.is_empty() {
            self.history.retain(|q| *q != self.query);
            self.history.insert(0, self.query.clone());
            self.history.truncate(HISTORY_MAX);
        }
    }
}

/// Whitespace-separated groups of hex digit pairs, each optionally prefixed with `0x`.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.split_whitespace().try_fold(Vec::new(), |mut out, token| {
        let digits = token.strip_prefix("0x").or_else(|| token.strip_prefix("0X")).unwrap_or(token);
        if digits.is_empty() || digits.len() % 2 != 0 {
            Err(format!("\"{token}\" is not a whole number of hex bytes"))
        } else {
            digits.as_bytes().chunks(2).try_for_each(|pair| {
                let s = std::str::from_utf8(pair).map_err(|_| format!("\"{token}\" is not hex"))?;
                out.push(u8::from_str_radix(s, 16).map_err(|_| format!("\"{token}\" is not hex"))?);
                Ok::<(), String>(())
            })?;
            Ok(out)
        }
    })?;
    if bytes.is_empty() {
        Err("empty search".to_string())
    } else {
        Ok(bytes)
    }
}

fn toggle(ui: &mut egui::Ui, value: &mut bool, text: &str, tip: &str, enabled: bool) {
    let response = ui.add_enabled(enabled, egui::SelectableLabel::new(*value, RichText::new(text).monospace()));
    if response.on_hover_text(tip).clicked() {
        *value = !*value;
    }
}

/// Find/replace bar. Enter = Next, Shift+Enter = Prev, Esc = Close (while a field has focus).
pub fn show_find_bar(ui: &mut egui::Ui, state: &mut FindState, status: &str) -> Vec<FindAction> {
    let mut actions = Vec::new();
    let query_id = ui.id().with("kladde_find_query");
    ui.horizontal(|ui| {
        let arrow = if state.show_replace { "⏷" } else { "⏵" };
        if ui.small_button(arrow).on_hover_text("Toggle replace").clicked() {
            state.show_replace = !state.show_replace;
        }
        let hint = if state.bytes { "hex bytes, e.g. DE AD BE EF" } else { "Find" };
        let query = ui.add(TextEdit::singleline(&mut state.query).id(query_id).hint_text(hint).desired_width(260.0));
        if std::mem::take(&mut state.focus_query) {
            query.request_focus();
        }
        if query.lost_focus() {
            ui.input(|i| {
                if i.key_pressed(Key::Enter) {
                    actions.push(if i.modifiers.shift { FindAction::Prev } else { FindAction::Next });
                } else if i.key_pressed(Key::Escape) {
                    actions.push(FindAction::Close);
                }
            });
            if actions.iter().any(|a| matches!(a, FindAction::Next | FindAction::Prev)) {
                query.request_focus();
            }
        }
        ui.add_enabled_ui(!state.history.is_empty(), |ui| {
            ui.menu_button("🕘", |ui| {
                let chosen = state.history.iter().find(|q| ui.button(q.as_str()).clicked()).cloned();
                if let Some(q) = chosen {
                    state.query = q;
                    ui.close_menu();
                }
            })
            .response
            .on_hover_text("Recent searches");
        });
        let text_mode = !state.bytes;
        toggle(ui, &mut state.case_sensitive, "Aa", "Match case", text_mode);
        toggle(ui, &mut state.whole_word, "W", "Whole word", text_mode);
        toggle(ui, &mut state.regex, ".*", "Regular expression", text_mode);
        toggle(ui, &mut state.bytes, "0x", "Search hex bytes", true);
        egui::ComboBox::from_id_salt(ui.id().with("kladde_find_scope")).selected_text(state.scope.label()).show_ui(
            ui,
            |ui| {
                [FindScope::Document, FindScope::OpenFiles, FindScope::Workspace].into_iter().for_each(|s| {
                    ui.selectable_value(&mut state.scope, s, s.label());
                });
            },
        );
        if ui.button("◀").on_hover_text("Previous (Shift+Enter)").clicked() {
            actions.push(FindAction::Prev);
        }
        if ui.button("▶").on_hover_text("Next (Enter)").clicked() {
            actions.push(FindAction::Next);
        }
        if ui.button("Find all").clicked() {
            actions.push(FindAction::FindAll);
        }
        ui.label(status);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✖").on_hover_text("Close (Esc)").clicked() {
                actions.push(FindAction::Close);
            }
        });
    });
    if state.show_replace {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().interact_size.y + ui.spacing().item_spacing.x);
            let field = ui.add(TextEdit::singleline(&mut state.replacement).hint_text("Replace").desired_width(260.0));
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                actions.push(FindAction::Close);
            }
            if ui.button("Replace").clicked() {
                actions.push(FindAction::ReplaceOne);
            }
            if ui.button("Replace all").clicked() {
                actions.push(FindAction::ReplaceAll);
            }
        });
    }
    if actions.iter().any(|a| *a != FindAction::Close) {
        state.remember_query();
    }
    if actions.contains(&FindAction::Close) {
        state.open = false;
    }
    actions
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindAction {
    Next,
    Prev,
    ReplaceOne,
    ReplaceAll,
    FindAll,
    Close,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_state(query: &str) -> FindState {
        let mut s = FindState::new();
        s.bytes = true;
        s.query = query.to_string();
        s
    }

    #[test]
    fn hex_pattern_forms() {
        let want = vec![0xDE, 0xAD, 0xBE, 0xEF];
        ["DE AD BE EF", "deadbeef", " dead  BEEF ", "0xDE 0xad 0XBE EF"].iter().for_each(|q| {
            match bytes_state(q).pattern() {
                Ok(SearchPattern::Bytes(b)) => assert_eq!(b, want, "{q}"),
                other => panic!("{q}: {other:?}"),
            }
        });
    }

    #[test]
    fn hex_pattern_rejects_bad_input() {
        ["D", "DEA", "DE A", "XY", "0x", "   ", "dé"].iter().for_each(|q| {
            assert!(bytes_state(q).pattern().is_err(), "{q:?} must be rejected");
        });
    }

    #[test]
    fn text_pattern_carries_flags() {
        let mut s = FindState::new();
        s.query = "a.b".to_string();
        s.regex = true;
        s.whole_word = true;
        match s.pattern() {
            Ok(SearchPattern::Text { pattern, regex: true, case_sensitive: false, whole_word: true }) => {
                assert_eq!(pattern, "a.b")
            }
            other => panic!("{other:?}"),
        }
        assert!(FindState::new().pattern().is_err());
    }
}
