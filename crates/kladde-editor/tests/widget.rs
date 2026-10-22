//! Drives `show` headlessly with synthetic egui input.

use egui::{CentralPanel, Context, Event, Id, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};
use kladde_core::Document;
use kladde_editor::{show, EditorConfig, EditorEvent, EditorOutput, EditorState, EditorTheme};

struct Harness {
    ctx: Context,
    doc: Document,
    state: EditorState,
    cfg: EditorConfig,
    time: f64,
}

impl Harness {
    fn new(text: &str, path: &str) -> Harness {
        Harness {
            ctx: Context::default(),
            doc: Document::from_bytes(text.as_bytes().to_vec()),
            state: EditorState::new(Some(std::path::Path::new(path))),
            cfg: EditorConfig::default(),
            time: 0.0,
        }
    }

    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) -> EditorOutput {
        self.time += 1.0;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            modifiers,
            time: Some(self.time),
            ..Default::default()
        };
        let mut out = None;
        let (doc, state, cfg) = (&mut self.doc, &mut self.state, &self.cfg);
        drop(self.ctx.run(input, |ctx| {
            CentralPanel::default().show(ctx, |ui| {
                out = Some(show(ui, Id::new("ed"), doc, state, cfg, &EditorTheme::dark()));
            });
        }));
        out.expect("editor shown")
    }

    fn click(&mut self, pos: Pos2, modifiers: Modifiers) -> EditorOutput {
        self.frame(vec![Event::PointerMoved(pos)], modifiers);
        let press = Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers };
        self.frame(vec![press], modifiers);
        let release = Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers };
        self.frame(vec![release], modifiers)
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> EditorOutput {
        let ev = |pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
        self.frame(vec![ev(true), ev(false)], modifiers)
    }

    fn text(&self) -> String {
        self.doc.read_string_lossy(0..self.doc.len())
    }
}

#[test]
fn typing_editing_and_undo_through_the_widget() {
    let mut h = Harness::new("    foo\nbar\n", "a.txt");
    h.frame(Vec::new(), Modifiers::NONE);
    let out = h.click(Pos2::new(400.0, 8.0), Modifiers::NONE);
    assert!(out.focused);
    assert_eq!(h.state.caret(), 7, "click right of the line puts the caret at its end");
    h.frame(vec![Event::Text("x".into())], Modifiers::NONE);
    let out = h.key(Key::Enter, Modifiers::NONE);
    assert!(out.changed);
    h.frame(vec![Event::Text("y".into())], Modifiers::NONE);
    assert_eq!(h.text(), "    foox\n    y\nbar\n");
    h.key(Key::ArrowDown, Modifiers::NONE);
    h.key(Key::End, Modifiers::NONE);
    h.key(Key::Backspace, Modifiers::NONE);
    assert_eq!(h.text(), "    foox\n    y\nba\n");
    h.key(Key::Z, Modifiers::COMMAND);
    assert_eq!(h.text(), "    foox\n    y\nbar\n");
    h.key(Key::Home, Modifiers::NONE);
    h.key(Key::Home, Modifiers::SHIFT);
    assert_eq!(h.state.primary_range(), 15..15);
}

#[test]
fn multi_cursor_typing_via_ctrl_alt_down() {
    let mut h = Harness::new("ab\ncd\nef\n", "a.txt");
    h.frame(Vec::new(), Modifiers::NONE);
    h.click(Pos2::new(400.0, 8.0), Modifiers::NONE);
    h.key(Key::Home, Modifiers::NONE);
    let ctrl_alt = Modifiers { alt: true, ctrl: true, command: true, ..Modifiers::NONE };
    h.key(Key::ArrowDown, ctrl_alt);
    h.key(Key::ArrowDown, ctrl_alt);
    assert_eq!(h.state.selections().len(), 3);
    h.frame(vec![Event::Text("-".into())], Modifiers::NONE);
    assert_eq!(h.text(), "-ab\n-cd\n-ef\n");
    h.key(Key::Z, Modifiers::COMMAND);
    assert_eq!(h.text(), "ab\ncd\nef\n");
    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(h.state.selections().len(), 1);
}

#[test]
fn read_only_edits_report_one_error_per_frame() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("ro.txt");
    std::fs::write(&path, "abc").expect("write temp file");
    let opts = kladde_core::OpenOptions { read_only: true, ..Default::default() };
    let doc = Document::open(&path, &opts, &kladde_core::Progress::default()).expect("open");
    let mut h = Harness::new("", "ro.txt");
    h.doc = doc;
    h.frame(Vec::new(), Modifiers::NONE);
    h.click(Pos2::new(400.0, 8.0), Modifiers::NONE);
    let out = h.frame(vec![Event::Text("x".into()), Event::Text("y".into())], Modifiers::NONE);
    let errors = out.events.iter().filter(|e| matches!(e, EditorEvent::Error(_))).count();
    assert_eq!(errors, 1);
    assert_eq!(h.text(), "abc");
}
