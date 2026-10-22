//! The `show` function: input handling, row layout with egui galleys, painting.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    pos2, vec2, Align2, Color32, Context, CursorIcon, Event, EventFilter, FontId, Galley, Id, ImeEvent, Key, Modifiers,
    Pos2, Rect, Response, Sense, Shape, Stroke, Ui, ViewportCommand,
};
use kladde_core::{CoreError, Document, LineEnding, MEMORY_LIMIT};
use kladde_syntax::{highlight_line, Token};
use kladde_tools::Interpreter;

use crate::edit;
use crate::rows::{Row, Rows};
use crate::text::{
    bytes, char_cols, content_end, find_newline, line_col, match_bracket, near_line_start, next_boundary,
    prev_boundary, word_at, word_left, word_right, Chars, LONG_LINE,
};
use crate::{Drag, EditorConfig, EditorEvent, EditorOutput, EditorState, EditorTheme, MenuAction, Reveal, Selection};

/// Rows longer than this many bytes only lay out the horizontally visible part.
const CLIP_BYTES: u64 = 1024;
/// Columns laid out beyond each side of the view when clipping.
const CLIP_MARGIN: usize = 16;
const BAR: f32 = 12.0;
const MULTI_CLICK_SECS: f64 = 0.4;
const OFFSCREEN: f32 = 1.0e5;

struct Out {
    events: Vec<EditorEvent>,
    changed: bool,
    error_sent: bool,
}

impl Out {
    fn error(&mut self, msg: String) {
        if !self.error_sent {
            self.error_sent = true;
            self.events.push(EditorEvent::Error(msg));
        }
    }

    fn result(&mut self, r: Result<bool, CoreError>) {
        match r {
            Ok(changed) => self.changed |= changed,
            Err(e) => self.error(e.to_string()),
        }
    }
}

/// A laid out visible row.
struct VisRow {
    row: Row,
    line: Option<u64>,
    galley: Arc<Galley>,
    /// `(byte offset, galley char index)` per laid out char plus an end sentinel.
    map: Vec<(u64, usize)>,
    /// x of the first laid out char relative to the row origin.
    x0: f32,
}

impl VisRow {
    /// x of `off` relative to the row origin.
    fn x_of(&self, off: u64) -> f32 {
        let off = off.min(self.row.end);
        let first = self.map[0];
        let last = self.map[self.map.len() - 1];
        if off < first.0 {
            self.x0 - OFFSCREEN
        } else if off > last.0 {
            self.x0 + self.galley.size().x + OFFSCREEN
        } else {
            let k = self.map.partition_point(|e| e.0 <= off).saturating_sub(1);
            self.x0 + self.galley.rows.first().map_or(0.0, |r| r.x_offset(self.map[k].1))
        }
    }

    /// Offset nearest to x (relative to the row origin).
    fn off_at_x(&self, x: f32, doc_len: u64) -> u64 {
        let g = self.galley.rows.first().map_or(0, |r| r.char_at(x - self.x0));
        let k = self.map.partition_point(|e| e.1 <= g).saturating_sub(1);
        let (off, gi) = self.map[k];
        let off = match self.map.get(k + 1) {
            Some(&(next, ng)) if g > gi && (g - gi) * 2 > ng - gi => next,
            _ => off,
        };
        let r = self.row;
        if off == r.end && r.next == r.end && r.end > r.start && r.end < doc_len {
            self.map.iter().rev().nth(1).map_or(off, |e| e.0)
        } else {
            off
        }
    }

    /// Whether a caret at `c` is drawn in this row.
    fn holds_caret(&self, c: u64, doc_len: u64) -> bool {
        let r = self.row;
        r.start <= c && (c < r.next || (c == r.end && r.end == doc_len))
    }
}

/// Collects runs of equally colored text into a layout job.
struct JobBuilder {
    job: LayoutJob,
    run: String,
    color: Color32,
    font: FontId,
}

impl JobBuilder {
    fn push(&mut self, text: &str, color: Color32) {
        if color != self.color && !self.run.is_empty() {
            self.flush();
        }
        self.color = color;
        self.run.push_str(text);
    }

    fn flush(&mut self) {
        let run = std::mem::take(&mut self.run);
        self.job.append(&run, 0.0, TextFormat::simple(self.font.clone(), self.color));
    }

    fn finish(mut self) -> LayoutJob {
        self.flush();
        self.job
    }
}

/// Geometry of one frame.
struct Frame {
    text_rect: Rect,
    origin_x: f32,
    row_h: f32,
    char_w: f32,
    font: FontId,
}

impl Frame {
    fn row_index(&self, y: f32, count: usize) -> usize {
        (((y - self.text_rect.top()) / self.row_h).floor().max(0.0) as usize).min(count.saturating_sub(1))
    }
}

fn eol_bytes(doc: &Document) -> &'static [u8] {
    match doc.line_ending() {
        LineEnding::Lf => b"\n",
        LineEnding::CrLf => b"\r\n",
        LineEnding::Cr => b"\r",
    }
}

