use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use egui::{Event, Key, Modifiers};
use kladde_core::TextEncoding;
use kladde_tools::LineTool;

/// A command the app can run from the menu, the palette or a key binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub default_chord: Option<&'static str>,
}

const fn cmd(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    default_chord: Option<&'static str>,
) -> CommandSpec {
    CommandSpec { id, label, category, default_chord }
}

const FIXED_HEAD: &[CommandSpec] = &[
    cmd("file.new", "New", "File", Some("Ctrl+N")),
    cmd("file.open", "Open…", "File", Some("Ctrl+O")),
    cmd("file.open_hex", "Open as hex…", "File", None),
    cmd("file.save", "Save", "File", Some("Ctrl+S")),
    cmd("file.save_as", "Save as…", "File", Some("Ctrl+Alt+S")),
    cmd("file.save_all", "Save all", "File", Some("Ctrl+Shift+S")),
    cmd("file.close", "Close", "File", Some("Ctrl+W")),
    cmd("file.close_all", "Close all", "File", None),
    cmd("file.reload", "Reload from disk", "File", None),
    cmd("file.recent", "Recent files…", "File", None),
    cmd("file.open_readonly", "Open read-only…", "File", None),
    cmd("edit.undo", "Undo", "Edit", Some("Ctrl+Z")),
    cmd("edit.redo", "Redo", "Edit", Some("Ctrl+Y")),
    cmd("edit.cut", "Cut", "Edit", None),
    cmd("edit.copy", "Copy", "Edit", None),
    cmd("edit.paste", "Paste", "Edit", None),
    cmd("edit.select_all", "Select all", "Edit", None),
    cmd("edit.duplicate_line", "Duplicate line", "Edit", Some("Ctrl+D")),
    cmd("edit.delete_line", "Delete line", "Edit", Some("Ctrl+Shift+L")),
    cmd("edit.toggle_comment", "Toggle comment", "Edit", Some("Ctrl+Q")),
    cmd("search.find", "Find…", "Search", Some("Ctrl+F")),
    cmd("search.replace", "Replace…", "Search", Some("Ctrl+H")),
    cmd("search.find_next", "Find next", "Search", Some("F3")),
    cmd("search.find_prev", "Find previous", "Search", Some("Shift+F3")),
    cmd("search.find_in_files", "Find in files…", "Search", Some("Ctrl+Shift+F")),
    cmd("search.goto", "Go to line…", "Search", Some("Ctrl+G")),
    cmd("search.goto_offset", "Go to offset…", "Search", None),
    cmd("bookmark.toggle", "Toggle bookmark", "Bookmarks", Some("Ctrl+F2")),
    cmd("bookmark.next", "Next bookmark", "Bookmarks", Some("F2")),
    cmd("bookmark.prev", "Previous bookmark", "Bookmarks", Some("Shift+F2")),
    cmd("view.toggle_hex", "Toggle hex view", "View", Some("Ctrl+Shift+H")),
    cmd("view.word_wrap", "Word wrap", "View", None),
    cmd("view.whitespace", "Show whitespace", "View", None),
    cmd("view.line_numbers", "Show line numbers", "View", None),
    cmd("view.split_vertical", "Split vertically", "View", None),
    cmd("view.split_horizontal", "Split horizontally", "View", None),
    cmd("view.unsplit", "Unsplit", "View", None),
    cmd("view.fold_all", "Fold all", "View", Some("Alt+0")),
    cmd("view.unfold_all", "Unfold all", "View", Some("Alt+Shift+0")),
    cmd("view.zoom_in", "Zoom in", "View", Some("Ctrl+Plus")),
    cmd("view.zoom_out", "Zoom out", "View", Some("Ctrl+Minus")),
    cmd("view.zoom_reset", "Reset zoom", "View", Some("Ctrl+0")),
    cmd("view.tail", "Follow file (tail -f)", "View", None),
    cmd("view.theme_toggle", "Toggle dark/light theme", "View", None),
    cmd("view.workspace_panel", "Workspace panel", "View", None),
    cmd("view.output_panel", "Output panel", "View", None),
    cmd("view.csv_table", "Show as CSV table", "View", None),
    cmd("view.diff_files", "Compare files…", "View", None),
    cmd("palette.commands", "Command palette", "Palette", Some("Ctrl+Shift+P")),
    cmd("palette.files", "Go to file", "Palette", Some("Ctrl+P")),
    cmd("workspace.new", "New workspace…", "Workspace", None),
    cmd("workspace.rename", "Rename workspace…", "Workspace", None),
    cmd("workspace.delete", "Delete workspace…", "Workspace", None),
    cmd("workspace.switch", "Switch workspace…", "Workspace", Some("Ctrl+Alt+W")),
    cmd("workspace.add_current_file", "Add current file to workspace", "Workspace", None),
    cmd("run.fence_at_caret", "Run code block at caret", "Run", Some("Ctrl+Enter")),
    cmd("run.selection", "Run selection", "Run", Some("F5")),
    cmd("run.file", "Run file", "Run", Some("Ctrl+F5")),
    cmd("run.in_terminal", "Run in terminal", "Run", Some("Shift+F5")),
    cmd("run.stop", "Stop running process", "Run", Some("Ctrl+Shift+F5")),
    cmd("quarto.render", "Quarto: render", "Quarto", Some("Ctrl+Shift+K")),
    cmd("quarto.render_html", "Quarto: render HTML", "Quarto", None),
    cmd("quarto.render_pdf", "Quarto: render PDF", "Quarto", None),
    cmd("quarto.render_docx", "Quarto: render Word", "Quarto", None),
    cmd("history.show", "Show file history", "History", None),
    cmd("history.snapshot_now", "Snapshot file now", "History", None),
    cmd("tools.hash", "Checksums (MD5, SHA, CRC32)…", "Tools", None),
    cmd("tools.json_pretty", "JSON: pretty print", "Tools", None),
    cmd("tools.json_minify", "JSON: minify", "Tools", None),
    cmd("tools.xml_pretty", "XML: pretty print", "Tools", None),
    cmd("tools.xml_minify", "XML: minify", "Tools", None),
    cmd("tools.base64_encode", "Base64: encode", "Tools", None),
    cmd("tools.base64_decode", "Base64: decode", "Tools", None),
    cmd("tools.url_encode", "URL: encode", "Tools", None),
    cmd("tools.url_decode", "URL: decode", "Tools", None),
    cmd("tools.hex_encode", "Hex: encode", "Tools", None),
    cmd("tools.hex_decode", "Hex: decode", "Tools", None),
];

