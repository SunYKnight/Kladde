//! The hex view widget: layout, painting, pointer and keyboard handling.

use egui::{
    pos2, vec2, Align2, Color32, Event, EventFilter, FontId, ImeEvent, Key, Modifiers, Pos2, Rect, Response,
    ScrollArea, Sense, Stroke, Ui, UiBuilder,
};
use kladde_core::Document;

use crate::format::{decode_text, encode_text, format_hex, offset_digits, parse_hex, text_cells, Layout};
use crate::format::{CONTINUATION, NON_PRINTABLE};
use crate::{HexConfig, HexEvent, HexMode, HexOutput, HexState, HexTheme};

const SCROLLBAR_WIDTH: f32 = 12.0;
const MIN_THUMB: f32 = 20.0;
const PAD: f32 = 4.0;
/// Larger selections are not copied; the clipboard would hold hundreds of MB.
const COPY_LIMIT: u64 = 16 << 20;

/// Values fixed for one frame.
struct Frame<'a> {
    cfg: &'a HexConfig,
    theme: &'a HexTheme,
    layout: Layout,
    font: FontId,
    cw: f32,
    row_h: f32,
}

/// Screen geometry of the content area.
struct Geometry {
    layout: Layout,
    cw: f32,
    row_h: f32,
    offset_x: f32,
    hex_x: f32,
    text_x: f32,
    header_y: f32,
    rows_top: f32,
}

/// Pointer position in content coordinates.
struct Hit {
    text: bool,
    row: i64,
    byte: u64,
    digit: u8,
}

impl Geometry {
    fn byte_x(&self, j: u64) -> f32 {
        self.hex_x + self.layout.cell_x(j) as f32 * self.cw
    }

    fn text_cell_x(&self, j: u64) -> f32 {
        self.text_x + j as f32 * self.cw
    }

    fn row_y(&self, i: u64) -> f32 {
        self.rows_top + i as f32 * self.row_h
    }

    fn hit(&self, pos: Pos2) -> Hit {
        let last = self.layout.bytes_per_row - 1;
        let text = pos.x >= self.text_x - self.cw;
        let col = |x0: f32| ((pos.x - x0) / self.cw).floor().max(0.0) as u64;
        let (byte, digit) = if text {
            (col(self.text_x).min(last), 0)
        } else {
            let c = col(self.hex_x);
            let j = (0..=last).rev().find(|&j| self.layout.cell_x(j) <= c).unwrap_or(0);
            (j, (c - self.layout.cell_x(j)).min(u64::from(self.layout.digits) - 1) as u8)
        };
        let row = ((pos.y - self.rows_top) / self.row_h).floor() as i64;
        Hit { text, row, byte, digit }
    }

    /// Document offset and digit under `hit`; positions past the end map to the append position.
    fn offset_of(&self, hit: &Hit, top_row: u64, len: u64) -> (u64, u8) {
        let row = if hit.row < 0 {
            top_row.saturating_sub(hit.row.unsigned_abs())
        } else {
            top_row.saturating_add(hit.row as u64)
        };
        let off = row.saturating_mul(self.layout.bytes_per_row).saturating_add(hit.byte);
        if off >= len {
            (len, 0)
        } else {
            (off, hit.digit)
        }
    }
}

