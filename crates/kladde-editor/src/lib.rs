//! Text view widget: byte-offset anchored rows for huge files, syntax colors, folds, multi-cursor editing.

mod edit;
mod rows;
mod text;
mod theme;
mod view;

use std::cell::Cell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;

use kladde_core::{CoreError, Document};
use kladde_syntax::{compute_folds, language_for_path, languages, FoldRange, HighlightCache, Language, LineState};
use kladde_tools::Interpreter;

use crate::edit::{map_offset, normalize, Plan};
use crate::rows::{Layout, Rows};
use crate::text::{bytes, content_end, find_newline, near_line_start, snap, text_cols, LONG_LINE};

pub use theme::EditorTheme;
pub use view::show;

/// Lines lexed per frame at most while catching up the highlight state of a far line.
const HIGHLIGHT_BUDGET: u64 = 20_000;
/// Fences and folds are computed on the full text only up to this size.
const SCAN_LIMIT: u64 = 8 << 20;
/// Folds are recomputed this long after the last change.
const FOLD_DEBOUNCE: f64 = 0.3;
/// Line numbers of offsets this close to the last lookup are counted from it.
const LINE_HINT_RANGE: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct EditorConfig {
    pub font_size: f32,
    pub tab_width: u8,
    pub insert_spaces: bool,
    pub show_whitespace: bool,
    pub word_wrap: bool,
    pub show_line_numbers: bool,
    /// Above this size: no highlighting, no wrap, no folds, no fence scan.
    pub large_file_bytes: u64,
}

impl Default for EditorConfig {
    fn default() -> Self {
        EditorConfig {
            font_size: 14.0,
            tab_width: 4,
            insert_spaces: true,
            show_whitespace: false,
            word_wrap: false,
            show_line_numbers: true,
            large_file_bytes: 64 << 20,
        }
    }
}

/// Byte offsets; `caret` is where typing happens, `anchor` the other end of the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: u64,
    pub caret: u64,
}

impl Selection {
    pub fn caret(off: u64) -> Selection {
        Selection { anchor: off, caret: off }
    }

