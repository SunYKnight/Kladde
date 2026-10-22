//! Macro recording and playback (E11): editor input events and commands, replayed one frame per step.

use std::collections::VecDeque;

use egui::Event;

use crate::state::App;

#[derive(Clone, Debug)]
pub enum MacroStep {
    /// Input events of one frame, injected at the start of a frame.
    Events(Vec<Event>),
    Command(String),
}

#[derive(Default)]
pub struct MacroState {
    pub recording: bool,
    pub recorded: Vec<MacroStep>,
    playing: VecDeque<MacroStep>,
}

impl MacroState {
    pub fn record_command(&mut self, id: &str) {
        self.recorded.push(MacroStep::Command(id.to_string()));
    }
}

fn recordable(e: &Event) -> bool {
    matches!(e, Event::Text(_) | Event::Key { .. } | Event::Paste(_) | Event::Cut | Event::Copy)
}

impl App {
    pub fn toggle_macro_recording(&mut self) {
        let chord = self.keymap.chord_for("macro.record").map(|c| c.to_string()).unwrap_or_default();
        if self.feat.macros.recording {
            self.feat.macros.recording = false;
            let steps = self.feat.macros.recorded.len();
            self.toast_info(format!("Macro recorded ({steps} steps)"));
        } else {
            self.feat.macros.recorded.clear();
            self.feat.macros.recording = true;
            self.toast_info(format!("Recording macro: type in the editor, stop with {chord} or Macro → Stop"));
        }
    }

    pub fn play_macro(&mut self) {
        self.feat.macros.playing = self.feat.macros.recorded.iter().cloned().collect();
    }

    /// Replays one recorded step; events reach the focused editor later in this frame.
    pub fn macro_playback_step(&mut self, ctx: &egui::Context) {
        match self.feat.macros.playing.pop_front() {
            Some(MacroStep::Events(events)) => {
                ctx.input_mut(|i| i.events.extend(events));
                ctx.request_repaint();
            }
            Some(MacroStep::Command(id)) => {
                self.execute(ctx, &id);
                ctx.request_repaint();
            }
            None => {}
        }
    }

    /// Records this frame's editor input; called after the editors ran.
    pub fn macro_record_events(&mut self, ctx: &egui::Context) {
        self.feat.editor_had_focus = self.shell.editor_focused;
        let macros = &self.feat.macros;
        if macros.recording && macros.playing.is_empty() && self.shell.editor_focused {
            let events: Vec<Event> = ctx.input(|i| i.events.iter().filter(|e| recordable(e)).cloned().collect());
            if !events.is_empty() {
                self.feat.macros.recorded.push(MacroStep::Events(events));
            }
        }
    }
}
