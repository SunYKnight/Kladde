use std::collections::VecDeque;

use crate::text::Piece;

/// Revisions kept for `changed_since`; older queries fall back to "everything changed".
const RING: usize = 256;

/// One piece-list splice: `inserted` replaced `removed` at `index`. Holds pieces, never bytes.
pub(crate) struct Change {
    pub(crate) index: usize,
    pub(crate) removed: Vec<Piece>,
    pub(crate) inserted: Vec<Piece>,
    pub(crate) pos: u64,
    pub(crate) removed_len: u64,
    pub(crate) inserted_len: u64,
}

impl Change {
    /// Folds `next` in when it only touches pieces this change inserted; else hands it back.
    fn absorb(&mut self, next: Change) -> Option<Change> {
        let within = next.index >= self.index && next.index + next.removed.len() <= self.index + self.inserted.len();
        if within {
            let at = next.index - self.index;
            self.inserted.splice(at..at + next.removed.len(), next.inserted);
            self.pos = self.pos.min(next.pos);
            self.inserted_len = (self.inserted_len + next.inserted_len).saturating_sub(next.removed_len);
            None
        } else {
            Some(next)
        }
    }
}

/// Changes undone and redone together.
pub(crate) struct Step {
    pub(crate) changes: Vec<Change>,
}

impl Step {
    fn push(&mut self, change: Change) {
        let rest = match self.changes.last_mut() {
            Some(last) => last.absorb(change),
            None => Some(change),
        };
        self.changes.extend(rest);
    }

    pub(crate) fn low(&self) -> u64 {
        self.changes.iter().map(|c| c.pos).min().unwrap_or(0)
    }
}

/// Kind of a single typed character, for merging typing into one undo step.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Typed {
    Word,
    Space,
}

pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    depth: u32,
    /// The open group already pushed its step.
    group_open: bool,
    /// Offset where the next typed character continues the current typing step.
    typing: Option<u64>,
    /// Undo depth matching the file on disk; None when unreachable.
    saved: Option<usize>,
    revision: u64,
    ring: VecDeque<(u64, u64)>,
}

impl History {
    pub(crate) fn new() -> History {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            depth: 0,
            group_open: false,
            typing: None,
            saved: Some(0),
            revision: 0,
            ring: VecDeque::new(),
        }
    }

    pub(crate) fn record(&mut self, change: Change, typed: Option<Typed>) {
        let pos = change.pos;
        let end = change.pos + change.inserted_len;
        if !self.redo.is_empty() {
            self.redo.clear();
            if self.saved.is_some_and(|d| d > self.undo.len()) {
                self.saved = None;
            }
        }
        let typing = self.depth == 0 && typed.is_some() && self.typing == Some(pos) && change.removed_len == 0;
        let extend = !self.undo.is_empty() && (typing || (self.depth > 0 && self.group_open));
        if extend {
            if self.saved == Some(self.undo.len()) {
                self.saved = None;
            }
            self.undo.last_mut().expect("extend implies a top step").push(change);
        } else {
            self.undo.push(Step { changes: vec![change] });
            self.group_open = self.depth > 0;
        }
        self.typing = match typed {
            Some(Typed::Word) if self.depth == 0 => Some(end),
            _ => None,
        };
        self.bump(pos);
    }

    pub(crate) fn begin_group(&mut self) {
        self.depth += 1;
        if self.depth == 1 {
            self.group_open = false;
        }
        self.typing = None;
    }

    pub(crate) fn end_group(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.group_open = false;
        }
        self.typing = None;
    }

    fn seal(&mut self) {
        self.depth = 0;
        self.group_open = false;
        self.typing = None;
    }

    pub(crate) fn take_undo(&mut self) -> Option<Step> {
        self.seal();
        self.undo.pop()
    }

    pub(crate) fn push_redo(&mut self, step: Step) {
        self.redo.push(step);
    }

    pub(crate) fn take_redo(&mut self) -> Option<Step> {
        self.seal();
        self.redo.pop()
    }

    pub(crate) fn push_undo(&mut self, step: Step) {
        self.undo.push(step);
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn mark_saved(&mut self) {
        self.seal();
        self.saved = Some(self.undo.len());
    }

    pub(crate) fn mark_unsaved(&mut self) {
        self.saved = None;
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    /// Forgets all steps; the current content becomes the saved state.
    pub(crate) fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.seal();
        self.saved = Some(0);
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn bump(&mut self, off: u64) {
        self.revision += 1;
        self.ring.push_back((self.revision, off));
        if self.ring.len() > RING {
            self.ring.pop_front();
        }
    }

    pub(crate) fn changed_since(&self, revision: u64) -> Option<u64> {
        let complete = self.ring.front().is_some_and(|f| f.0 <= revision + 1);
        if revision >= self.revision {
            None
        } else if !complete {
            Some(0)
        } else {
            self.ring.iter().filter(|e| e.0 > revision).map(|e| e.1).min()
        }
    }
}