pub fn show(
    ui: &mut Ui,
    id: Id,
    doc: &mut Document,
    state: &mut EditorState,
    cfg: &EditorConfig,
    theme: &EditorTheme,
) -> EditorOutput {
    let mut out = Out { events: Vec::new(), changed: false, error_sent: false };
    doc.poll_line_index();
    if let Some(e) = doc.take_error() {
        out.events.push(EditorEvent::Error(e.to_string()));
    }
    state.sync(doc);
    let large = doc.len() > cfg.large_file_bytes;
    let now = ui.input(|i| i.time);
    state.refresh(doc, large, now, ui.ctx());

    let font = FontId::monospace(cfg.font_size);
    let (row_h, char_w) = ui.fonts(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
    let outer = ui.available_rect_before_wrap();
    ui.advance_cursor_after_rect(outer);

    let digits = doc.line_count().map_or(4, |n| n.to_string().len()).max(3);
    let numbers_w = if cfg.show_line_numbers { (digits as f32 + 1.0) * char_w } else { 0.0 };
    let mark_w = row_h;
    let run_w = if state.fences.is_empty() { 0.0 } else { row_h };
    let fold_w = if large { 0.0 } else { row_h * 0.8 };
    let gutter = Rect::from_min_size(outer.min, vec2(numbers_w + mark_w + run_w + fold_w, outer.height()));
    let vbar = Rect::from_min_max(pos2(outer.right() - BAR, outer.top()), outer.max);
    let text_rect = Rect::from_min_max(pos2(gutter.right(), outer.top()), pos2(vbar.left(), outer.bottom()));
    let text_left = text_rect.left() + char_w * 0.5;
    let text_w = (text_rect.right() - text_left).max(char_w);

    let wrap = (cfg.word_wrap && !large).then(|| ((text_w / char_w).floor() as usize).max(8));
    state.layout = crate::rows::Layout { wrap, tab: usize::from(cfg.tab_width.max(1)) };
    state.view_rows = ((text_rect.height() / row_h).floor() as usize).max(1);
    if wrap.is_some() {
        state.scroll_x = 0.0;
    }

    let resp = ui.interact(text_rect, id, Sense::click_and_drag());
    let gutter_resp = ui.interact(gutter, id.with("gutter"), Sense::click());
    let vbar_resp = ui.interact(vbar, id.with("vbar"), Sense::click_and_drag());
    if resp.is_pointer_button_down_on() || gutter_resp.clicked() {
        resp.request_focus();
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    let focused = resp.has_focus();
    if focused {
        let filter = EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false };
        ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
    }

    if let Some(action) = state.menu_action.take() {
        menu_action(ui.ctx(), action, doc, state, &mut out);
    }
    if focused {
        let events = ui.input(|i| i.events.clone());
        events.iter().for_each(|ev| handle_event(ui.ctx(), ev, doc, state, cfg, &mut out));
    }

    if ui.rect_contains_pointer(outer) {
        let delta = ui.input(|i| i.smooth_scroll_delta);
        state.wheel_acc -= delta.y / row_h;
        let n = state.wheel_acc.trunc();
        state.wheel_acc -= n;
        scroll_rows(state, doc, n as i64);
        if wrap.is_none() {
            state.scroll_x = (state.scroll_x - delta.x).max(0.0);
        }
    }

    vscrollbar_input(ui, &vbar_resp, vbar, doc, state);

    if let Some(mode) = state.reveal.take() {
        reveal(state, doc, mode, char_w, text_w);
    }

    let hidden = state.hidden();
    let vis_rows = {
        let rows = Rows { doc, layout: state.layout, hidden: &hidden };
        state.scroll_top = rows.row_at(state.scroll_top).start;
        let want = state.view_rows + 1;
        let first: Vec<Row> = rows.iter_from(state.scroll_top).take(want).collect();
        // Never leave empty space below the last row while there is text above.
        if first.len() < state.view_rows && state.scroll_top > 0 {
            state.scroll_top = rows.back(first[0], state.view_rows - first.len());
            rows.iter_from(state.scroll_top).take(want).collect()
        } else {
            first
        }
    };
    state.visible_bytes = vis_rows.last().map_or(0, |r| r.next.saturating_sub(state.scroll_top));

    let frame = Frame { text_rect, origin_x: text_left - state.scroll_x, row_h, char_w, font };
    let vis = layout_rows(ui, doc, state, cfg, theme, &frame, &vis_rows, &hidden, large, text_w);

    if !vis.is_empty() {
        mouse(ui, &resp, doc, state, &frame, &vis, &hidden);
        gutter_input(ui, &gutter_resp, doc, state, &vis, numbers_w, mark_w, run_w, &frame);
    }
    context_menu(&resp, doc, state);

    let hbar = hscrollbar(ui, id, state, &vis, &frame, text_w);

    paint(ui, doc, state, cfg, theme, &frame, &vis, outer, gutter, numbers_w, mark_w, run_w, fold_w, focused);
    paint_vscrollbar(ui, doc, state, theme, vbar, &vis_rows);
    if !vis.is_empty() {
        run_markers(ui, id, doc, state, &vis, gutter.left() + numbers_w + mark_w, run_w, theme, &frame, &mut out);
    }
    if let Some((bar, thumb)) = hbar {
        ui.painter().rect_filled(bar, 0.0, theme.scrollbar);
        ui.painter().rect_filled(thumb, 3.0, theme.scrollbar_thumb);
    }
    EditorOutput { events: out.events, changed: out.changed, focused }
}

fn scroll_rows(state: &mut EditorState, doc: &Document, n: i64) {
    if n != 0 {
        let hidden = state.hidden();
        let rows = Rows { doc, layout: state.layout, hidden: &hidden };
        let top = rows.row_at(state.scroll_top);
        state.scroll_top =
            if n > 0 { rows.forward(top, n as usize) } else { rows.back(top, n.unsigned_abs() as usize) };
    }
}

fn reveal(state: &mut EditorState, doc: &Document, mode: Reveal, char_w: f32, text_w: f32) {
    let hidden = state.hidden();
    let rows = Rows { doc, layout: state.layout, hidden: &hidden };
    let caret = state.caret();
    let row = rows.row_at(caret);
    let n = state.view_rows;
    let visible =
        row.start >= state.scroll_top && rows.iter_from(state.scroll_top).take(n).any(|r| r.start == row.start);
    if !visible {
        state.scroll_top = match mode {
            Reveal::Center => rows.back(row, n / 2),
            Reveal::Minimal if row.start < state.scroll_top => row.start,
            Reveal::Minimal => rows.back(row, n.saturating_sub(1)),
        };
    }
    if state.layout.wrap.is_none() {
        let x = rows.col_of(&row, caret) as f32 * char_w;
        let margin = 4.0 * char_w;
        if x < state.scroll_x + margin {
            state.scroll_x = (x - margin).max(0.0);
        } else if x > state.scroll_x + text_w - margin {
            state.scroll_x = x - text_w + margin;
        }
    }
}

fn caret_at(off: u64) -> Selection {
    Selection::caret(off)
}

fn handle_event(
    ctx: &Context,
    ev: &Event,
    doc: &mut Document,
    st: &mut EditorState,
    cfg: &EditorConfig,
    out: &mut Out,
) {
    match ev {
        Event::Text(t) if !t.is_empty() => {
            let plan = edit::type_text(&st.sels, t.as_bytes());
            out.result(st.commit(doc, plan));
        }
        Event::Ime(ImeEvent::Commit(t)) => {
            st.preedit.clear();
            if !t.is_empty() {
                let plan = edit::type_text(&st.sels, t.as_bytes());
                out.result(st.commit(doc, plan));
            }
        }
        Event::Ime(ImeEvent::Preedit(t)) => st.preedit = t.clone(),
        Event::Ime(ImeEvent::Disabled) => st.preedit.clear(),
        Event::Copy => {
            copy(ctx, doc, st, out);
        }
        Event::Cut => cut(ctx, doc, st, out),
        Event::Paste(t) => {
            let plan = edit::paste(&st.sels, t, eol_bytes(doc));
            out.result(st.commit(doc, plan));
        }
        Event::Key { key, pressed: true, modifiers, .. } => handle_key(*key, *modifiers, doc, st, cfg, out),
        _ => {}
    }
}

/// Copies all selections (joined by line endings); false if nothing was selected or it was too large.
fn copy(ctx: &Context, doc: &Document, st: &EditorState, out: &mut Out) -> bool {
    let mut ranges: Vec<std::ops::Range<u64>> = st.sels.iter().filter(|s| !s.is_empty()).map(|s| s.range()).collect();
    ranges.sort_by_key(|r| r.start);
    let total: u64 = ranges.iter().map(|r| r.end - r.start).sum();
    if total > MEMORY_LIMIT {
        out.error(format!("Selection too large to copy ({} MiB)", total >> 20));
        false
    } else if ranges.is_empty() {
        false
    } else {
        let eol = String::from_utf8_lossy(eol_bytes(doc)).into_owned();
        let text: Vec<String> = ranges.into_iter().map(|r| doc.read_string_lossy(r)).collect();
        ctx.copy_text(text.join(&eol));
        true
    }
}

fn cut(ctx: &Context, doc: &mut Document, st: &mut EditorState, out: &mut Out) {
    if copy(ctx, doc, st, out) {
        let plan = edit::per_selection(&st.sels, |s| (s.range(), Vec::new()));
        out.result(st.commit(doc, plan));
    }
}

fn menu_action(ctx: &Context, action: MenuAction, doc: &mut Document, st: &mut EditorState, out: &mut Out) {
    match action {
        MenuAction::Cut => cut(ctx, doc, st, out),
        MenuAction::Copy => {
            copy(ctx, doc, st, out);
        }
        MenuAction::Paste => ctx.send_viewport_cmd(ViewportCommand::RequestPaste),
        MenuAction::SelectAll => st.select_all(doc),
        MenuAction::Run(interp) => {
            out.events.push(EditorEvent::Run { interp, code: st.selected_text(doc), in_terminal: false })
        }
    }
}

/// Moves every caret to `f(selection)`; extends selections with Shift.
fn move_carets(st: &mut EditorState, shift: bool, f: impl Fn(&Selection) -> u64) {
    let sels = st
        .sels
        .iter()
        .map(|s| {
            let c = f(s);
            Selection { anchor: if shift { s.anchor } else { c }, caret: c }
        })
        .collect();
    st.set_sels(sels);
    st.reveal = Some(Reveal::Minimal);
}

/// Moves carets `n` rows down (negative: up), keeping their goal columns.
fn vertical(st: &mut EditorState, doc: &Document, n: i64, shift: bool) {
    let hidden = st.hidden();
    let rows = Rows { doc, layout: st.layout, hidden: &hidden };
    let len = doc.len();
    let moved: Vec<(Selection, usize)> = st
        .sels
        .iter()
        .zip(st.goals.iter())
        .map(|(s, g)| {
            let row = rows.row_at(s.caret);
            let goal = g.unwrap_or_else(|| rows.col_of(&row, s.caret));
            let steps = n.unsigned_abs() as usize;
            let (target, done) = (0..steps)
                .try_fold((row, 0usize), |(r, k), _| {
                    let step = if n < 0 { rows.prev(&r) } else { rows.next(&r) };
                    step.map(|x| (x, k + 1)).ok_or((r, k))
                })
                .unwrap_or_else(|stuck| stuck);
            let c = if done == 0 && steps > 0 {
                if n < 0 {
                    0
                } else {
                    len
                }
            } else {
                rows.off_at_col(&target, goal)
            };
            (Selection { anchor: if shift { s.anchor } else { c }, caret: c }, goal)
        })
        .collect();
    let goals: Vec<Option<usize>> = moved.iter().map(|m| Some(m.1)).collect();
    st.set_sels(moved.into_iter().map(|m| m.0).collect());
    if st.goals.len() == goals.len() {
        st.goals = goals;
    }
    st.reveal = Some(Reveal::Minimal);
}

/// Ctrl+Alt+Up/Down: adds a caret one row above the topmost or below the bottommost caret.
fn add_cursor(st: &mut EditorState, doc: &Document, down: bool) {
    let hidden = st.hidden();
    let rows = Rows { doc, layout: st.layout, hidden: &hidden };
    let base = if down { st.sels.iter().map(|s| s.caret).max() } else { st.sels.iter().map(|s| s.caret).min() };
    let added = base.and_then(|c| {
        let row = rows.row_at(c);
        let col = rows.col_of(&row, c);
        let other = if down { rows.next(&row) } else { rows.prev(&row) };
        other.map(|r| rows.off_at_col(&r, col))
    });
    if let Some(off) = added {
        let mut sels = st.sels.clone();
        sels.push(caret_at(off));
        st.set_sels(sels);
        st.reveal = Some(Reveal::Minimal);
    }
}

fn smart_home(rows: &Rows, c: u64) -> u64 {
    let row = rows.row_at(c);
    if row.line_start {
        let head = bytes(rows.doc, row.start..row.end.min(row.start + LONG_LINE));
        let first = row.start + head.iter().take_while(|&&b| b == b' ' || b == b'\t').count() as u64;
        if c == first {
            row.start
        } else {
            first
        }
    } else {
        row.start
    }
}

fn handle_key(key: Key, m: Modifiers, doc: &mut Document, st: &mut EditorState, cfg: &EditorConfig, out: &mut Out) {
    let (cmd, shift, alt) = (m.command, m.shift, m.alt);
    let tab = usize::from(cfg.tab_width.max(1));
    let page = st.view_rows.saturating_sub(1).max(1) as i64;
    match key {
        Key::ArrowLeft | Key::ArrowRight if !alt => {
            let right = key == Key::ArrowRight;
            let d: &Document = doc;
            move_carets(st, shift, |s| match (s.is_empty() || shift, right, cmd) {
                (false, false, _) => s.range().start,
                (false, true, _) => s.range().end,
                (true, false, true) => word_left(d, s.caret),
                (true, false, false) => prev_boundary(d, s.caret),
                (true, true, true) => word_right(d, s.caret),
                (true, true, false) => next_boundary(d, s.caret),
            });
        }
        Key::ArrowUp | Key::ArrowDown if cmd && alt => add_cursor(st, doc, key == Key::ArrowDown),
        Key::ArrowUp | Key::ArrowDown if cmd && shift => {
            let plan = edit::move_lines(doc, &st.sels, key == Key::ArrowUp);
            out.result(st.commit(doc, plan));
        }
        Key::ArrowUp | Key::ArrowDown if cmd => scroll_rows(st, doc, if key == Key::ArrowUp { -1 } else { 1 }),
        Key::ArrowUp | Key::ArrowDown if !alt => vertical(st, doc, if key == Key::ArrowUp { -1 } else { 1 }, shift),
        Key::PageUp | Key::PageDown => {
            let n = if key == Key::PageUp { -page } else { page };
            vertical(st, doc, n, shift);
            scroll_rows(st, doc, n);
        }
        Key::Home | Key::End if cmd => {
            let target = if key == Key::Home { 0 } else { doc.len() };
            move_carets(st, shift, |_| target);
        }
        Key::Home | Key::End => {
            let hidden = st.hidden();
            let rows = Rows { doc, layout: st.layout, hidden: &hidden };
            let home = key == Key::Home;
            move_carets(st, shift, |s| {
                if home {
                    smart_home(&rows, s.caret)
                } else {
                    rows.off_at_col(&rows.row_at(s.caret), usize::MAX)
                }
            });
        }
        Key::Backspace => {
            let plan = edit::backspace(doc, &st.sels, cmd);
            out.result(st.commit(doc, plan));
        }
        Key::Delete if !shift => {
            let plan = edit::delete_forward(doc, &st.sels, cmd);
            out.result(st.commit(doc, plan));
        }
        Key::Enter if !cmd && !alt => {
            let plan = edit::newline(doc, &st.sels, eol_bytes(doc));
            out.result(st.commit(doc, plan));
        }
        Key::Tab if !cmd && !alt => {
            let plan = if shift {
                edit::outdent(doc, &st.sels, tab)
            } else if edit::spans_lines(doc, &st.sels) {
                edit::indent(doc, &st.sels, tab, cfg.insert_spaces)
            } else {
                edit::insert_tab(doc, &st.sels, tab, cfg.insert_spaces)
            };
            out.result(st.commit(doc, plan));
        }
        Key::Escape => {
            st.preedit.clear();
            st.set_sels(vec![st.sels[0]]);
        }
        Key::A if cmd => st.select_all(doc),
        Key::Z if cmd && shift => out.changed |= st.redo(doc),
        Key::Z if cmd => out.changed |= st.undo(doc),
        Key::Y if cmd => out.changed |= st.redo(doc),
        Key::D if cmd && !shift => {
            let plan = edit::duplicate_lines(doc, &st.sels, eol_bytes(doc));
            out.result(st.commit(doc, plan));
        }
        Key::L if cmd && shift => {
            let plan = edit::delete_lines(doc, &st.sels);
            out.result(st.commit(doc, plan));
        }
        _ => {}
    }
}

/// Colors of the chars of one line from its tokens.
struct LineTokens {
    line_start: u64,
    tokens: Vec<Token>,
}

fn control_picture(c: char) -> char {
    match c {
        '\u{7f}' => '\u{2421}',
        c if (c as u32) < 0x20 => char::from_u32(0x2400 + c as u32).unwrap_or('\u{FFFD}'),
        c => c,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_row(
    ui: &Ui,
    doc: &Document,
    row: Row,
    line: Option<u64>,
    tokens: Option<&LineTokens>,
    cfg: &EditorConfig,
    theme: &EditorTheme,
    frame: &Frame,
    clip: Option<(usize, usize)>,
    suffix: Option<&str>,
) -> VisRow {
    let tab = usize::from(cfg.tab_width.max(1));
    let text = bytes(doc, row.start..row.end);
    let mut jb =
        JobBuilder { job: LayoutJob::default(), run: String::new(), color: theme.fg, font: frame.font.clone() };
    let mut map = Vec::new();
    let mut col = 0usize;
    let mut gidx = 0usize;
    let mut first_col: Option<usize> = None;
    let mut tok = 0usize;
    let mut clip_end = row.start;
    let ws = cfg.show_whitespace;
    let mut chars = Chars::new(&text);
    let mut past_clip = false;
    while let Some((i, c, n)) = (!past_clip).then(|| chars.next()).flatten() {
        let w = char_cols(c, col, tab);
        if clip.is_none_or(|(a, b)| col + w > a && col < b) {
            first_col.get_or_insert(col);
            let abs = row.start + i as u64;
            let color = tokens.map_or(theme.fg, |lt| {
                let rel = (abs - lt.line_start) as usize;
                while lt.tokens.get(tok).is_some_and(|t| t.range.end <= rel) {
                    tok += 1;
                }
                lt.tokens.get(tok).filter(|t| t.range.start <= rel).map_or(theme.fg, |t| theme.token_color(t.kind))
            });
            let (shown, color) = match c {
                '\t' if ws => (format!("→{}", " ".repeat(w - 1)), theme.whitespace),
                '\t' => (" ".repeat(w), color),
                ' ' if ws => ("·".to_string(), theme.whitespace),
                c if c.is_control() => (control_picture(c).to_string(), theme.whitespace),
                c => (c.to_string(), color),
            };
            map.push((abs, gidx));
            gidx += shown.chars().count();
            jb.push(&shown, color);
            clip_end = abs + n as u64;
        }
        col += w;
        past_clip = clip.is_some_and(|(_, b)| col >= b);
    }
    let reaches_end = chars.next().is_none();
    map.push(if map.is_empty() { (row.end, 0) } else { (clip_end, gidx) });
    if reaches_end {
        if ws && row.next > row.end {
            jb.push("¶", theme.whitespace);
        }
        if let Some(s) = suffix {
            jb.push(s, theme.fold_marker);
        }
    }
    let x0 = first_col.unwrap_or(col) as f32 * frame.char_w;
    let galley = ui.fonts(|f| f.layout_job(jb.finish()));
    VisRow { row, line, galley, map, x0 }
}

#[allow(clippy::too_many_arguments)]
fn layout_rows(
    ui: &Ui,
    doc: &Document,
    state: &mut EditorState,
    cfg: &EditorConfig,
    theme: &EditorTheme,
    frame: &Frame,
    rows: &[Row],
    hidden: &[std::ops::Range<u64>],
    large: bool,
    text_w: f32,
) -> Vec<VisRow> {
    let highlight = !large && doc.line_count().is_some();
    let view_cols = (text_w / frame.char_w).ceil() as usize;
    let first_col = (state.scroll_x / frame.char_w) as usize;
    let clip_window = (first_col.saturating_sub(CLIP_MARGIN), first_col + view_cols + CLIP_MARGIN);
    let mut line = rows.first().and_then(|r| state.line_of(doc, r.start));
    let mut current: Option<LineTokens> = None;
    let mut next_state: Option<(u64, kladde_syntax::LineState)> = None;
    let mut starved = false;
    let vis: Vec<VisRow> = rows
        .iter()
        .enumerate()
        .map(|(i, &row)| {
            if i > 0 && row.line_start {
                line = if hidden.iter().any(|h| h.end == row.start) {
                    state.line_of(doc, row.start)
                } else {
                    line.map(|l| l + 1)
                };
            }
            let colored = highlight && !row.long;
            let ls = if row.line_start || !colored { row.start } else { near_line_start(doc, row.start) };
            if colored && current.as_ref().is_none_or(|c| c.line_start != ls) {
                current = line.and_then(|n| {
                    let start_state = match next_state {
                        Some((l, s)) if l == n => Some(s),
                        _ => state.line_state(doc, n),
                    };
                    starved |= start_state.is_none();
                    start_state.map(|s| {
                        let end = find_newline(doc, ls, LONG_LINE + 2).map_or(doc.len(), |q| content_end(doc, q, ls));
                        let raw = bytes(doc, ls..end);
                        let (tokens, after) = match std::str::from_utf8(&raw) {
                            Ok(t) => highlight_line(state.lang, s, t),
                            Err(_) => (Vec::new(), highlight_line(state.lang, s, &String::from_utf8_lossy(&raw)).1),
                        };
                        next_state = Some((n + 1, after));
                        LineTokens { line_start: ls, tokens }
                    })
                });
            }
            let tokens = current.as_ref().filter(|c| colored && c.line_start == ls);
            let clip = (state.layout.wrap.is_none() && row.end - row.start > CLIP_BYTES).then_some(clip_window);
            let folded = state.collapsed.iter().any(|c| c.hidden.start == row.next && row.next > row.end);
            let suffix = folded.then_some(" ⋯ ");
            build_row(ui, doc, row, line, tokens, cfg, theme, frame, clip, suffix)
        })
        .collect();
    if starved {
        ui.ctx().request_repaint();
    }
    vis
}

fn mouse(
    ui: &Ui,
    resp: &Response,
    doc: &Document,
    st: &mut EditorState,
    frame: &Frame,
    vis: &[VisRow],
    hidden: &[std::ops::Range<u64>],
) {
    let (pointer, pressed, down, mods, now) = ui.input(|i| {
        (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers, i.time)
    });
    let len = doc.len();
    let tab = st.layout.tab;
    let hit = |p: Pos2| {
        let v = &vis[frame.row_index(p.y, vis.len())];
        let local = p.x - frame.origin_x;
        let off = v.off_at_x(local, len);
        let extra = if off == v.row.end { ((local - v.x_of(off)) / frame.char_w).round().max(0.0) as usize } else { 0 };
        (off, extra)
    };
    match pointer {
        Some(p) if pressed && resp.hovered() => {
            let (off, extra) = hit(p);
            let (t0, p0, n0) = st.clicks;
            let count = if now - t0 < MULTI_CLICK_SECS && p0.distance(p) < 4.0 { n0 % 3 + 1 } else { 1 };
            st.clicks = (now, p, count);
            if mods.alt {
                let anchor_ls = near_line_start(doc, off);
                st.drag = Some(Drag::Box { anchor_ls, anchor_col: line_col(doc, off, tab) + extra });
                st.set_sels(vec![caret_at(off)]);
            } else if mods.command {
                let mut sels = st.sels.clone();
                sels.push(caret_at(off));
                st.set_sels(sels);
                st.drag = None;
            } else if mods.shift {
                let anchor = st.sels[0].anchor;
                st.set_sels(vec![Selection { anchor, caret: off }]);
                st.drag = Some(Drag::Char { anchor });
            } else {
                match count {
                    1 => {
                        st.set_sels(vec![caret_at(off)]);
                        st.drag = Some(Drag::Char { anchor: off });
                    }
                    2 => {
                        let w = word_at(doc, off);
                        st.set_sels(vec![Selection { anchor: w.start, caret: w.end }]);
                        st.drag = Some(Drag::Word { range: w });
                    }
                    _ => {
                        let l = doc.line_start_at(off)..doc.next_line_start(off);
                        st.set_sels(vec![Selection { anchor: l.start, caret: l.end }]);
                        st.drag = Some(Drag::Line { range: l });
                    }
                }
            }
        }
        Some(p) if down && st.drag.is_some() => {
            let (off, extra) = hit(p);
            let sels = match st.drag.clone() {
                Some(Drag::Char { anchor }) => vec![Selection { anchor, caret: off }],
                Some(Drag::Word { range }) if off < range.start => {
                    vec![Selection { anchor: range.end, caret: word_at(doc, off).start }]
                }
                Some(Drag::Word { range }) => {
                    vec![Selection { anchor: range.start, caret: word_at(doc, off).end.max(range.end) }]
                }
                Some(Drag::Line { range }) if off < range.start => {
                    vec![Selection { anchor: range.end, caret: doc.line_start_at(off) }]
                }
                Some(Drag::Line { range }) => {
                    vec![Selection { anchor: range.start, caret: doc.next_line_start(off).max(range.end) }]
                }
                Some(Drag::Box { anchor_ls, anchor_col }) => {
                    let col = line_col(doc, off, tab) + extra;
                    edit::column_selections(doc, anchor_ls, anchor_col, near_line_start(doc, off), col, tab)
                }
                None => st.sels.clone(),
            };
            if !sels.is_empty() {
                st.set_sels(sels);
            }
            let rect = frame.text_rect;
            let over = if p.y < rect.top() {
                -(((rect.top() - p.y) / frame.row_h).ceil() as i64).min(10)
            } else if p.y > rect.bottom() {
                (((p.y - rect.bottom()) / frame.row_h).ceil() as i64).min(10)
            } else {
                0
            };
            if over != 0 {
                let rows = Rows { doc, layout: st.layout, hidden };
                let top = rows.row_at(st.scroll_top);
                st.scroll_top = if over > 0 {
                    rows.forward(top, over as usize)
                } else {
                    rows.back(top, over.unsigned_abs() as usize)
                };
                ui.ctx().request_repaint();
            }
        }
        _ => {}
    }
    if !down {
        st.drag = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn gutter_input(
    ui: &Ui,
    resp: &Response,
    doc: &Document,
    st: &mut EditorState,
    vis: &[VisRow],
    numbers_w: f32,
    mark_w: f32,
    run_w: f32,
    frame: &Frame,
) {
    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
        let v = &vis[frame.row_index(p.y, vis.len())];
        let x = p.x - resp.rect.left();
        let ls = if v.row.line_start { v.row.start } else { doc.line_start_at(v.row.start) };
        if x < numbers_w {
            st.set_sels(vec![Selection { anchor: ls, caret: doc.next_line_start(ls) }]);
        } else if x < numbers_w + mark_w {
            if !st.bookmarks.remove(&ls) {
                st.bookmarks.insert(ls);
            }
        } else if x >= numbers_w + mark_w + run_w {
            if let Some(line) = v.line.filter(|&l| v.row.line_start && st.is_fold_start(l)) {
                st.toggle_fold(doc, line);
                ui.ctx().request_repaint();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_markers(
    ui: &Ui,
    id: Id,
    doc: &Document,
    st: &EditorState,
    vis: &[VisRow],
    left: f32,
    run_w: f32,
    theme: &EditorTheme,
    frame: &Frame,
    out: &mut Out,
) {
    if run_w > 0.0 {
        vis.iter().enumerate().filter(|(_, v)| v.row.line_start).for_each(|(i, v)| {
            let fence = v.line.and_then(|l| st.fence_at_line(l));
            if let Some(f) = fence {
                let y = frame.text_rect.top() + i as f32 * frame.row_h;
                let rect = Rect::from_min_size(pos2(left, y), vec2(run_w, frame.row_h));
                let r = ui.interact(rect, id.with(("run", f.header_line)), Sense::click());
                let r = r.on_hover_text(format!("Run with {}", f.interp.label()));
                let c = rect.center();
                let s = frame.row_h * 0.3;
                let color = if r.hovered() { theme.fg } else { theme.run_button };
                ui.painter().add(Shape::convex_polygon(
                    vec![pos2(c.x - s * 0.8, c.y - s), pos2(c.x + s, c.y), pos2(c.x - s * 0.8, c.y + s)],
                    color,
                    Stroke::NONE,
                ));
                let code = || doc.read_string_lossy(f.content.clone());
                if r.clicked() {
                    out.events.push(EditorEvent::Run { interp: f.interp, code: code(), in_terminal: false });
                }
                r.context_menu(|ui| {
                    if ui.button("Run").clicked() {
                        out.events.push(EditorEvent::Run { interp: f.interp, code: code(), in_terminal: false });
                        ui.close_menu();
                    }
                    if ui.button("Run in terminal").clicked() {
                        out.events.push(EditorEvent::Run { interp: f.interp, code: code(), in_terminal: true });
                        ui.close_menu();
                    }
                    if ui.button("Copy block").clicked() {
                        ui.ctx().copy_text(code());
                        ui.close_menu();
                    }
                });
            }
        });
    }
}

fn context_menu(resp: &Response, doc: &Document, st: &mut EditorState) {
    let has_sel = !st.primary_range().is_empty();
    let writable = !doc.is_read_only();
    let interpreters = &mut st.interpreters;
    let action = &mut st.menu_action;
    resp.context_menu(|ui| {
        let mut pick = |ui: &mut Ui, a: MenuAction| {
            *action = Some(a);
            ui.close_menu();
            ui.ctx().request_repaint();
        };
        if ui.add_enabled(has_sel && writable, egui::Button::new("Cut")).clicked() {
            pick(ui, MenuAction::Cut);
        }
        if ui.add_enabled(has_sel, egui::Button::new("Copy")).clicked() {
            pick(ui, MenuAction::Copy);
        }
        if ui.add_enabled(writable, egui::Button::new("Paste")).clicked() {
            pick(ui, MenuAction::Paste);
        }
        if ui.button("Select all").clicked() {
            pick(ui, MenuAction::SelectAll);
        }
        ui.separator();
        ui.add_enabled_ui(has_sel, |ui| {
            ui.menu_button("Run selection with", |ui| {
                let available = interpreters.get_or_insert_with(|| {
                    Interpreter::all().iter().copied().filter(|i| i.program().is_some()).collect()
                });
                if available.is_empty() {
                    ui.label("No interpreter found");
                }
                available.iter().for_each(|&interp| {
                    if ui.button(interp.label()).clicked() {
                        pick(ui, MenuAction::Run(interp));
                    }
                });
            });
        });
    });
    if resp.secondary_clicked() {
        resp.request_focus();
    }
}

/// Thumb position as a fraction and its size as a fraction of the track.
fn vscroll_metrics(doc: &Document, st: &EditorState) -> (f64, f64, bool) {
    let len = doc.len().max(1);
    match doc.line_count().filter(|_| st.layout.wrap.is_none()) {
        Some(total) => {
            let top = st.line_of(doc, st.scroll_top).unwrap_or(0);
            let span = total.saturating_sub(st.view_rows as u64).max(1);
            (top as f64 / span as f64, st.view_rows as f64 / total.max(1) as f64, true)
        }
        None => (st.scroll_top as f64 / len as f64, st.visible_bytes as f64 / len as f64, false),
    }
}

fn thumb_rect(track: Rect, frac: f64, size: f64) -> Rect {
    let h = ((size.min(1.0) as f32) * track.height()).max(20.0).min(track.height());
    let top = track.top() + (frac.clamp(0.0, 1.0) as f32) * (track.height() - h);
    Rect::from_min_size(pos2(track.left(), top), vec2(track.width(), h))
}

fn vscrollbar_input(ui: &Ui, resp: &Response, bar: Rect, doc: &Document, st: &mut EditorState) {
    let track = bar.shrink(2.0);
    let (frac, size, line_mode) = vscroll_metrics(doc, st);
    let thumb = thumb_rect(track, frac, size);
    let (pointer, pressed, down) =
        ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down()));
    if let Some(p) = pointer.filter(|_| pressed && resp.hovered()) {
        st.vbar_grab = Some(if thumb.contains(p) { p.y - thumb.top() } else { thumb.height() / 2.0 });
    }
    match (st.vbar_grab, pointer) {
        (Some(grab), Some(p)) if down => {
            let free = (track.height() - thumb.height()).max(1.0);
            let f = f64::from(((p.y - grab - track.top()) / free).clamp(0.0f32, 1.0));
            st.scroll_top = if line_mode {
                let total = doc.line_count().unwrap_or(1);
                let span = total.saturating_sub(st.view_rows as u64);
                doc.line_to_offset((f * span as f64).round() as u64).unwrap_or(st.scroll_top)
            } else {
                let hidden = st.hidden();
                let rows = Rows { doc, layout: st.layout, hidden: &hidden };
                rows.row_at((f * doc.len() as f64) as u64).start
            };
        }
        _ => st.vbar_grab = None,
    }
}

fn paint_vscrollbar(ui: &Ui, doc: &Document, st: &EditorState, theme: &EditorTheme, bar: Rect, vis: &[Row]) {
    let painter = ui.painter();
    painter.rect_filled(bar, 0.0, theme.scrollbar);
    let everything = st.scroll_top == 0 && vis.last().is_none_or(|r| r.end >= doc.len());
    if !everything {
        let (frac, size, _) = vscroll_metrics(doc, st);
        painter.rect_filled(thumb_rect(bar.shrink(2.0), frac, size), 3.0, theme.scrollbar_thumb);
    }
}

/// Horizontal scrollbar over the bottom of the text area when rows are wider than the view.
fn hscrollbar(
    ui: &Ui,
    id: Id,
    st: &mut EditorState,
    vis: &[VisRow],
    frame: &Frame,
    text_w: f32,
) -> Option<(Rect, Rect)> {
    let content = vis.iter().map(|v| v.x0 + v.galley.size().x).fold(0.0f32, f32::max) + frame.char_w;
    let max_scroll = (content - text_w).max(0.0);
    st.scroll_x = st.scroll_x.min(max_scroll);
    let shown = st.layout.wrap.is_none() && max_scroll > 1.0;
    shown.then(|| {
        let r = frame.text_rect;
        let bar = Rect::from_min_max(pos2(r.left(), r.bottom() - BAR * 0.7), r.max);
        let free_w = bar.width();
        let w = (free_w * text_w / content).max(20.0).min(free_w);
        let x = bar.left() + (st.scroll_x / max_scroll).clamp(0.0, 1.0) * (free_w - w);
        let thumb = Rect::from_min_size(pos2(x, bar.top() + 1.0), vec2(w, bar.height() - 2.0));
        let resp = ui.interact(bar, id.with("hbar"), Sense::click_and_drag());
        let (pointer, pressed, down) =
            ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down()));
        if let Some(p) = pointer.filter(|_| pressed && resp.hovered()) {
            st.hbar_grab = Some(if thumb.contains(p) { p.x - thumb.left() } else { w / 2.0 });
        }
        match (st.hbar_grab, pointer) {
            (Some(grab), Some(p)) if down => {
                let f = ((p.x - grab - bar.left()) / (free_w - w).max(1.0)).clamp(0.0, 1.0);
                st.scroll_x = f * max_scroll;
                ui.ctx().request_repaint();
            }
            _ => st.hbar_grab = None,
        }
        (bar, thumb)
    })
}

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &Ui,
    doc: &Document,
    st: &EditorState,
    cfg: &EditorConfig,
    theme: &EditorTheme,
    frame: &Frame,
    vis: &[VisRow],
    outer: Rect,
    gutter: Rect,
    numbers_w: f32,
    mark_w: f32,
    run_w: f32,
    fold_w: f32,
    focused: bool,
) {
    let painter = ui.painter_at(outer);
    let text_painter = painter.with_clip_rect(frame.text_rect);
    text_painter.rect_filled(frame.text_rect, 0.0, theme.bg);
    let len = doc.len();
    let caret = st.caret();
    let bracket = match_bracket(doc, caret);
    let (row_h, char_w) = (frame.row_h, frame.char_w);
    let caret_line = vis.iter().find(|v| v.holds_caret(caret, len)).and_then(|v| v.line);
    let mut caret_rect = None;
    vis.iter().enumerate().for_each(|(i, v)| {
        let y = frame.text_rect.top() + i as f32 * row_h;
        let ox = frame.origin_x;
        let x_of = |off: u64| ox + v.x_of(off);
        let span = |a: f32, b: f32| Rect::from_min_max(pos2(a, y), pos2(b, y + row_h));
        if v.holds_caret(caret, len) {
            text_painter.rect_filled(span(frame.text_rect.left(), frame.text_rect.right()), 0.0, theme.current_line);
        }
        let r = v.row;
        let first = st.find_matches.partition_point(|m| m.end <= r.start);
        st.find_matches[first..].iter().take_while(|m| m.start < r.end.max(r.start + 1)).for_each(|m| {
            let (a, b) = (x_of(m.start.max(r.start)), x_of(m.end.min(r.end)));
            text_painter.rect_filled(span(a, b.max(a + 2.0)), 2.0, theme.find_match);
        });
        st.sels.iter().filter(|s| !s.is_empty()).for_each(|s| {
            let sr = s.range();
            if sr.start < r.next.max(r.end + 1) && sr.end > r.start {
                let a = x_of(sr.start.max(r.start));
                let b = if sr.end > r.end && r.next > r.end { x_of(r.end) + char_w * 0.6 } else { x_of(sr.end) };
                text_painter.rect_filled(span(a, b), 0.0, theme.selection);
            }
        });
        if let Some((p, q)) = bracket {
            [p, q].into_iter().filter(|&o| o >= r.start && o < r.end).for_each(|o| {
                text_painter.rect_filled(span(x_of(o), x_of(o + 1)), 2.0, theme.bracket_match);
            });
        }
        text_painter.galley(pos2(ox + v.x0, y), v.galley.clone(), theme.fg);
        st.sels.iter().enumerate().filter(|(_, s)| v.holds_caret(s.caret, len)).for_each(|(k, s)| {
            let x = x_of(s.caret);
            let rect = Rect::from_min_size(pos2(x - 1.0, y), vec2(2.0, row_h));
            if k == 0 {
                caret_rect = Some(rect);
            }
            if focused || k == 0 {
                text_painter.rect_filled(rect, 0.0, theme.caret);
            }
        });
    });
    if let Some(rect) = caret_rect.filter(|_| focused) {
        if !st.preedit.is_empty() {
            let galley = ui.fonts(|f| f.layout_no_wrap(st.preedit.clone(), frame.font.clone(), theme.fg));
            let r = Rect::from_min_size(rect.min, galley.size());
            text_painter.rect_filled(r, 0.0, theme.bg);
            text_painter.galley(r.min, galley, theme.fg);
            text_painter.hline(r.x_range(), r.bottom() - 1.0, Stroke::new(1.0_f32, theme.fg));
        }
        ui.ctx().output_mut(|o| o.ime = Some(egui::output::IMEOutput { rect: frame.text_rect, cursor_rect: rect }));
    }

    painter.rect_filled(gutter, 0.0, theme.gutter_bg);
    vis.iter().enumerate().filter(|(_, v)| v.row.line_start).for_each(|(i, v)| {
        let y = frame.text_rect.top() + i as f32 * row_h;
        let mid = y + row_h / 2.0;
        if let Some(line) = v.line.filter(|_| cfg.show_line_numbers) {
            let color = if Some(line) == caret_line { theme.gutter_fg_active } else { theme.gutter_fg };
            let x = gutter.left() + numbers_w - char_w * 0.5;
            painter.text(pos2(x, mid), Align2::RIGHT_CENTER, (line + 1).to_string(), frame.font.clone(), color);
        }
        if st.bookmarks.contains(&v.row.start) {
            let c = pos2(gutter.left() + numbers_w + mark_w / 2.0, mid);
            painter.circle_filled(c, row_h * 0.28, theme.bookmark);
        }
        let folded = st.collapsed.iter().any(|c| c.header == v.row.start);
        if fold_w > 0.0 && v.line.is_some_and(|l| st.is_fold_start(l)) {
            let c = pos2(gutter.left() + numbers_w + mark_w + run_w + fold_w / 2.0, mid);
            let s = row_h * 0.22;
            let points = if folded {
                vec![pos2(c.x - s * 0.6, c.y - s), pos2(c.x + s * 0.8, c.y), pos2(c.x - s * 0.6, c.y + s)]
            } else {
                vec![pos2(c.x - s, c.y - s * 0.6), pos2(c.x + s, c.y - s * 0.6), pos2(c.x, c.y + s * 0.8)]
            };
            painter.add(Shape::convex_polygon(points, theme.fold_marker, Stroke::NONE));
        }
    });
}