/// Shows the hex view of `doc` filling the available space.
pub fn show(
    ui: &mut Ui,
    id: egui::Id,
    doc: &mut Document,
    state: &mut HexState,
    cfg: &HexConfig,
    theme: &HexTheme,
) -> HexOutput {
    let revision = doc.revision();
    let font = FontId::monospace(cfg.font_size);
    let (cw, row_h) = ui.fonts(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let frame = Frame { cfg, theme, layout: Layout::new(cfg), font, cw, row_h };
    let bpr = frame.layout.bytes_per_row;
    state.clamp(doc.len(), &frame.layout);

    let (outer, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    ui.painter().rect_filled(outer, 0.0, theme.background);
    let bar = Rect::from_min_max(pos2(outer.right() - SCROLLBAR_WIDTH, outer.top()), outer.max);
    let view = Rect::from_min_max(outer.min, pos2(bar.left(), outer.bottom()));

    let mut top_row = state.top / bpr;
    if ui.rect_contains_pointer(outer) {
        state.wheel += ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta.y));
        let rows = (state.wheel / row_h).trunc();
        state.wheel -= rows * row_h;
        top_row = scroll_by(top_row, -(rows as i64));
    }
    top_row = scrollbar_input(ui, id.with("vscroll"), bar, state, top_row, doc.len() / bpr + 1);
    state.top = top_row * bpr;

    let mut events = Vec::new();
    let mut child = ui.new_child(UiBuilder::new().max_rect(view));
    let focused = ScrollArea::horizontal()
        .id_salt(id.with("hscroll"))
        .auto_shrink(false)
        .drag_to_scroll(false)
        .show(&mut child, |ui| content(ui, id, doc, state, &frame, &mut events))
        .inner;

    paint_scrollbar(ui, bar, theme, state.top / bpr, doc.len() / bpr + 1, state.page_rows);
    if let Some(e) = doc.take_error() {
        report(&mut events, e.to_string());
    }
    HexOutput { events, changed: doc.revision() != revision, focused }
}

/// Moves `row` by `delta` rows without wrapping.
fn scroll_by(row: u64, delta: i64) -> u64 {
    if delta < 0 {
        row.saturating_sub(delta.unsigned_abs())
    } else {
        row.saturating_add(delta as u64)
    }
}

/// Adds an error unless the same message is already queued this frame.
fn report(events: &mut Vec<HexEvent>, msg: String) {
    let event = HexEvent::Error(msg);
    if !events.contains(&event) {
        events.push(event);
    }
}

fn thumb_rect(bar: Rect, top_row: u64, total_rows: u64, page_rows: u64) -> Rect {
    let max_top = total_rows.saturating_sub(page_rows);
    let height = f64::from(bar.height());
    let thumb = (height * page_rows as f64 / total_rows.max(1) as f64).clamp(f64::from(MIN_THUMB).min(height), height);
    let frac = if max_top == 0 { 0.0 } else { top_row.min(max_top) as f64 / max_top as f64 };
    let y = bar.top() + ((height - thumb) * frac) as f32;
    Rect::from_min_size(pos2(bar.left(), y), vec2(bar.width(), thumb as f32))
}

/// Dragging or clicking the scrollbar; returns the new top row.
fn scrollbar_input(ui: &Ui, id: egui::Id, bar: Rect, state: &mut HexState, top_row: u64, total_rows: u64) -> u64 {
    let response = ui.interact(bar, id, Sense::click_and_drag());
    let page = state.page_rows.max(1);
    let thumb = thumb_rect(bar, top_row, total_rows, page);
    let (pos, pressed) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed()));
    match pos.filter(|_| response.is_pointer_button_down_on()) {
        Some(p) => {
            let grab = match state.thumb_grab {
                Some(g) if !pressed => g,
                _ if thumb.contains(p) => p.y - thumb.top(),
                _ => thumb.height() / 2.0,
            };
            state.thumb_grab = Some(grab);
            let free = (bar.height() - thumb.height()).max(1.0);
            let frac = f64::from(((p.y - grab - bar.top()) / free).clamp(0.0, 1.0));
            (frac * total_rows.saturating_sub(page) as f64).round() as u64
        }
        None => {
            state.thumb_grab = None;
            top_row
        }
    }
}

fn paint_scrollbar(ui: &Ui, bar: Rect, theme: &HexTheme, top_row: u64, total_rows: u64, page_rows: u64) {
    let painter = ui.painter();
    painter.rect_filled(bar, 0.0, theme.scrollbar_track);
    if total_rows > page_rows {
        painter.rect_filled(thumb_rect(bar, top_row, total_rows, page_rows).shrink(2.0), 3.0, theme.scrollbar_thumb);
    }
}

