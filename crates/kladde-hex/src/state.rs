//! Caret, selection, scroll position and the editing operations of the hex view.

use std::io;
use std::ops::Range;

use kladde_core::{CoreError, Document};

use crate::format::Layout;

/// View state of one hex view; keep one per open document view.
#[derive(Debug, Default)]
pub struct HexState {
    pub(crate) cursor: u64,
    pub(crate) anchor: u64,
    /// Nibble (hex) or bit (binary) of the cursor byte that the next digit writes; 0 = most significant.
    pub(crate) digit: u8,
    pub(crate) text_active: bool,
    pub(crate) insert: bool,
    pub(crate) top: u64,
    /// Offset to centre in the view on the next frame (set by `goto`/`select`).
    pub(crate) reveal: Option<u64>,
    /// Byte where the current mouse drag started.
    pub(crate) drag_origin: Option<u64>,
    /// Pointer distance from the thumb top while dragging the scrollbar.
    pub(crate) thumb_grab: Option<f32>,
    /// Wheel movement not yet converted into whole rows.
    pub(crate) wheel: f32,
    /// Rows that fitted into the view last frame.
    pub(crate) page_rows: u64,
}

impl HexState {
    pub fn new() -> HexState {
        HexState::default()
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Selected bytes; empty when nothing is selected.
    pub fn selection(&self) -> Range<u64> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }

    /// Moves the caret to `off` (clamped to the document length) and scrolls it into view.
    pub fn goto(&mut self, doc: &Document, off: u64) {
        self.cursor = off.min(doc.len());
        self.anchor = self.cursor;
        self.digit = 0;
        self.reveal = Some(self.cursor);
    }

    /// Selects `range` (clamped), caret at its end, and scrolls its start into view.
    pub fn select(&mut self, doc: &Document, range: Range<u64>) {
        let len = doc.len();
        self.anchor = range.start.min(len);
        self.cursor = range.end.clamp(self.anchor, len);
        self.digit = 0;
        self.reveal = Some(self.anchor);
    }

    pub fn insert_mode(&self) -> bool {
        self.insert
    }

    pub fn set_insert_mode(&mut self, on: bool) {
        self.insert = on;
    }

    /// Offset of the first visible row.
    pub fn top_offset(&self) -> u64 {
        self.top
    }

    /// Scrolls so the row containing `off` is the first visible row.
    pub fn set_top_offset(&mut self, off: u64) {
        self.top = off;
    }

    /// Keeps caret and selection inside a document that may have shrunk.
    pub(crate) fn clamp(&mut self, len: u64, layout: &Layout) {
        self.cursor = self.cursor.min(len);
        self.anchor = self.anchor.min(len);
        if self.cursor == len || self.digit >= layout.digits {
            self.digit = 0;
        }
    }

    /// Caret move; `extend` keeps the anchor to grow the selection.
    pub(crate) fn move_to(&mut self, off: u64, len: u64, extend: bool) {
        self.cursor = off.min(len);
        if !extend {
            self.anchor = self.cursor;
        }
        self.digit = 0;
    }

    /// Selects whole bytes between the drag origin and byte `b`, both included.
    pub(crate) fn drag_to(&mut self, origin: u64, b: u64, len: u64) {
        let (anchor, cursor) = match b.cmp(&origin) {
            std::cmp::Ordering::Equal => (origin, origin),
            std::cmp::Ordering::Greater => (origin, b + 1),
            std::cmp::Ordering::Less => (origin + 1, b),
        };
        self.anchor = anchor.min(len);
        self.cursor = cursor.min(len);
        self.digit = 0;
    }

    /// Writes one typed digit (hex nibble or binary bit) at the caret and advances it.
    pub(crate) fn type_digit(&mut self, doc: &mut Document, value: u8, layout: &Layout) -> Result<(), CoreError> {
        grouped(doc, |doc| {
            self.prepare_edit(doc)?;
            let at = self.cursor;
            let bits = layout.digit_bits();
            let shift = 8 - bits * (self.digit + 1);
            let mask = ((1u16 << bits) - 1) as u8;
            let fresh = at >= doc.len() || (self.insert && self.digit == 0);
            let old = if fresh { 0 } else { doc.byte_at(at).ok_or_else(|| unreadable(at))? };
            let new = (old & !(mask << shift)) | ((value & mask) << shift);
            if fresh {
                doc.insert(at, &[new])?;
            } else {
                doc.replace(at..at + 1, &[new])?;
            }
            self.digit += 1;
            if self.digit == layout.digits {
                self.digit = 0;
                self.cursor += 1;
            }
            self.anchor = self.cursor;
            Ok(())
        })
    }