    pub fn range(&self) -> Range<u64> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.caret
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditorEvent {
    Run { interp: Interpreter, code: String, in_terminal: bool },
    Error(String),
}

pub struct EditorOutput {
    pub events: Vec<EditorEvent>,
    pub changed: bool,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reveal {
    Minimal,
    Center,
}

/// A collapsed fold: the header line start and the hidden lines after it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Collapsed {
    header: u64,
    hidden: Range<u64>,
}

/// A fenced code block whose language maps to an interpreter.
#[derive(Clone, Debug, PartialEq)]
struct Fence {
    header_line: u64,
    interp: Interpreter,
    content: Range<u64>,
}

#[derive(Clone, Debug, PartialEq)]
enum Drag {
    Char { anchor: u64 },
    Word { range: Range<u64> },
    Line { range: Range<u64> },
    Box { anchor_ls: u64, anchor_col: usize },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MenuAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    Run(Interpreter),
}

/// (revision, caret, index ready) -> (line, column)
type LineColCache = Option<(u64, u64, bool, Option<u64>, u64)>;

pub struct EditorState {
    lang: &'static Language,
    sels: Vec<Selection>,
    /// Visual column kept by vertical moves, parallel to `sels`.
    goals: Vec<Option<usize>>,
    scroll_top: u64,
    scroll_x: f32,
    wheel_acc: f32,
    reveal: Option<Reveal>,
    /// Revision the selections, bookmarks and folds were last adjusted to.
    seen_rev: Option<u64>,
    layout: Layout,
    view_rows: usize,
    visible_bytes: u64,
    hl: HighlightCache,
    hl_rev: Option<u64>,
    /// States of lines `0..=hl_known` are cached.
    hl_known: u64,
    bookmarks: BTreeSet<u64>,
    find_matches: Vec<Range<u64>>,
    folds: Vec<FoldRange>,
    fold_rev: Option<u64>,
    fold_seen: Option<u64>,
    fold_due: f64,
    fold_all_pending: bool,
    collapsed: Vec<Collapsed>,
    fences: Vec<Fence>,
    fence_rev: Option<u64>,
    drag: Option<Drag>,
    clicks: (f64, egui::Pos2, u8),
    vbar_grab: Option<f32>,
    hbar_grab: Option<f32>,
    preedit: String,
    menu_action: Option<MenuAction>,
    interpreters: Option<Vec<Interpreter>>,
    line_col: Cell<LineColCache>,
    /// (revision, offset, line) of the last line lookup.
    line_hint: Cell<Option<(u64, u64, u64)>>,
}

fn is_line_start(doc: &Document, p: u64) -> bool {
    p == 0 || doc.byte_at(p - 1) == Some(b'\n')
}

/// Text of 0-based `line` without terminator, at most 64 KiB.
fn line_text(doc: &Document, line: u64) -> String {
    let len = doc.len();
    let ls = doc.line_to_offset(line).unwrap_or(len);
    let end = find_newline(doc, ls, LONG_LINE + 2).map_or(len, |q| content_end(doc, q, ls)).min(ls + LONG_LINE);
    String::from_utf8_lossy(&bytes(doc, ls..end)).into_owned()
}

impl EditorState {
    /// Picks the language from `path` (plain text without one).
    pub fn new(path: Option<&Path>) -> EditorState {
        let lang = path.map_or(&languages()[0], language_for_path);
        EditorState {
            lang,
            sels: vec![Selection::caret(0)],
            goals: vec![None],
            scroll_top: 0,
            scroll_x: 0.0,
            wheel_acc: 0.0,
            reveal: None,
            seen_rev: None,
            layout: Layout { wrap: None, tab: 4 },
            view_rows: 1,
            visible_bytes: 0,
            hl: HighlightCache::new(lang),
            hl_rev: None,
            hl_known: 0,
            bookmarks: BTreeSet::new(),
            find_matches: Vec::new(),
            folds: Vec::new(),
            fold_rev: None,
            fold_seen: None,
            fold_due: 0.0,
            fold_all_pending: false,
            collapsed: Vec::new(),
            fences: Vec::new(),
            fence_rev: None,
            drag: None,
            clicks: (f64::NEG_INFINITY, egui::Pos2::ZERO, 0),
            vbar_grab: None,
            hbar_grab: None,
            preedit: String::new(),
            menu_action: None,
            interpreters: None,
            line_col: Cell::new(None),
            line_hint: Cell::new(None),
        }
    }

    pub fn language(&self) -> &'static Language {
        self.lang
    }

    pub fn set_language(&mut self, l: &'static Language) {
        self.lang = l;
        self.hl = HighlightCache::new(l);
        self.hl_known = 0;
        self.folds.clear();
        self.collapsed.clear();
        self.fold_rev = None;
        self.fold_seen = None;
    }

    /// Never empty; `[0]` is the primary selection.
    pub fn selections(&self) -> &[Selection] {
        &self.sels
    }

    pub fn caret(&self) -> u64 {
        self.sels[0].caret
    }

    /// Single cursor at `off`, scrolled into view.
    pub fn set_caret(&mut self, doc: &Document, off: u64) {
        self.sync(doc);
        self.set_sels(vec![Selection::caret(snap(doc, off))]);
        self.reveal = Some(Reveal::Center);
    }

    /// Single selection, scrolled into view.
    pub fn select(&mut self, doc: &Document, range: Range<u64>) {
        self.sync(doc);
        self.set_sels(vec![Selection { anchor: snap(doc, range.start), caret: snap(doc, range.end) }]);
        self.reveal = Some(Reveal::Center);
    }

    /// Replaces all selections (first is primary); offsets are clamped to the document.
    pub fn set_selections(&mut self, doc: &Document, sels: Vec<Selection>) {
        self.sync(doc);
        let sels: Vec<Selection> =
            sels.iter().map(|s| Selection { anchor: snap(doc, s.anchor), caret: snap(doc, s.caret) }).collect();
        if !sels.is_empty() {
            self.set_sels(sels);
            self.reveal = Some(Reveal::Minimal);
        }
    }

    pub fn primary_range(&self) -> Range<u64> {
        self.sels[0].range()
    }