/// Rows, input and painting inside the horizontal scroll area; returns whether the view has focus.
fn content(
    ui: &mut Ui,
    id: egui::Id,
    doc: &mut Document,
    state: &mut HexState,
    f: &Frame,
    events: &mut Vec<HexEvent>,
) -> bool {
    let layout = f.layout;
    let bpr = layout.bytes_per_row;
    let digits = offset_digits(doc.len());
    let chars = digits as u64 + 2 + layout.hex_chars() + 2 + bpr;
    let avail = ui.available_size();
    let width = (2.0 * PAD + chars as f32 * f.cw).max(avail.x);
    let (rect, _) = ui.allocate_exact_size(vec2(width, avail.y), Sense::hover());
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let page_rows = ((rect.height() - f.row_h) / f.row_h).floor().max(1.0) as u64;
    state.page_rows = page_rows;
    let offset_x = rect.left() + PAD;
    let hex_x = offset_x + (digits + 2) as f32 * f.cw;
    let geo = Geometry {
        layout,
        cw: f.cw,
        row_h: f.row_h,
        offset_x,
        hex_x,
        text_x: hex_x + (layout.hex_chars() + 2) as f32 * f.cw,
        header_y: rect.top(),
        rows_top: rect.top() + f.row_h,
    };

    pointer_input(ui, &response, &geo, rect, state, doc.len());
    let focused = response.has_focus();
    let moved = focused && keyboard_input(ui, id, doc, state, f, events);

    let len = doc.len();
    let mut top_row = state.top / bpr;
    let cursor_row = state.cursor / bpr;
    match state.reveal.take() {
        Some(off) if off / bpr < top_row || off / bpr >= top_row + page_rows => {
            top_row = (off / bpr).saturating_sub(page_rows / 3);
        }
        _ if moved && cursor_row < top_row => top_row = cursor_row,
        _ if moved && cursor_row >= top_row + page_rows => top_row = cursor_row + 1 - page_rows,
        _ => {}
    }
    top_row = top_row.min((len / bpr + 1).saturating_sub(page_rows));
    state.top = top_row * bpr;

    if focused && state.text_active {
        let caret = Rect::from_min_size(pos2(geo.text_cell_x(state.cursor % bpr), geo.rows_top), vec2(f.cw, f.row_h));
        ui.ctx().output_mut(|o| o.ime = Some(egui::output::IMEOutput { rect, cursor_rect: caret }));
    }
    paint(ui, doc, state, f, &geo, rect, focused);
    focused
}

fn pointer_input(ui: &Ui, response: &Response, geo: &Geometry, rect: Rect, state: &mut HexState, len: u64) {
    let bpr = geo.layout.bytes_per_row;
    let top_row = state.top / bpr;
    let (pos, pressed, down, shift) = ui.input(|i| {
        (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.shift)
    });
    match pos {
        Some(p) if pressed && response.is_pointer_button_down_on() => {
            response.request_focus();
            let hit = geo.hit(p);
            let (off, digit) = geo.offset_of(&hit, top_row, len);
            state.text_active = hit.text;
            if shift {
                state.cursor = if off >= state.anchor { (off + 1).min(len) } else { off };
                state.digit = 0;
                state.drag_origin = None;
            } else {
                state.move_to(off, len, false);
                state.digit = if hit.text { 0 } else { digit };
                state.drag_origin = Some(off);
            }
        }
        Some(p) if down && state.drag_origin.is_some() => {
            let origin = state.drag_origin.expect("matched is_some");
            let (off, _) = geo.offset_of(&geo.hit(p), top_row, len);
            if off != origin || !state.selection().is_empty() {
                state.drag_to(origin, off, len);
            }
            let delta = i64::from(p.y > rect.bottom()) - i64::from(p.y < geo.rows_top);
            if delta != 0 {
                state.top = scroll_by(top_row, delta) * bpr;
                ui.ctx().request_repaint();
            }
        }
        _ if !down => state.drag_origin = None,
        _ => {}
    }
}