    /// Typed or pasted bytes: inserted in insert mode, else overwriting from the caret (growing at the end).
    pub(crate) fn write_bytes(&mut self, doc: &mut Document, bytes: &[u8]) -> Result<(), CoreError> {
        grouped(doc, |doc| {
            self.prepare_edit(doc)?;
            let at = self.cursor;
            let n = bytes.len() as u64;
            if self.insert {
                doc.insert(at, bytes)?;
            } else {
                doc.replace(at..(at + n).min(doc.len()), bytes)?;
            }
            self.cursor = at + n;
            self.anchor = self.cursor;
            self.digit = 0;
            Ok(())
        })
    }

    /// Backspace: removes the selection or the byte before the caret in insert mode; only moves left otherwise.
    pub(crate) fn backspace(&mut self, doc: &mut Document) -> Result<(), CoreError> {
        let sel = self.selection();
        if !self.insert {
            self.move_to(self.cursor.saturating_sub(u64::from(self.digit == 0)), doc.len(), false);
            Ok(())
        } else if !sel.is_empty() {
            self.remove(doc, sel)
        } else if self.digit > 0 {
            self.remove(doc, self.cursor..self.cursor + 1)
        } else if self.cursor > 0 {
            self.remove(doc, self.cursor - 1..self.cursor)
        } else {
            Ok(())
        }
    }

    /// Delete: removes the selection or the byte at the caret; overwrite mode keeps the size and does nothing.
    pub(crate) fn delete_forward(&mut self, doc: &mut Document) -> Result<(), CoreError> {
        let sel = self.selection();
        if !self.insert {
            Ok(())
        } else if sel.is_empty() {
            self.remove(doc, self.cursor..self.cursor + 1)
        } else {
            self.remove(doc, sel)
        }
    }

    /// Removes `range` and puts the caret at its start.
    pub(crate) fn remove(&mut self, doc: &mut Document, range: Range<u64>) -> Result<(), CoreError> {
        let start = range.start;
        doc.delete(range)?;
        self.move_to(start, doc.len(), false);
        Ok(())
    }

    /// Before typing: insert mode replaces the selection, overwrite mode writes from its start.
    fn prepare_edit(&mut self, doc: &mut Document) -> Result<(), CoreError> {
        let sel = self.selection();
        if sel.is_empty() {
            Ok(())
        } else if self.insert {
            self.remove(doc, sel)
        } else {
            self.move_to(sel.start, doc.len(), false);
            Ok(())
        }
    }
}

/// Runs `f` as one undo step.
fn grouped<T>(doc: &mut Document, f: impl FnOnce(&mut Document) -> Result<T, CoreError>) -> Result<T, CoreError> {
    doc.begin_undo_group();
    let result = f(doc);
    doc.end_undo_group();
    result
}

