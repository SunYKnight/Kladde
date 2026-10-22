//! Edit commands, text transforms, hashing, file history, encoding/EOL, go to, diff, CSV table and settings.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use kladde_core::{CoreError, Document, LineEnding, OpenMode, TextEncoding, MEMORY_LIMIT};
use kladde_editor::{EditorState, Selection};
use kladde_hex::HexState;
use kladde_panels::{
    parse_hex, show_csv, show_diff, show_history, show_settings, DiffTheme, HashJob, HistoryEvent, Keymap,
};
use kladde_tools::{diff_text, parse_csv, CsvTable, DiffLine, DiffTag, HashAlgo};
use kladde_workspace::ViewKind;

use crate::commands::{poll_task, spawn_task, BottomTab, Cmd, HISTORY_MISSING};
use crate::docs::auto_open;
use crate::state::{App, DocId};

/// Texts above this size are not diffed or shown next to history versions.
const TEXT_VIEW_MAX: u64 = 8 << 20;
const CSV_MAX_BYTES: u64 = 64 << 20;
const CSV_MAX_ROWS: usize = 100_000;
/// A requested clipboard paste that has not arrived by then is dropped.
const PASTE_WAIT: Duration = Duration::from_secs(1);
/// A history restore waits this long for its file to open.
const RESTORE_WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    JsonPretty,
    JsonMinify,
    XmlPretty,
    XmlMinify,
    Base64Encode,
    Base64Decode,
    UrlEncode,
    UrlDecode,
    HexEncode,
    HexDecode,
}

impl Transform {
    pub fn label(self) -> &'static str {
        match self {
            Transform::JsonPretty => "JSON pretty print",
            Transform::JsonMinify => "JSON minify",
            Transform::XmlPretty => "XML pretty print",
            Transform::XmlMinify => "XML minify",
            Transform::Base64Encode => "Base64 encode",
            Transform::Base64Decode => "Base64 decode",
            Transform::UrlEncode => "URL encode",
            Transform::UrlDecode => "URL decode",
            Transform::HexEncode => "Hex encode",
            Transform::HexDecode => "Hex decode",
        }
    }

    pub fn apply(self, text: &str) -> Result<String, String> {
        match self {
            Transform::JsonPretty => kladde_tools::json_pretty(text),
            Transform::JsonMinify => kladde_tools::json_minify(text),
            Transform::XmlPretty => kladde_tools::xml_pretty(text),
            Transform::XmlMinify => kladde_tools::xml_minify(text),
            Transform::Base64Encode => Ok(kladde_tools::base64_encode(text)),
            Transform::Base64Decode => kladde_tools::base64_decode(text),
            Transform::UrlEncode => Ok(kladde_tools::url_encode(text)),
            Transform::UrlDecode => kladde_tools::url_decode(text),
            Transform::HexEncode => Ok(kladde_tools::hex_encode(text)),
            Transform::HexDecode => kladde_tools::hex_decode(text),
        }
    }
}

/// Destructive action waiting for confirmation because the document has unsaved changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Confirm {
    Reload(DocId),
    Reopen(DocId, TextEncoding),
}

pub struct GotoState {
    offset: bool,
    input: String,
    focus: bool,
}

impl GotoState {
    pub fn new(offset: bool) -> GotoState {
        GotoState { offset, input: String::new(), focus: true }
    }
}

pub struct DiffWindow {
    title: String,
    lines: Vec<DiffLine>,
    rx: Option<Receiver<Vec<DiffLine>>>,
}

pub struct CsvCache {
    revision: u64,
    table: Option<CsvTable>,
    rx: Option<Receiver<Result<CsvTable, String>>>,
    error: Option<String>,
}

#[derive(Default)]
pub struct HashPanel {
    job: Option<(DocId, HashJob)>,
    title: String,
    results: Vec<(HashAlgo, String)>,
    error: Option<String>,
}