    pub fn selected_text(&self, doc: &Document) -> String {
        doc.read_string_lossy(self.primary_range())
    }

    /// Replaces the primary selection in one undo group; the new text stays selected if the old one was.
    pub fn replace_selection(&mut self, doc: &mut Document, text: &str) -> Result<(), CoreError> {
        self.sync(doc);
        let r = self.primary_range();
        let end = r.start + text.len() as u64;
        let sel = if r.is_empty() { Selection::caret(end) } else { Selection { anchor: r.start, caret: end } };
        let plan = Plan { edits: vec![edit::Edit { range: r, text: text.as_bytes().to_vec() }], sels: vec![sel] };
        doc.begin_undo_group();
        let result = self.commit(doc, plan);
        doc.end_undo_group();
        result.map(|_| ())
    }

    pub fn scroll_top(&self) -> u64 {
        self.scroll_top
    }

    /// Scrolls so the row containing `off` is the first visible one.
    pub fn set_scroll_top(&mut self, doc: &Document, off: u64) {
        self.sync(doc);
        let hidden = self.hidden();
        let rows = Rows { doc, layout: self.layout, hidden: &hidden };
        self.scroll_top = rows.row_at(off.min(doc.len())).start;
    }

    /// Moves the caret to the start of 0-based `line`; false while the line index is building.
    pub fn goto_line(&mut self, doc: &Document, line: u64) -> bool {
        let target = doc.line_count().and_then(|n| doc.line_to_offset(line.min(n.saturating_sub(1))));
        target.inspect(|&off| self.set_caret(doc, off)).is_some()
    }

    /// 0-based line (if the index is ready) and column in chars with tabs expanded. Beyond 64 KiB into a line
    /// the column counts bytes.
    pub fn caret_line_col(&self, doc: &Document) -> (Option<u64>, u64) {
        let (rev, caret, ready) = (doc.revision(), self.caret().min(doc.len()), doc.line_count().is_some());
        match self.line_col.get() {
            Some((r, c, k, line, col)) if r == rev && c == caret && k == ready => (line, col),
            _ => {
                let line = self.line_of(doc, caret);
                let ls = line.and_then(|l| doc.line_to_offset(l)).unwrap_or_else(|| near_line_start(doc, caret));
                let col = if caret - ls <= LONG_LINE {
                    text_cols(&bytes(doc, ls..caret), self.layout.tab) as u64
                } else {
                    caret - ls
                };
                self.line_col.set(Some((rev, caret, ready, line, col)));
                (line, col)
            }
        }
    }

    /// Line comment if the language has one, else block comment.
    pub fn toggle_comment(&mut self, doc: &mut Document) -> Result<(), CoreError> {
        self.sync(doc);
        let plan = edit::toggle_comment(doc, &self.sels, self.lang);
        self.commit(doc, plan).map(|_| ())
    }

    pub fn toggle_bookmark(&mut self, doc: &Document) {
        self.sync(doc);
        let ls = doc.line_start_at(self.caret());
        if !self.bookmarks.remove(&ls) {
            self.bookmarks.insert(ls);
        }
    }

    pub fn next_bookmark(&mut self, doc: &Document) {
        self.sync(doc);
        let ls = doc.line_start_at(self.caret());
        let target = self.bookmarks.range(ls + 1..).next().or(self.bookmarks.iter().next()).copied();
        target.into_iter().for_each(|b| self.set_caret(doc, b));
    }

    pub fn prev_bookmark(&mut self, doc: &Document) {
        self.sync(doc);
        let ls = doc.line_start_at(self.caret());
        let target = self.bookmarks.range(..ls).next_back().or(self.bookmarks.iter().next_back()).copied();
        target.into_iter().for_each(|b| self.set_caret(doc, b));
    }