fn unreadable(off: u64) -> CoreError {
    CoreError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("byte at offset {off:#X} is not readable")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HexConfig, HexMode, TextColumn};

    fn layout(mode: HexMode) -> Layout {
        Layout::new(&HexConfig { bytes_per_row: 16, group: 1, mode, text: TextColumn::Ascii, font_size: 12.0 })
    }

    fn bytes(doc: &Document) -> Vec<u8> {
        let mut out = Vec::new();
        doc.read(0..doc.len(), &mut out);
        out
    }

    fn type_hex(state: &mut HexState, doc: &mut Document, digits: &str) {
        let hex = layout(HexMode::Hex);
        digits.chars().for_each(|c| {
            let value = c.to_digit(16).expect("test digit") as u8;
            state.type_digit(doc, value, &hex).expect("edit");
        });
    }

    #[test]
    fn nibble_overwrite_sequence() {
        let mut doc = Document::from_bytes(vec![0x00, 0x11, 0x22]);
        let mut state = HexState::new();
        type_hex(&mut state, &mut doc, "ABC");
        assert_eq!(bytes(&doc), [0xAB, 0xC1, 0x22]);
        assert_eq!((state.cursor(), state.digit), (1, 1));
        // Past the end, overwrite mode appends.
        state.goto(&doc, 3);
        type_hex(&mut state, &mut doc, "F");
        assert_eq!(bytes(&doc), [0xAB, 0xC1, 0x22, 0xF0]);
        type_hex(&mut state, &mut doc, "E");
        assert_eq!(bytes(&doc), [0xAB, 0xC1, 0x22, 0xFE]);
        assert_eq!(state.cursor(), 4);
    }

    #[test]
    fn overwrite_with_selection_starts_at_selection() {
        let mut doc = Document::from_bytes(vec![1, 2, 3, 4]);
        let mut state = HexState::new();
        state.select(&doc, 1..3);
        type_hex(&mut state, &mut doc, "99");
        assert_eq!(bytes(&doc), [1, 0x99, 3, 4]);
        assert_eq!(state.selection(), 2..2);
    }

    #[test]
    fn bit_edits_in_binary_mode() {
        let mut doc = Document::from_bytes(vec![0x00]);
        let mut state = HexState::new();
        let bin = layout(HexMode::Binary);
        [1, 0, 1, 1].iter().for_each(|&bit| state.type_digit(&mut doc, bit, &bin).expect("edit"));
        assert_eq!(bytes(&doc), [0b1011_0000]);
        assert_eq!(state.digit, 4);
        [0, 0, 0, 1].iter().for_each(|&bit| state.type_digit(&mut doc, bit, &bin).expect("edit"));
        assert_eq!(bytes(&doc), [0b1011_0001]);
        assert_eq!((state.cursor(), state.digit), (1, 0));
    }

    #[test]
    fn insert_mode_inserts_and_deletes() {
        let mut doc = Document::from_bytes(vec![0xAA, 0xBB]);
        let mut state = HexState::new();
        state.set_insert_mode(true);
        state.goto(&doc, 1);
        type_hex(&mut state, &mut doc, "123");
        assert_eq!(bytes(&doc), [0xAA, 0x12, 0x30, 0xBB]);
        // Backspace on a half-typed byte removes that byte.
        state.backspace(&mut doc).expect("backspace");
        assert_eq!(bytes(&doc), [0xAA, 0x12, 0xBB]);
        assert_eq!(state.cursor(), 2);
        state.backspace(&mut doc).expect("backspace");
        assert_eq!(bytes(&doc), [0xAA, 0xBB]);
        assert_eq!(state.cursor(), 1);
        state.delete_forward(&mut doc).expect("delete");
        assert_eq!(bytes(&doc), [0xAA]);
        state.select(&doc, 0..1);
        state.write_bytes(&mut doc, b"xy").expect("insert");
        assert_eq!(bytes(&doc), b"xy");
        assert_eq!(state.cursor(), 2);
    }

    #[test]
    fn overwrite_mode_keeps_size_on_delete() {
        let mut doc = Document::from_bytes(vec![1, 2, 3]);
        let mut state = HexState::new();
        state.goto(&doc, 2);
        state.delete_forward(&mut doc).expect("delete");
        state.backspace(&mut doc).expect("backspace");
        assert_eq!(bytes(&doc), [1, 2, 3]);
        assert_eq!(state.cursor(), 1);
        state.write_bytes(&mut doc, &[9, 9, 9]).expect("overwrite");
        assert_eq!(bytes(&doc), [1, 9, 9, 9]);
    }

    #[test]
    fn one_keystroke_is_one_undo_step() {
        let mut doc = Document::from_bytes(vec![1, 2, 3]);
        let mut state = HexState::new();
        state.set_insert_mode(true);
        state.select(&doc, 0..2);
        type_hex(&mut state, &mut doc, "F");
        assert_eq!(bytes(&doc), [0xF0, 3]);
        doc.undo();
        assert_eq!(bytes(&doc), [1, 2, 3]);
    }

    #[test]
    fn read_only_edit_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ro.bin");
        std::fs::write(&path, [1u8, 2]).expect("write");
        let opts = kladde_core::OpenOptions { mode: kladde_core::OpenMode::Binary, read_only: true, no_lock: false };
        let mut doc = Document::open(&path, &opts, &kladde_core::Progress::new()).expect("open");
        let mut state = HexState::new();
        let result = state.type_digit(&mut doc, 0xA, &layout(HexMode::Hex));
        assert!(matches!(result, Err(CoreError::ReadOnly)));
        assert_eq!(bytes(&doc), [1, 2]);
    }
}