/// Applies this frame's keyboard and clipboard events; returns whether the caret should be kept in view.
fn keyboard_input(
    ui: &Ui,
    id: egui::Id,
    doc: &mut Document,
    state: &mut HexState,
    f: &Frame,
    events: &mut Vec<HexEvent>,
) -> bool {
    let filter = EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false };
    ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
    let input = ui.input(|i| i.events.clone());
    input.into_iter().fold(false, |moved, event| {
        let result = match event {
            Event::Text(text) | Event::Ime(ImeEvent::Commit(text)) => type_text(doc, state, f, &text),
            Event::Copy => copy(ui, doc, state, f.cfg).map(|_| false),
            Event::Cut => copy(ui, doc, state, f.cfg).and_then(|_| cut(doc, state)),
            Event::Paste(text) => paste(doc, state, f.cfg, &text),
            Event::Key { key, pressed: true, modifiers, .. } => {
                key_press(doc, state, &f.layout, key, modifiers).map_err(|e| e.to_string())
            }
            _ => Ok(false),
        };
        match result {
            Ok(m) => moved || m,
            Err(msg) => {
                report(events, msg);
                moved
            }
        }
    })
}

fn type_text(doc: &mut Document, state: &mut HexState, f: &Frame, text: &str) -> Result<bool, String> {
    if state.text_active {
        let bytes = encode_text(text, f.cfg.text)?;
        state.write_bytes(doc, &bytes).map_err(|e| e.to_string())?;
    } else {
        let radix = match f.cfg.mode {
            HexMode::Hex => 16,
            HexMode::Binary => 2,
        };
        text.chars()
            .filter_map(|c| c.to_digit(radix))
            .try_for_each(|v| state.type_digit(doc, v as u8, &f.layout))
            .map_err(|e| e.to_string())?;
    }
    Ok(true)
}

fn copy(ui: &Ui, doc: &Document, state: &HexState, cfg: &HexConfig) -> Result<(), String> {
    let sel = state.selection();
    if sel.end - sel.start > COPY_LIMIT {
        Err(format!("selection too large to copy (max {} MiB)", COPY_LIMIT >> 20))
    } else {
        if !sel.is_empty() {
            let mut bytes = Vec::new();
            doc.read(sel, &mut bytes);
            let text = if state.text_active { decode_text(&bytes, cfg.text) } else { format_hex(&bytes) };
            ui.ctx().copy_text(text);
        }
        Ok(())
    }
}

/// Cut removes bytes only in insert mode; overwrite mode keeps the size, so it only copies.
fn cut(doc: &mut Document, state: &mut HexState) -> Result<bool, String> {
    let sel = state.selection();
    if state.insert && !sel.is_empty() {
        state.remove(doc, sel).map_err(|e| e.to_string())?;
    }
    Ok(state.insert)
}

fn paste(doc: &mut Document, state: &mut HexState, cfg: &HexConfig, text: &str) -> Result<bool, String> {
    let bytes = if state.text_active { encode_text(text, cfg.text)? } else { parse_hex(text)? };
    if !bytes.is_empty() {
        state.write_bytes(doc, &bytes).map_err(|e| e.to_string())?;
    }
    Ok(true)
}