    /// Line starts of bookmarked lines.
    pub fn bookmarks(&self) -> impl Iterator<Item = u64> + '_ {
        self.bookmarks.iter().copied()
    }

    /// Ranges painted as find matches.
    pub fn set_find_matches(&mut self, mut matches: Vec<Range<u64>>) {
        matches.sort_by_key(|m| (m.start, m.end));
        self.find_matches = matches;
    }

    /// Collapses every fold on the next frame.
    pub fn fold_all(&mut self) {
        self.fold_all_pending = true;
    }

    pub fn unfold_all(&mut self) {
        self.fold_all_pending = false;
        self.collapsed.clear();
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        self.sync(doc);
        let hint = doc.undo();
        hint.inspect(|&h| self.after_history(doc, h)).is_some()
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        self.sync(doc);
        let hint = doc.redo();
        hint.inspect(|&h| self.after_history(doc, h)).is_some()
    }

    pub fn select_all(&mut self, doc: &Document) {
        self.sync(doc);
        self.set_sels(vec![Selection { anchor: 0, caret: doc.len() }]);
    }

    fn after_history(&mut self, doc: &Document, hint: u64) {
        self.sync(doc);
        self.set_sels(vec![Selection::caret(snap(doc, hint))]);
        self.reveal = Some(Reveal::Minimal);
    }

    /// Adjusts to changes not made through this state: offsets after the first changed byte are clamped and
    /// snapped, folds touching the change open, find matches after it are dropped.
    fn sync(&mut self, doc: &Document) {
        let rev = doc.revision();
        if self.seen_rev != Some(rev) {
            let len = doc.len();
            let from = match self.seen_rev {
                Some(seen) => doc.changed_since(seen).unwrap_or(0),
                None => len,
            };
            let fix = |p: u64| if p >= from { snap(doc, p.min(len)) } else { p };
            let sels = self.sels.iter().map(|s| Selection { anchor: fix(s.anchor), caret: fix(s.caret) }).collect();
            self.bookmarks = self
                .bookmarks
                .iter()
                .map(|&b| if b < from || is_line_start(doc, b.min(len)) { b.min(len) } else { doc.line_start_at(b) })
                .collect();
            self.collapsed.retain(|c| c.hidden.end < from);
            self.find_matches.retain(|m| m.end < from);
            self.seen_rev = Some(rev);
            self.set_sels(sels);
        }
    }

    fn set_sels(&mut self, sels: Vec<Selection>) {
        self.sels = normalize(sels);
        self.goals = vec![None; self.sels.len()];
        self.collapsed.retain(|c| !self.sels.iter().any(|s| c.hidden.contains(&s.caret)));
    }

    /// Applies a plan and moves bookmarks, folds and find matches along with the text.
    fn commit(&mut self, doc: &mut Document, plan: Plan) -> Result<bool, CoreError> {
        if plan.edits.is_empty() {
            self.set_sels(plan.sels);
            Ok(false)
        } else {
            edit::apply(doc, &plan.edits)?;
            let e = &plan.edits;
            self.bookmarks = self
                .bookmarks
                .iter()
                .map(|&b| map_offset(e, b, false))
                .map(|b| if is_line_start(doc, b) { b } else { doc.line_start_at(b) })
                .collect();
            self.collapsed = self
                .collapsed
                .iter()
                .map(|c| Collapsed {
                    header: map_offset(e, c.header, false),
                    hidden: map_offset(e, c.hidden.start, false)..map_offset(e, c.hidden.end, false),
                })
                .filter(|c| c.header < c.hidden.start && c.hidden.start < c.hidden.end)
                .collect();
            self.find_matches = self
                .find_matches
                .iter()
                .map(|m| map_offset(e, m.start, false)..map_offset(e, m.end, false))
                .filter(|m| !m.is_empty())
                .collect();
            self.seen_rev = Some(doc.revision());
            self.set_sels(plan.sels);
            self.reveal = Some(Reveal::Minimal);
            Ok(true)
        }
    }

    /// Sorted, merged hidden ranges of collapsed folds.
    fn hidden(&self) -> Vec<Range<u64>> {
        let mut ranges: Vec<Range<u64>> = self.collapsed.iter().map(|c| c.hidden.clone()).collect();
        ranges.sort_by_key(|r| r.start);
        ranges.into_iter().fold(Vec::new(), |mut acc: Vec<Range<u64>>, r| {
            match acc.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => acc.push(r),
            }
            acc
        })
    }

    /// `offset_to_line`, counting newlines from the previous answer when it is near: the index rescans up to
    /// 1024 lines (or a whole long line) per lookup.
    fn line_of(&self, doc: &Document, off: u64) -> Option<u64> {
        let rev = doc.revision();
        let near = self.line_hint.get().filter(|&(r, o, _)| r == rev && o.abs_diff(off) <= LINE_HINT_RANGE);
        let line = match near {
            Some((_, o, l)) => {
                let count = bytes(doc, o.min(off)..o.max(off)).iter().filter(|&&b| b == b'\n').count() as u64;
                Some(if off >= o { l + count } else { l - count })
            }
            None => doc.offset_to_line(off),
        };
        if let Some(l) = line {
            self.line_hint.set(Some((rev, off, l)));
        }
        line
    }

    /// Start-of-line highlight state, lexing at most a budget of lines per call; None while catching up.
    fn line_state(&mut self, doc: &Document, line: u64) -> Option<LineState> {
        let mut get = |k: u64| line_text(doc, k);
        let reachable = line <= self.hl_known + HIGHLIGHT_BUDGET;
        let target = if reachable { line } else { self.hl_known + HIGHLIGHT_BUDGET };
        let state = self.hl.state_for_line(target, &mut get);
        self.hl_known = self.hl_known.max(target);
        reachable.then_some(state)
    }

    /// Per-frame upkeep: highlight invalidation, fence scan, debounced folds.
    fn refresh(&mut self, doc: &Document, large: bool, now: f64, ctx: &egui::Context) {
        let rev = doc.revision();
        if let Some(r) = self.hl_rev.filter(|&r| r != rev) {
            let line = doc.changed_since(r).and_then(|off| doc.offset_to_line(off)).unwrap_or(0);
            self.hl.invalidate_from_line(line);
            self.hl_known = self.hl_known.min(line);
        }
        self.hl_rev = Some(rev);
        if large || doc.len() > SCAN_LIMIT {
            self.fences.clear();
            self.folds.clear();
            self.collapsed.clear();
            self.fold_all_pending = false;
        } else {
            if self.fence_rev != Some(rev) {
                let text = doc.read_string_lossy(0..doc.len());
                self.fences = kladde_tools::find_fences(&text)
                    .into_iter()
                    .filter_map(|f| {
                        Interpreter::from_lang(&f.lang).map(|interp| Fence {
                            header_line: f.header_line as u64,
                            interp,
                            content: f.content.start as u64..f.content.end as u64,
                        })
                    })
                    .collect();
                self.fence_rev = Some(rev);
            }
            if self.fold_seen != Some(rev) {
                self.fold_due = now + if self.fold_seen.is_some() { FOLD_DEBOUNCE } else { 0.0 };
                self.fold_seen = Some(rev);
            }
            if self.fold_rev != Some(rev) {
                if now >= self.fold_due {
                    self.recompute_folds(doc);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(self.fold_due - now));
                }
            }
            if self.fold_all_pending {
                self.fold_all_pending = false;
                self.fresh_folds(doc);
                let collapsed: Vec<Collapsed> = self.folds.iter().filter_map(|f| collapse(doc, f)).collect();
                self.collapse(collapsed);
            }
        }
    }

    fn fresh_folds(&mut self, doc: &Document) {
        if self.fold_rev != Some(doc.revision()) {
            self.recompute_folds(doc);
        }
    }

    fn recompute_folds(&mut self, doc: &Document) {
        let text = doc.read_string_lossy(0..doc.len());
        self.folds = compute_folds(self.lang, &text);
        self.fold_rev = Some(doc.revision());
        let folds = &self.folds;
        self.collapsed = self
            .collapsed
            .iter()
            .filter_map(|c| {
                let line = doc.offset_to_line(c.header)?;
                folds.iter().find(|f| f.start_line == line).and_then(|f| collapse(doc, f))
            })
            .collect();
    }

    /// Adds collapsed folds, moving carets they would hide to the fold header.
    fn collapse(&mut self, added: Vec<Collapsed>) {
        let out_of = |p: u64| added.iter().find(|c| c.hidden.contains(&p)).map_or(p, |c| c.header);
        let sels: Vec<Selection> =
            self.sels.iter().map(|s| Selection { anchor: out_of(s.anchor), caret: out_of(s.caret) }).collect();
        self.collapsed.extend(added);
        self.set_sels(sels);
    }

    /// Toggles the fold starting at 0-based `line`.
    fn toggle_fold(&mut self, doc: &Document, line: u64) {
        self.fresh_folds(doc);
        let header = doc.line_to_offset(line);
        match self.collapsed.iter().position(|c| Some(c.header) == header) {
            Some(i) => {
                self.collapsed.remove(i);
            }
            None => {
                let added = self.folds.iter().find(|f| f.start_line == line).and_then(|f| collapse(doc, f));
                self.collapse(added.into_iter().collect());
            }
        }
    }

    fn fence_at_line(&self, line: u64) -> Option<&Fence> {
        self.fences.binary_search_by_key(&line, |f| f.header_line).ok().map(|i| &self.fences[i])
    }

    fn is_fold_start(&self, line: u64) -> bool {
        self.folds.binary_search_by_key(&line, |f| f.start_line).is_ok()
    }
}