const FIXED_TAIL: &[CommandSpec] = &[
    cmd("eol.lf", "Line endings: LF (Unix)", "Line endings", None),
    cmd("eol.crlf", "Line endings: CRLF (Windows)", "Line endings", None),
    cmd("eol.cr", "Line endings: CR (classic Mac)", "Line endings", None),
    cmd("settings.open", "Settings…", "Settings", Some("Ctrl+Comma")),
    cmd("macro.record", "Start/stop macro recording", "Macro", Some("Ctrl+Shift+R")),
    cmd("macro.play", "Play macro", "Macro", Some("Ctrl+Shift+M")),
    cmd("help.check_updates", "Check for updates…", "Help", None),
    cmd("app.quit", "Exit", "App", Some("Alt+F4")),
];

/// "SortAscCi" -> "sort_asc_ci".
fn snake_case(name: &str) -> String {
    name.chars().enumerate().fold(String::new(), |mut out, (i, c)| {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
        out
    })
}

/// Command id of a line tool, e.g. `lines.sort_asc`.
pub fn line_tool_command(tool: LineTool) -> String {
    format!("lines.{}", snake_case(&format!("{tool:?}")))
}

/// Command id of an encoding choice, e.g. `encoding.utf8`, `encoding.utf16le_bom`, `encoding.windows1252`.
pub fn encoding_command(enc: TextEncoding) -> String {
    let name: String =
        enc.name().to_lowercase().chars().filter(|c| *c != '-').map(|c| if c == ' ' { '_' } else { c }).collect();
    format!("encoding.{name}")
}

/// The line tool a `lines.*` command id stands for.
pub fn line_tool_for_command(id: &str) -> Option<LineTool> {
    LineTool::all().iter().copied().find(|t| line_tool_command(*t) == id)
}

/// The encoding an `encoding.*` command id stands for.
pub fn encoding_for_command(id: &str) -> Option<TextEncoding> {
    TextEncoding::choices().into_iter().find(|e| encoding_command(*e) == id)
}

/// Generated (id, label) pairs; owned by a static so `CommandSpec` can borrow them for `'static`.
static GENERATED: LazyLock<Vec<(String, String, &'static str)>> = LazyLock::new(|| {
    let lines = LineTool::all().iter().map(|t| (line_tool_command(*t), t.label().to_string(), "Lines"));
    let encodings = TextEncoding::choices()
        .into_iter()
        .map(|e| (encoding_command(e), format!("Encoding: {}", e.name()), "Encoding"));
    lines.chain(encodings).collect()
});