/// Navigation and editing keys; returns whether the key was handled.
fn key_press(
    doc: &mut Document,
    state: &mut HexState,
    layout: &Layout,
    key: Key,
    m: Modifiers,
) -> Result<bool, kladde_core::CoreError> {
    let len = doc.len();
    let bpr = layout.bytes_per_row;
    let cur = state.cursor;
    let page = state.page_rows.max(1) * bpr;
    let digit_step = !m.shift && !m.command && !state.text_active;
    let handled = match key {
        Key::ArrowLeft if digit_step && state.digit > 0 => {
            state.digit -= 1;
            true
        }
        Key::ArrowLeft if digit_step && cur > 0 => {
            state.move_to(cur - 1, len, false);
            state.digit = layout.digits - 1;
            true
        }
        Key::ArrowRight if digit_step && state.digit + 1 < layout.digits && cur < len => {
            state.digit += 1;
            true
        }
        Key::ArrowLeft => move_caret(state, cur.saturating_sub(1), len, m),
        Key::ArrowRight => move_caret(state, cur.saturating_add(1), len, m),
        Key::ArrowUp => move_caret(state, cur.saturating_sub(bpr), len, m),
        Key::ArrowDown => move_caret(state, cur.saturating_add(bpr), len, m),
        Key::PageUp => {
            state.top = state.top.saturating_sub(page);
            move_caret(state, cur.saturating_sub(page), len, m)
        }
        Key::PageDown => {
            state.top = state.top.saturating_add(page);
            move_caret(state, cur.saturating_add(page), len, m)
        }
        Key::Home if m.command => move_caret(state, 0, len, m),
        Key::End if m.command => move_caret(state, len, len, m),
        Key::Home => move_caret(state, cur - cur % bpr, len, m),
        Key::End => move_caret(state, cur - cur % bpr + bpr - 1, len, m),
        Key::Tab => {
            state.text_active = !state.text_active;
            state.digit = 0;
            true
        }
        Key::Insert if m.is_none() => {
            state.insert = !state.insert;
            true
        }
        Key::Backspace => state.backspace(doc).map(|_| true)?,
        Key::Delete => state.delete_forward(doc).map(|_| true)?,
        Key::Z if m.command && m.shift => redo(doc, state),
        Key::Z if m.command => {
            if let Some(hint) = doc.undo() {
                state.move_to(hint, doc.len(), false);
            }
            true
        }
        Key::Y if m.command => redo(doc, state),
        Key::A if m.command => {
            state.anchor = 0;
            state.cursor = len;
            state.digit = 0;
            true
        }
        _ => false,
    };
    Ok(handled)
}

fn move_caret(state: &mut HexState, off: u64, len: u64, m: Modifiers) -> bool {
    state.move_to(off, len, m.shift);
    true
}

fn redo(doc: &mut Document, state: &mut HexState) -> bool {
    if let Some(hint) = doc.redo() {
        state.move_to(hint, doc.len(), false);
    }
    true
}