impl HashPanel {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn cancel_doc(&mut self, id: DocId) {
        if self.job.as_ref().is_some_and(|(d, _)| *d == id) {
            if let Some((_, job)) = self.job.take() {
                job.cancel();
                self.error = Some("Cancelled because the document was saved; run Checksums again".to_string());
            }
        }
    }
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

fn too_large(range: &Range<u64>) -> Option<String> {
    (range.end - range.start > MEMORY_LIMIT).then(|| "the selection exceeds 64 MiB".to_string())
}

/// Sorted, merged whole-line blocks touched by `sels` (each ends after its line terminator, or at the end).
fn line_blocks(doc: &Document, sels: &[Selection]) -> Vec<Range<u64>> {
    let mut blocks: Vec<Range<u64>> = sels
        .iter()
        .map(|s| {
            let r = s.range();
            let last = if r.end > r.start && doc.line_start_at(r.end) == r.end { r.end - 1 } else { r.end };
            doc.line_start_at(r.start)..doc.next_line_start(last)
        })
        .collect();
    blocks.sort_by_key(|b| b.start);
    blocks.into_iter().fold(Vec::new(), |mut merged: Vec<Range<u64>>, b| {
        match merged.last_mut() {
            Some(prev) if b.start < prev.end => prev.end = prev.end.max(b.end),
            _ => merged.push(b),
        }
        merged
    })
}

fn ends_with_newline(doc: &Document, block: &Range<u64>) -> bool {
    block.end > block.start && doc.byte_at(block.end - 1) == Some(b'\n')
}

/// Copies each touched line block below itself; carets stay on the original lines.
fn duplicate_lines(doc: &mut Document, ed: &mut EditorState) -> Result<(), CoreError> {
    let sels: Vec<Selection> = ed.selections().to_vec();
    let blocks = line_blocks(doc, &sels);
    let eol = doc.line_ending().as_bytes();
    let inserts: Vec<(u64, Vec<u8>, bool)> = blocks
        .iter()
        .map(|b| {
            let mut text = Vec::new();
            doc.read(b.clone(), &mut text);
            let newline = ends_with_newline(doc, b);
            let bytes = if newline { text } else { [eol, text.as_slice()].concat() };
            (b.end, bytes, newline)
        })
        .collect();
    doc.begin_undo_group();
    let result = inserts.iter().rev().try_for_each(|(at, bytes, _)| doc.insert(*at, bytes));
    doc.end_undo_group();
    result?;
    let shift = |off: u64| -> u64 {
        inserts.iter().filter(|(at, _, newline)| *newline && *at <= off).map(|(_, b, _)| b.len() as u64).sum()
    };
    let moved = sels
        .iter()
        .map(|s| Selection { anchor: s.anchor + shift(s.anchor), caret: s.caret + shift(s.caret) })
        .collect();
    ed.set_selections(doc, moved);
    Ok(())
}

/// Removes each touched line block including its terminator (the preceding one for a last line).
fn delete_lines(doc: &mut Document, ed: &mut EditorState) -> Result<(), CoreError> {
    let sels: Vec<Selection> = ed.selections().to_vec();
    let ranges: Vec<Range<u64>> = line_blocks(doc, &sels)
        .into_iter()
        .map(|b| {
            let crlf = b.start >= 2 && doc.byte_at(b.start - 2) == Some(b'\r');
            match (ends_with_newline(doc, &b), b.start) {
                (false, s) if s > 0 && crlf => s - 2..b.end,
                (false, s) if s > 0 => s - 1..b.end,
                _ => b,
            }
        })
        .collect();
    doc.begin_undo_group();
    let result = ranges.iter().rev().try_for_each(|r| doc.delete(r.clone()));
    doc.end_undo_group();
    result?;
    let carets: Vec<Selection> = ranges
        .iter()
        .scan(0u64, |removed, r| {
            let at = r.start - *removed;
            *removed += r.end - r.start;
            Some(Selection::caret(at))
        })
        .collect();
    ed.set_selections(doc, carets);
    Ok(())
}

/// Edit command on a text view; Ok(Some) is an info message.
fn text_edit(
    ctx: &egui::Context,
    cmd: Cmd,
    doc: &mut Document,
    ed: &mut EditorState,
) -> Result<Option<String>, String> {
    let core = |r: Result<(), CoreError>| r.map(|_| None).map_err(|e| e.to_string());
    match cmd {
        Cmd::Undo => Ok((!ed.undo(doc)).then(|| "Nothing to undo".to_string())),
        Cmd::Redo => Ok((!ed.redo(doc)).then(|| "Nothing to redo".to_string())),
        Cmd::EditCopy | Cmd::EditCut => {
            let range = ed.primary_range();
            match too_large(&range) {
                Some(e) => Err(e),
                None if range.is_empty() => Ok(Some("Nothing selected".to_string())),
                None => {
                    ctx.copy_text(ed.selected_text(doc));
                    if cmd == Cmd::EditCut {
                        core(ed.replace_selection(doc, ""))
                    } else {
                        Ok(None)
                    }
                }
            }
        }
        Cmd::SelectAll => {
            ed.select_all(doc);
            Ok(None)
        }
        Cmd::DuplicateLine => core(duplicate_lines(doc, ed)),
        Cmd::DeleteLine => core(delete_lines(doc, ed)),
        Cmd::ToggleComment => core(ed.toggle_comment(doc)),
        Cmd::BookmarkToggle => {
            ed.toggle_bookmark(doc);
            Ok(None)
        }
        Cmd::BookmarkNext | Cmd::BookmarkPrev if ed.bookmarks().next().is_none() => {
            Ok(Some("No bookmarks; toggle one with Toggle bookmark".to_string()))
        }
        Cmd::BookmarkNext => {
            ed.next_bookmark(doc);
            Ok(None)
        }
        Cmd::BookmarkPrev => {
            ed.prev_bookmark(doc);
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Edit command on a hex view; Ok(Some) is an info message.
fn hex_edit(ctx: &egui::Context, cmd: Cmd, doc: &mut Document, hex: &mut HexState) -> Result<Option<String>, String> {
    match cmd {
        Cmd::Undo | Cmd::Redo => {
            let hint = if cmd == Cmd::Undo { doc.undo() } else { doc.redo() };
            match hint {
                Some(off) => {
                    hex.goto(doc, off);
                    Ok(None)
                }
                None => Ok(Some(if cmd == Cmd::Undo { "Nothing to undo" } else { "Nothing to redo" }.to_string())),
            }
        }
        Cmd::EditCopy | Cmd::EditCut => {
            let range = hex.selection();
            match too_large(&range) {
                Some(e) => Err(e),
                None if range.is_empty() => Ok(Some("Nothing selected".to_string())),
                None => {
                    let mut bytes = Vec::new();
                    doc.read(range.clone(), &mut bytes);
                    ctx.copy_text(hex_string(&bytes));
                    if cmd == Cmd::EditCut {
                        doc.delete(range.clone()).map_err(|e| e.to_string())?;
                        hex.goto(doc, range.start);
                    }
                    Ok(None)
                }
            }
        }
        Cmd::SelectAll => {
            hex.select(doc, 0..doc.len());
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Pastes `text` into a text view, converting line breaks to the document's line ending.
fn paste_text(doc: &mut Document, ed: &mut EditorState, text: &str) -> Result<(), CoreError> {
    let eol = String::from_utf8_lossy(doc.line_ending().as_bytes()).into_owned();
    let text = text.replace("\r\n", "\n").replace('\n', &eol);
    let start = ed.primary_range().start;
    ed.replace_selection(doc, &text)?;
    ed.set_caret(doc, start + text.len() as u64);
    Ok(())
}

/// Pastes into a hex view: hex digits become bytes, other text its UTF-8 bytes; overwrites unless in insert mode.
fn paste_bytes(doc: &mut Document, hex: &mut HexState, text: &str) -> Result<(), CoreError> {
    // Clipboard text that is not hex is pasted as its bytes, like typing in the text column.
    let bytes = parse_hex(text).unwrap_or_else(|_| text.as_bytes().to_vec());
    let sel = hex.selection();
    let start = if sel.is_empty() { hex.cursor() } else { sel.start };
    let replaced = match (sel.is_empty(), hex.insert_mode()) {
        (false, _) => sel,
        (true, true) => start..start,
        (true, false) => start..(start + bytes.len() as u64).min(doc.len()),
    };
    doc.replace(replaced, &bytes)?;
    hex.goto(doc, start + bytes.len() as u64);
    Ok(())
}

impl App {
    pub fn edit_command(&mut self, ctx: &egui::Context, cmd: Cmd) {
        if cmd == Cmd::EditPaste {
            self.feat.pending_paste = Some(Instant::now());
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        } else {
            let outcome = self.active_parts().map(|(entry, tab)| match tab.view {
                ViewKind::Text => text_edit(ctx, cmd, &mut entry.doc, &mut tab.editor),
                ViewKind::Hex => hex_edit(ctx, cmd, &mut entry.doc, &mut tab.hex),
            });
            match outcome {
                Some(Ok(Some(info))) => self.toast_info(info),
                Some(Err(e)) => self.toast_error(format!("{}: {e}", crate::commands::command_label(cmd_id(cmd)))),
                _ => {}
            }
        }
    }

    /// Applies the clipboard text requested by edit.paste before the editor sees the paste event.
    pub fn apply_pending_paste(&mut self, ctx: &egui::Context) {
        if let Some(at) = self.feat.pending_paste {
            let text = ctx.input_mut(|i| {
                let pos = i.events.iter().position(|e| matches!(e, egui::Event::Paste(_)));
                match pos.map(|p| i.events.remove(p)) {
                    Some(egui::Event::Paste(t)) => Some(t),
                    _ => None,
                }
            });
            match text {
                Some(text) => {
                    self.feat.pending_paste = None;
                    let result = self.active_parts().map(|(entry, tab)| match tab.view {
                        ViewKind::Text => paste_text(&mut entry.doc, &mut tab.editor, &text),
                        ViewKind::Hex => paste_bytes(&mut entry.doc, &mut tab.hex, &text),
                    });
                    if let Some(Err(e)) = result {
                        self.toast_error(format!("Paste: {e}"));
                    }
                }
                None if at.elapsed() > PASTE_WAIT => {
                    self.feat.pending_paste = None;
                    self.toast_info("Paste: the clipboard holds no text");
                }
                None => ctx.request_repaint(),
            }
        }
    }

    pub fn transform_text(&mut self, label: &str, f: impl FnOnce(&str) -> Result<String, String>) {
        let outcome = self.active_parts().map(|(entry, tab)| {
            let whole = tab.editor.primary_range().is_empty();
            let range = if whole { 0..entry.doc.len() } else { tab.editor.primary_range() };
            match too_large(&range) {
                Some(_) if whole => {
                    Err("select text first; whole-document transforms are limited to 64 MiB".to_string())
                }
                Some(e) => Err(e),
                None => {
                    if whole {
                        tab.editor.select_all(&entry.doc);
                    }
                    let text = tab.editor.selected_text(&entry.doc);
                    f(&text)
                        .and_then(|out| tab.editor.replace_selection(&mut entry.doc, &out).map_err(|e| e.to_string()))
                }
            }
        });
        if let Some(Err(e)) = outcome {
            self.toast_error(format!("{label}: {e}"));
        }
    }

    pub fn convert_encoding(&mut self, enc: TextEncoding) {
        if let Some((entry, _)) = self.active_parts() {
            entry.doc.set_encoding(enc);
            self.toast_info(format!("Encoding set to {}; applied when saving", enc.name()));
        }
    }

    /// "Reopen with" an encoding; asks first when that discards unsaved changes.
    pub fn reopen_with(&mut self, id: DocId, enc: TextEncoding) {
        if self.docs.get(&id).is_some_and(|e| e.doc.is_dirty()) {
            self.feat.confirm = Some(Confirm::Reopen(id, enc));
        } else {
            self.do_reopen(id, enc);
        }
    }

    fn do_reopen(&mut self, id: DocId, enc: TextEncoding) {
        let opts = self
            .docs
            .get(&id)
            .map(|e| kladde_core::OpenOptions { mode: OpenMode::Text { encoding: Some(enc) }, ..e.open_opts.clone() });
        if let Some(opts) = opts {
            self.reopen_doc(id, opts);
        }
    }

    pub fn convert_eol(&mut self, le: LineEnding) {
        let result = self.active_parts().map(|(entry, _)| entry.doc.convert_line_endings(le));
        match result {
            Some(Ok(())) => self.toast_info(format!("Line endings converted to {}", le.name())),
            Some(Err(e)) => self.toast_error(format!("Convert line endings: {e}")),
            None => {}
        }
    }

    pub fn start_hash(&mut self) {
        let job = self.focused_view().map(|(e, t)| {
            let sel = match t.view {
                ViewKind::Text => t.editor.primary_range(),
                ViewKind::Hex => t.hex.selection(),
            };
            let name = e.doc.path().and_then(Path::file_name).map_or(e.title.clone(), |n| n.to_string_lossy().into());
            let (range, what) = if sel.is_empty() { (0..e.doc.len(), "whole file") } else { (sel, "selection") };
            let title = format!("{name}: {what}, {} bytes", range.end - range.start);
            (e.id, title, HashJob::start(e.doc.snapshot(), range, HashAlgo::all().to_vec()))
        });
        if let Some((doc, title, job)) = job {
            if let Some((_, old)) = self.feat.hash.job.take() {
                old.cancel();
            }
            self.feat.hash = HashPanel { job: Some((doc, job)), title, results: Vec::new(), error: None };
            self.feat.show_bottom = Some(BottomTab::Hash);
        }
    }

    pub fn poll_hash(&mut self) {
        let done = self.feat.hash.job.as_mut().and_then(|(_, job)| job.poll());
        if let Some(result) = done {
            self.feat.hash.job = None;
            match result {
                Ok(results) => self.feat.hash.results = results,
                Err(e) => self.feat.hash.error = Some(e),
            }
        }
    }

    pub fn show_hash_tab(&mut self, ui: &mut egui::Ui) {
        let panel = &self.feat.hash;
        if panel.title.is_empty() {
            ui.weak("Run Tools → Checksums to hash the selection or the whole file.");
        } else {
            ui.strong(&panel.title);
        }
        if let Some((_, job)) = &panel.job {
            let fraction = job.progress();
            let cancel = ui
                .horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(fraction).desired_width(240.0).show_percentage());
                    ui.button("Cancel").clicked()
                })
                .inner;
            if cancel {
                job.cancel();
            }
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if let Some(e) = &panel.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        let copied = egui::Grid::new("kladde_hash_grid")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                panel.results.iter().fold(None, |copied, (algo, digest)| {
                    ui.label(algo.label());
                    ui.monospace(digest);
                    let clicked = ui.small_button("Copy").clicked();
                    ui.end_row();
                    if clicked {
                        Some(digest.clone())
                    } else {
                        copied
                    }
                })
            })
            .inner;
        if let Some(digest) = copied {
            ui.ctx().copy_text(digest);
        }
    }

    pub fn show_history_for(&mut self, path: PathBuf) {
        match self.history.clone() {
            None => self.toast_info(HISTORY_MISSING),
            Some(h) => {
                self.feat.history_panel.show_file(h, path);
                self.feat.history_text = None;
                self.feat.show_bottom = Some(BottomTab::History);
            }
        }
    }

    pub fn snapshot_now(&mut self, id: DocId) {
        let limit = self.settings.history_max_mb.saturating_mul(1 << 20);
        let dirty = self.docs.get(&id).is_some_and(|e| e.doc.is_dirty());
        match (self.history.clone(), self.doc_path(id)) {
            (Some(h), Some(path)) => match std::fs::metadata(&path) {
                Err(e) => self.toast_error(format!("Snapshot: cannot read {}: {e}", path.display())),
                Ok(m) if m.len() > limit => self.toast_info(format!(
                    "Snapshot: the file exceeds the history size limit of {} MB",
                    self.settings.history_max_mb
                )),
                Ok(_) => {
                    if dirty {
                        self.toast_info("Snapshot stores the file on disk; unsaved changes are not included");
                    }
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.feat.background.push(spawn_task(move || match h.snapshot(&path) {
                        Ok(Some(_)) => Ok(format!("Stored a version of {name}")),
                        Ok(None) => Ok(format!("{name} is unchanged since its last version")),
                        Err(e) => Err(format!("Snapshot of {name} failed: {e}")),
                    }));
                }
            },
            (None, _) => self.toast_info(HISTORY_MISSING),
            (_, None) => self.toast_info("Snapshot: the document is not saved to a file yet"),
        }
    }

    /// Moves the stored versions after a rename on disk (V4).
    pub fn history_rename(&mut self, old: PathBuf, new: PathBuf) {
        if let Some(h) = self.history.clone() {
            if self.feat.history_panel.file() == Some(old.as_path()) {
                self.feat.history_panel = kladde_panels::HistoryState::new();
            }
            self.feat.background.push(spawn_task(move || {
                h.rename(&old, &new)
                    .map(|_| format!("File history follows {}", new.display()))
                    .map_err(|e| format!("Moving the history of {} failed: {e}", old.display()))
            }));
        }
    }

    /// Replaces the whole content of `id` with `bytes` as one undoable step.
    fn replace_doc_content(&mut self, id: DocId, bytes: &[u8]) {
        let result = self.docs.get_mut(&id).map(|e| {
            e.doc.begin_undo_group();
            let r = e.doc.replace(0..e.doc.len(), bytes);
            e.doc.end_undo_group();
            r
        });
        match result {
            Some(Ok(())) => self.toast_info("Version restored; Undo reverts it"),
            Some(Err(e)) => self.toast_error(format!("Restore failed: {e}")),
            None => {}
        }
    }

    fn restore_version(&mut self, file: PathBuf, bytes: Vec<u8>) {
        match self.doc_by_path(&file) {
            Some(id) => self.replace_doc_content(id, &bytes),
            None => {
                self.open_path(&file, auto_open(), None);
                self.feat.pending_restore = Some((file, bytes, Instant::now()));
            }
        }
    }

    pub fn poll_pending_restore(&mut self) {
        if let Some((file, bytes, at)) = self.feat.pending_restore.take() {
            match self.doc_by_path(&file) {
                Some(id) => self.replace_doc_content(id, &bytes),
                None if at.elapsed() > RESTORE_WAIT => {
                    self.toast_error(format!("Restore: {} could not be opened", file.display()))
                }
                None => self.feat.pending_restore = Some((file, bytes, at)),
            }
        }
    }

    pub fn show_history_tab(&mut self, ui: &mut egui::Ui) {
        let current = self.feat.history_panel.file().map(Path::to_path_buf).and_then(|f| self.doc_by_path(&f));
        let text = current
            .and_then(|id| self.docs.get(&id))
            .filter(|e| e.doc.len() <= TEXT_VIEW_MAX)
            .map(|e| (e.id, e.doc.revision()));
        let cached = self.feat.history_text.as_ref().map(|(d, r, _)| (*d, *r));
        if text.is_some() && text != cached {
            self.feat.history_text = text
                .and_then(|(id, rev)| self.docs.get(&id).map(|e| (id, rev, e.doc.read_string_lossy(0..e.doc.len()))));
        }
        if text.is_none() {
            self.feat.history_text = None;
        }
        let current_text = self.feat.history_text.as_ref().map(|(_, _, t)| t.as_str());
        let events = show_history(ui, &mut self.feat.history_panel, current_text);
        events.into_iter().for_each(|HistoryEvent::Restore { file, bytes }| self.restore_version(file, bytes));
    }

    pub fn show_goto(&mut self, ctx: &egui::Context) {
        let mut submit = false;
        let mut close = false;
        if let Some(goto) = self.feat.goto.as_mut() {
            let hint = if goto.offset { "Byte offset (decimal or 0x hex)" } else { "Line number" };
            egui::Window::new(if goto.offset { "Go to offset" } else { "Go to line" })
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 40.0])
                .show(ctx, |ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut goto.input).hint_text(hint).desired_width(260.0));
                    if std::mem::take(&mut goto.focus) {
                        edit.request_focus();
                    }
                    let (enter, esc) =
                        ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                    submit = edit.lost_focus() && enter;
                    close = esc;
                    ui.horizontal(|ui| {
                        submit |= ui.button("Go").clicked();
                        close |= ui.button("Cancel").clicked();
                    });
                });
        }
        if submit {
            if let Some(goto) = self.feat.goto.take() {
                self.apply_goto(&goto);
            }
        } else if close {
            self.feat.goto = None;
        }
    }

    fn apply_goto(&mut self, goto: &GotoState) {
        let input = goto.input.trim();
        let parsed = if goto.offset {
            match input.strip_prefix("0x").or_else(|| input.strip_prefix("0X")) {
                Some(h) => u64::from_str_radix(h, 16).map_err(|e| e.to_string()),
                None => input.parse::<u64>().map_err(|e| e.to_string()),
            }
        } else {
            let line = input.parse::<u64>().map_err(|e| e.to_string());
            line.and_then(|n| n.checked_sub(1).ok_or_else(|| "lines start at 1".to_string()))
        };
        let result = parsed.and_then(|n| {
            let (entry, tab) = self.active_parts().ok_or("no document open")?;
            match (goto.offset, tab.view) {
                (true, ViewKind::Hex) => {
                    tab.hex.goto(&entry.doc, n);
                    Ok(())
                }
                (true, ViewKind::Text) => {
                    tab.editor.set_caret(&entry.doc, n);
                    Ok(())
                }
                (false, _) => tab
                    .editor
                    .goto_line(&entry.doc, n)
                    .then_some(())
                    .ok_or_else(|| "the line index is still being built; try again shortly".to_string()),
            }
        });
        if let Err(e) = result {
            self.toast_info(format!("Go to: {e}"));
        }
    }

    pub fn show_confirm(&mut self, ctx: &egui::Context) {
        if let Some(confirm) = self.feat.confirm {
            let id = match confirm {
                Confirm::Reload(id) | Confirm::Reopen(id, _) => id,
            };
            let name = self.docs.get(&id).map(|e| e.title.clone());
            let (action, label) = match confirm {
                Confirm::Reload(_) => ("Reloading".to_string(), "Discard and reload".to_string()),
                Confirm::Reopen(_, enc) => (format!("Reopening as {}", enc.name()), "Discard and reopen".to_string()),
            };
            let mut choice = None;
            match name {
                None => self.feat.confirm = None,
                Some(name) => {
                    egui::Window::new("Unsaved changes")
                        .collapsible(false)
                        .resizable(false)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(ctx, |ui| {
                            ui.label(format!("{name} has unsaved changes. {action} discards them."));
                            ui.horizontal(|ui| {
                                if ui.button(label).clicked() {
                                    choice = Some(true);
                                }
                                if ui.button("Cancel").clicked() {
                                    choice = Some(false);
                                }
                            });
                        });
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        choice = Some(false);
                    }
                }
            }
            if let Some(yes) = choice {
                self.feat.confirm = None;
                match (yes, confirm) {
                    (true, Confirm::Reload(id)) => self.reload_doc(id),
                    (true, Confirm::Reopen(id, enc)) => self.do_reopen(id, enc),
                    _ => {}
                }
            }
        }
    }

    pub fn open_diff(&mut self, a: DocId, b: DocId) {
        let text = |id: DocId| {
            self.docs.get(&id).map(|e| {
                let name =
                    e.doc.path().and_then(Path::file_name).map_or(e.title.clone(), |n| n.to_string_lossy().into());
                let text = (e.doc.len() <= TEXT_VIEW_MAX).then(|| e.doc.read_string_lossy(0..e.doc.len()));
                (name, text)
            })
        };
        match (text(a), text(b)) {
            (Some((na, Some(ta))), Some((nb, Some(tb)))) => {
                let rx = spawn_task(move || diff_text(&ta, &tb));
                self.feat.diff =
                    Some(DiffWindow { title: format!("Compare: {na} ↔ {nb}"), lines: Vec::new(), rx: Some(rx) });
            }
            (Some(_), Some(_)) => self.toast_info("Compare: documents above 8 MiB cannot be compared"),
            _ => self.toast_info("Compare: a document was closed"),
        }
    }

    pub fn show_diff_window(&mut self, ctx: &egui::Context) {
        let polled = self.feat.diff.as_ref().and_then(|w| w.rx.as_ref()).and_then(|rx| poll_task(rx, "compare"));
        match polled {
            Some(Ok(lines)) => {
                if let Some(w) = self.feat.diff.as_mut() {
                    w.lines = lines;
                    w.rx = None;
                }
            }
            Some(Err(e)) => {
                self.feat.diff = None;
                self.toast_error(e);
            }
            None => {}
        }
        let theme = if ctx.style().visuals.dark_mode { DiffTheme::dark() } else { DiffTheme::light() };
        let mut open = true;
        if let Some(w) = self.feat.diff.as_ref() {
            egui::Window::new(&w.title)
                .id(egui::Id::new("kladde_diff_window"))
                .open(&mut open)
                .default_size([900.0, 600.0])
                .show(ctx, |ui| {
                    if w.rx.is_some() {
                        ui.spinner();
                        ctx.request_repaint_after(Duration::from_millis(100));
                    } else {
                        let count = |tag: DiffTag| w.lines.iter().filter(|l| l.tag == tag).count();
                        let (ins, del) = (count(DiffTag::Insert), count(DiffTag::Delete));
                        if ins == 0 && del == 0 {
                            ui.label("The documents are identical.");
                        } else {
                            ui.label(format!("+{ins} −{del} lines"));
                        }
                        show_diff(ui, &w.lines, &theme);
                    }
                });
        }
        if !open {
            self.feat.diff = None;
        }
    }

    pub fn toggle_csv(&mut self) {
        let result = self.active_parts().map(|(entry, tab)| {
            if tab.csv_mode {
                tab.csv_mode = false;
                Ok(())
            } else if entry.doc.len() > CSV_MAX_BYTES {
                Err("the table view is limited to documents up to 64 MiB")
            } else {
                tab.csv_mode = true;
                Ok(())
            }
        });
        if let Some(Err(e)) = result {
            self.toast_info(format!("CSV table: {e}"));
        }
    }

    /// Table view of document `doc` (X10); parsed on a worker thread whenever the text changed.
    pub fn show_csv_view(&mut self, ui: &mut egui::Ui, doc: DocId) {
        let docs = &self.docs;
        self.feat.csv.retain(|id, _| docs.contains_key(id));
        let revision = self.docs.get(&doc).map(|e| e.doc.revision());
        let stale =
            revision.is_some_and(|rev| self.feat.csv.get(&doc).is_none_or(|c| c.revision != rev && c.rx.is_none()));
        if let (true, Some(entry)) = (stale, self.docs.get(&doc)) {
            let text = entry.doc.read_string_lossy(0..entry.doc.len().min(CSV_MAX_BYTES));
            let rx = spawn_task(move || parse_csv(&text, CSV_MAX_ROWS));
            let table = self.feat.csv.remove(&doc).and_then(|c| c.table);
            self.feat.csv.insert(doc, CsvCache { revision: entry.doc.revision(), table, rx: Some(rx), error: None });
        }
        let back = match self.feat.csv.get_mut(&doc) {
            None => false,
            Some(cache) => {
                match cache.rx.as_ref().and_then(|rx| poll_task(rx, "CSV parsing")) {
                    Some(Ok(Ok(table))) => {
                        cache.table = Some(table);
                        cache.rx = None;
                    }
                    Some(Ok(Err(e)) | Err(e)) => {
                        cache.error = Some(e);
                        cache.rx = None;
                    }
                    None => {}
                }
                let back = ui
                    .horizontal(|ui| {
                        let back = ui.button("Show as text").clicked();
                        match &cache.table {
                            Some(t) if t.rows.len() >= CSV_MAX_ROWS => {
                                ui.label(format!("first {} rows", t.rows.len()));
                            }
                            Some(t) => {
                                ui.label(format!("{} rows", t.rows.len()));
                            }
                            None => {}
                        }
                        if cache.rx.is_some() {
                            ui.spinner();
                            ui.ctx().request_repaint_after(Duration::from_millis(100));
                        }
                        back
                    })
                    .inner;
                if let Some(e) = &cache.error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Not a CSV table: {e}"));
                }
                if let Some(t) = &cache.table {
                    show_csv(ui, t);
                }
                back
            }
        };
        if back {
            self.views_of(doc).for_each(|t| t.csv_mode = false);
        }
    }

    pub fn show_settings_dialog(&mut self, ctx: &egui::Context) {
        if self.feat.settings_open {
            let changed = show_settings(ctx, &mut self.feat.settings_open, &mut self.settings);
            if changed {
                self.keymap = Keymap::new(&self.settings.keybindings);
                self.feat.settings_dirty = true;
                let errors = self.keymap.errors().join("; ");
                if !errors.is_empty() {
                    self.toast_error(errors);
                }
            }
            if !self.feat.settings_open && self.feat.settings_dirty {
                self.feat.settings_dirty = false;
                self.store_settings();
            }
        }
    }
}