static COMMANDS: LazyLock<Vec<CommandSpec>> = LazyLock::new(|| {
    let generated = GENERATED.iter().map(|(id, label, category)| CommandSpec {
        id: id.as_str(),
        label: label.as_str(),
        category,
        default_chord: None,
    });
    FIXED_HEAD.iter().cloned().chain(generated).chain(FIXED_TAIL.iter().cloned()).collect()
});

/// Every command of the app, in menu order.
pub fn commands() -> &'static [CommandSpec] {
    &COMMANDS
}

/// A key with modifiers; Ctrl means Cmd on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: Key,
}

fn parse_key(name: &str) -> Option<Key> {
    Key::from_name(name).or_else(|| Key::ALL.iter().copied().find(|k| k.name().eq_ignore_ascii_case(name)))
}

impl Chord {
    /// Parses "Ctrl+Shift+P", "F3", "Ctrl+Plus", "Ctrl++", "alt+shift+0"; modifiers in any order.
    pub fn parse(s: &str) -> Option<Chord> {
        let s = s.trim();
        let (mods, key) = match s.strip_suffix("++") {
            Some(head) => (Some(head), "+"),
            None => match s.rsplit_once('+') {
                Some((head, key)) => (Some(head), key.trim()),
                None => (None, s),
            },
        };
        let empty = Chord { ctrl: false, shift: false, alt: false, key: parse_key(key)? };
        mods.map_or(Some(empty), |m| {
            m.split('+').try_fold(empty, |c, token| match token.trim().to_ascii_lowercase().as_str() {
                // A repeated modifier is a typo, not a chord.
                "ctrl" | "control" | "cmd" | "command" if !c.ctrl => Some(Chord { ctrl: true, ..c }),
                "shift" if !c.shift => Some(Chord { shift: true, ..c }),
                "alt" | "option" if !c.alt => Some(Chord { alt: true, ..c }),
                _ => None,
            })
        })
    }

    pub fn modifiers(&self) -> Modifiers {
        let mut m = Modifiers::NONE;
        m.alt = self.alt;
        m.shift = self.shift;
        m.ctrl = self.ctrl && !cfg!(target_os = "macos");
        m.mac_cmd = self.ctrl && cfg!(target_os = "macos");
        m.command = self.ctrl;
        m
    }

    /// The chord a key press with `mods` represents.
    pub fn from_event(mods: Modifiers, key: Key) -> Chord {
        Chord { ctrl: mods.command || mods.ctrl, shift: mods.shift, alt: mods.alt, key }
    }
}

impl fmt::Display for Chord {
    /// Canonical form "Ctrl+Alt+Shift+Key" that `parse` reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mods = [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+")];
        mods.iter().filter(|(on, _)| *on).try_for_each(|(_, s)| f.write_str(s))?;
        f.write_str(self.key.name())
    }
}

/// Keys whose symbol needs Shift on some layouts (US `+` is Shift+=).
fn shift_produced(key: Key) -> bool {
    matches!(key, Key::Plus | Key::Colon | Key::Questionmark | Key::Pipe)
}

/// Effective key bindings: defaults merged with user overrides.
pub struct Keymap {
    bindings: Vec<(&'static str, Option<Chord>)>,
    errors: Vec<String>,
}

impl Keymap {
    /// `overrides` maps command ids to chord strings; an empty string unbinds the command.
    pub fn new(overrides: &BTreeMap<String, String>) -> Keymap {
        let mut errors: Vec<String> = overrides
            .keys()
            .filter(|id| !commands().iter().any(|c| c.id == id.as_str()))
            .map(|id| format!("unknown command in key bindings: {id}"))
            .collect();
        let bindings = commands()
            .iter()
            .map(|c| {
                let chord = match overrides.get(c.id).map(|s| s.trim()) {
                    Some("") => None,
                    Some(s) => Chord::parse(s).or_else(|| {
                        errors.push(format!("invalid key binding for {}: {s:?}", c.id));
                        c.default_chord.and_then(Chord::parse)
                    }),
                    None => c.default_chord.and_then(Chord::parse),
                };
                (c.id, chord)
            })
            .collect();
        Keymap { bindings, errors }
    }

    pub fn chord_for(&self, id: &str) -> Option<Chord> {
        self.bindings.iter().find(|(i, _)| *i == id).and_then(|(_, c)| *c)
    }

    /// Overrides that could not be applied (unknown ids, unparsable chords).
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The command bound to `chord`; the first in menu order wins on conflicts.
    pub fn command_for(&self, chord: Chord) -> Option<&'static str> {
        self.bindings.iter().find(|(_, c)| *c == Some(chord)).map(|(id, _)| *id)
    }