fn paint(ui: &Ui, doc: &Document, state: &HexState, f: &Frame, geo: &Geometry, rect: Rect, focused: bool) {
    let painter = ui.painter_at(rect);
    let layout = &f.layout;
    let bpr = layout.bytes_per_row;
    let len = doc.len();
    let first = state.top;
    let rows = state.page_rows + 1;

    // 3 extra bytes on each side let UTF-8 sequences crossing the visible edges decode.
    let read_start = first.saturating_sub(3);
    let mut data = Vec::new();
    doc.read(read_start..first.saturating_add(rows * bpr).saturating_add(3), &mut data);
    let lead = (first - read_start) as usize;

    (0..bpr).for_each(|j| {
        let label = format_hex(&[(j & 0xFF) as u8]);
        painter.text(pos2(geo.byte_x(j), geo.header_y), Align2::LEFT_TOP, label, f.font.clone(), f.theme.header);
    });
    let rule_y = geo.rows_top - 1.0;
    painter.line_segment(
        [pos2(rect.left(), rule_y), pos2(rect.right(), rule_y)],
        Stroke::new(1.0_f32, f.theme.cursor_row),
    );

    let sel = state.selection();
    (0..rows).map(|i| (i, first + i * bpr)).filter(|&(_, off)| off <= len).for_each(|(i, row_off)| {
        let y = geo.row_y(i);
        let row_len = (len - row_off).min(bpr);
        let start = lead + (row_off - first) as usize;
        let bytes = &data[start..start + row_len as usize];
        let row_rect = |x0: f32, x1: f32| Rect::from_min_max(pos2(x0, y), pos2(x1, y + geo.row_h));

        if state.cursor / bpr == row_off / bpr {
            painter.rect_filled(row_rect(rect.left(), rect.right()), 0.0, f.theme.cursor_row);
        }
        let s = sel.start.max(row_off);
        let e = sel.end.min(row_off + row_len);
        if s < e {
            let (j0, j1) = (s - row_off, e - 1 - row_off);
            let hex_end = geo.byte_x(j1) + f32::from(layout.digits) * geo.cw;
            painter.rect_filled(row_rect(geo.byte_x(j0), hex_end), 0.0, f.theme.selection);
            painter.rect_filled(row_rect(geo.text_cell_x(j0), geo.text_cell_x(j1 + 1)), 0.0, f.theme.selection);
        }

        let offset = format!("{row_off:0width$X}", width = offset_digits(len));
        painter.text(pos2(geo.offset_x, y), Align2::LEFT_TOP, offset, f.font.clone(), f.theme.offset);

        // Each byte at its own cell: a single text run advances by rounded glyph widths and drifts from the
        // header, selection and hit-testing at fractional scale factors.
        bytes.iter().enumerate().for_each(|(j, &b)| {
            let color = if b == 0 { f.theme.zero_byte } else { f.theme.byte };
            let pos = pos2(geo.byte_x(j as u64), y);
            painter.text(pos, Align2::LEFT_TOP, layout.byte_text(b), f.font.clone(), color);
        });

        let cells = text_cells(&data, start..start + row_len as usize, f.cfg.text);
        cells.iter().zip(bytes).enumerate().filter(|(_, (&c, _))| c != CONTINUATION).for_each(|(j, (&c, &b))| {
            let color = if c == NON_PRINTABLE && b != b'.' { f.theme.non_printable } else { f.theme.text };
            let pos = pos2(geo.text_cell_x(j as u64), y);
            painter.text(pos, Align2::LEFT_TOP, c, f.font.clone(), color);
        });

        if state.cursor / bpr == row_off / bpr {
            paint_caret(&painter, state, f, geo, y, focused);
        }
    });
}

fn paint_caret(painter: &egui::Painter, state: &HexState, f: &Frame, geo: &Geometry, y: f32, focused: bool) {
    let j = state.cursor % f.layout.bytes_per_row;
    let active = if focused { f.theme.caret } else { f.theme.caret_inactive };
    let (hex_color, text_color) =
        if state.text_active { (f.theme.caret_inactive, active) } else { (active, f.theme.caret_inactive) };
    let cell = |x: f32, chars: f32| Rect::from_min_size(pos2(x, y), vec2(chars * geo.cw, geo.row_h));
    let bar = |x: f32, color: Color32| {
        painter.line_segment([pos2(x - 1.0, y), pos2(x - 1.0, y + geo.row_h)], Stroke::new(2.0_f32, color))
    };
    let byte_cell = cell(geo.byte_x(j), f32::from(f.layout.digits));
    let text_cell = cell(geo.text_cell_x(j), 1.0);
    let insert_bar = state.insert && state.digit == 0;
    match (state.text_active, insert_bar) {
        (false, true) => bar(byte_cell.left(), hex_color),
        (false, false) => {
            let digit = cell(byte_cell.left() + f32::from(state.digit) * geo.cw, 1.0);
            painter.rect_stroke(digit, 0.0, Stroke::new(1.5_f32, hex_color))
        }
        (true, _) => painter.rect_stroke(byte_cell, 0.0, Stroke::new(1.0_f32, hex_color)),
    };
    match (state.text_active, state.insert) {
        (true, true) => bar(text_cell.left(), text_color),
        (true, false) => painter.rect_stroke(text_cell, 0.0, Stroke::new(1.5_f32, text_color)),
        (false, _) => painter.rect_stroke(text_cell, 0.0, Stroke::new(1.0_f32, text_color)),
    };
}