/// Command id of an edit command, for error messages.
fn cmd_id(cmd: Cmd) -> &'static str {
    match cmd {
        Cmd::Undo => "edit.undo",
        Cmd::Redo => "edit.redo",
        Cmd::EditCut => "edit.cut",
        Cmd::EditCopy => "edit.copy",
        Cmd::EditPaste => "edit.paste",
        Cmd::SelectAll => "edit.select_all",
        Cmd::DuplicateLine => "edit.duplicate_line",
        Cmd::DeleteLine => "edit.delete_line",
        Cmd::ToggleComment => "edit.toggle_comment",
        Cmd::BookmarkToggle => "bookmark.toggle",
        Cmd::BookmarkNext => "bookmark.next",
        Cmd::BookmarkPrev => "bookmark.prev",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_bytes(text.as_bytes().to_vec())
    }

    fn text(d: &Document) -> String {
        d.read_string_lossy(0..d.len())
    }

    #[test]
    fn duplicate_and_delete_cover_last_line_without_terminator() {
        let mut d = doc("a\nb");
        let mut ed = EditorState::new(None);
        ed.set_caret(&d, 3);
        duplicate_lines(&mut d, &mut ed).expect("editable");
        assert_eq!(text(&d), "a\nb\nb");
        delete_lines(&mut d, &mut ed).expect("editable");
        assert_eq!(text(&d), "a\nb");
        ed.set_caret(&d, 0);
        delete_lines(&mut d, &mut ed).expect("editable");
        assert_eq!(text(&d), "b");
    }

    #[test]
    fn line_blocks_merge_selections_on_shared_lines() {
        let d = doc("one\ntwo\nthree\n");
        let sels = [Selection { anchor: 0, caret: 5 }, Selection::caret(6), Selection::caret(9)];
        assert_eq!(line_blocks(&d, &sels), vec![0..8, 8..14]);
        // A selection ending at a line start does not include that line.
        assert_eq!(line_blocks(&d, &[Selection { anchor: 0, caret: 4 }]), vec![0..4]);
    }
}