    fn lookup(&self, mods: Modifiers, key: Key) -> Option<&'static str> {
        let chord = Chord::from_event(mods, key);
        self.command_for(chord).or_else(|| {
            (chord.shift && shift_produced(key)).then(|| self.command_for(Chord { shift: false, ..chord }))?
        })
    }

    /// Commands whose chord was pressed this frame; their key events are consumed.
    pub fn take_triggered(&self, ctx: &egui::Context) -> Vec<&'static str> {
        ctx.input_mut(|i| {
            let hits: Vec<(Modifiers, Key, &'static str)> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    Event::Key { key, pressed: true, modifiers, .. } => {
                        self.lookup(*modifiers, *key).map(|id| (*modifiers, *key, id))
                    }
                    _ => None,
                })
                .collect();
            // consume_key also removes repeats, so each hit counts once per frame.
            hits.into_iter().filter(|(m, k, _)| i.consume_key(*m, *k)).map(|(_, _, id)| id).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn chord_round_trip() {
        ["Ctrl+Shift+P", "F3", "Ctrl+Plus", "Ctrl+Comma", "Alt+Shift+0", "Ctrl+Alt+S", "Shift+F5", "Ctrl+Enter"]
            .iter()
            .for_each(|s| {
                let c = Chord::parse(s).unwrap_or_else(|| panic!("{s} must parse"));
                assert_eq!(c.to_string(), *s);
                assert_eq!(Chord::parse(&c.to_string()), Some(c));
            });
        assert_eq!(Chord::parse("ctrl + shift + p"), Chord::parse("Ctrl+Shift+P"));
        assert_eq!(Chord::parse("Ctrl++"), Chord::parse("Ctrl+Plus"));
        assert_eq!(Chord::parse("Shift+Ctrl+f3"), Chord::parse("Ctrl+Shift+F3"));
    }

    #[test]
    fn chord_rejects_garbage() {
        ["", "Ctrl+", "Ctrl", "Ctrl+Shift", "Hyper+A", "Ctrl+Ctrl+A", "Ctrl+NoSuchKey", "+", "Ctrl++A", "A+B"]
            .iter()
            .for_each(|s| assert_eq!(Chord::parse(s), None, "{s:?} must be rejected"));
    }

    #[test]
    fn command_ids_unique_and_default_chords_valid() {
        let ids: HashSet<&str> = commands().iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), commands().len());
        let chords: Vec<Chord> = commands()
            .iter()
            .filter_map(|c| c.default_chord)
            .map(|s| Chord::parse(s).unwrap_or_else(|| panic!("default chord {s} must parse")))
            .collect();
        let distinct: HashSet<String> = chords.iter().map(|c| c.to_string()).collect();
        assert_eq!(distinct.len(), chords.len());
        assert!(ids.contains("encoding.utf8") && ids.contains("encoding.utf8_bom"));
        assert!(LineTool::all().iter().all(|t| line_tool_for_command(&line_tool_command(*t)) == Some(*t)));
        assert!(TextEncoding::choices().into_iter().all(|e| encoding_for_command(&encoding_command(e)) == Some(e)));
    }

    #[test]
    fn keymap_override_and_unbind() {
        let mut o = BTreeMap::new();
        o.insert("file.save".to_string(), "Ctrl+Alt+K".to_string());
        o.insert("file.new".to_string(), String::new());
        o.insert("file.open".to_string(), "garbage".to_string());
        let km = Keymap::new(&o);
        assert_eq!(km.chord_for("file.save"), Chord::parse("Ctrl+Alt+K"));
        assert_eq!(km.chord_for("file.new"), None);
        assert_eq!(km.chord_for("file.open"), Chord::parse("Ctrl+O"));
        assert_eq!(km.errors().len(), 1);
        assert_eq!(km.command_for(Chord::parse("Ctrl+S").expect("valid")), None);
        assert_eq!(km.chord_for("file.open_hex"), None);
        o.insert("file.open_hex".to_string(), "Ctrl+S".to_string());
        assert_eq!(Keymap::new(&o).command_for(Chord::parse("Ctrl+S").expect("valid")), Some("file.open_hex"));
    }

    #[test]
    fn take_triggered_consumes_only_bound_keys() {
        let ctx = egui::Context::default();
        let km = Keymap::new(&BTreeMap::new());
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let events = vec![key(Key::S, Modifiers::COMMAND | Modifiers::SHIFT), key(Key::J, Modifiers::COMMAND)];
        let input = egui::RawInput { events, ..Default::default() };
        let mut triggered = Vec::new();
        let mut left = 0;
        let _output = ctx.run(input, |ctx| {
            triggered = km.take_triggered(ctx);
            left = ctx.input(|i| i.events.len());
        });
        assert_eq!(triggered, vec!["file.save_all"]);
        assert_eq!(left, 1);
    }
}