fn collapse(doc: &Document, f: &FoldRange) -> Option<Collapsed> {
    let header = doc.line_to_offset(f.start_line)?;
    let start = doc.line_to_offset(f.start_line + 1)?;
    let end = doc.line_to_offset(f.end_line + 1).unwrap_or(doc.len());
    (start < end).then_some(Collapsed { header, hidden: start..end })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::from_bytes(s.as_bytes().to_vec())
    }

    #[test]
    fn external_edits_clamp_selections_and_undo_moves_caret() {
        let mut d = doc("hello world");
        let mut st = EditorState::new(None);
        st.set_selections(&d, vec![Selection::caret(2), Selection { anchor: 6, caret: 11 }]);
        d.delete(3..11).expect("writable");
        st.sync(&d);
        assert_eq!(st.selections(), &[Selection::caret(2), Selection::caret(3)]);
        st.set_caret(&d, 3);
        st.replace_selection(&mut d, "p!").expect("writable");
        assert_eq!(d.read_string_lossy(0..d.len()), "help!");
        assert!(st.undo(&mut d));
        assert_eq!(d.read_string_lossy(0..d.len()), "hel");
        assert_eq!(st.caret(), 3);
    }

    #[test]
    fn toggle_comment_via_state_is_one_undo_step() {
        let mut d = doc("a\nb\n");
        let mut st = EditorState::new(Some(Path::new("x.py")));
        st.select_all(&d);
        st.toggle_comment(&mut d).expect("writable");
        assert_eq!(d.read_string_lossy(0..d.len()), "# a\n# b\n");
        st.toggle_comment(&mut d).expect("writable");
        assert_eq!(d.read_string_lossy(0..d.len()), "a\nb\n");
        assert!(st.undo(&mut d));
        assert_eq!(d.read_string_lossy(0..d.len()), "# a\n# b\n");
    }

    #[test]
    fn bookmarks_follow_edits_and_cycle() {
        let mut d = doc("a\nb\nc\n");
        let mut st = EditorState::new(None);
        st.set_caret(&d, 2);
        st.toggle_bookmark(&d);
        st.set_caret(&d, 4);
        st.toggle_bookmark(&d);
        st.set_caret(&d, 0);
        let plan = edit::type_text(st.selections(), b"xx\n");
        st.commit(&mut d, plan).expect("writable");
        assert_eq!(st.bookmarks().collect::<Vec<_>>(), vec![5, 7]);
        st.next_bookmark(&d);
        assert_eq!(st.caret(), 5);
        st.next_bookmark(&d);
        assert_eq!(st.caret(), 7);
        st.next_bookmark(&d);
        assert_eq!(st.caret(), 5);
        st.prev_bookmark(&d);
        assert_eq!(st.caret(), 7);
    }

    #[test]
    fn caret_column_expands_tabs() {
        let d = doc("x\n\tab");
        let mut st = EditorState::new(None);
        st.set_caret(&d, 4);
        assert_eq!(st.caret_line_col(&d), (Some(1), 5));
    }
}
