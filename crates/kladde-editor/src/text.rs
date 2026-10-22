//! Byte-level helpers over a `Document`: bounded scans, UTF-8 decoding, char and word boundaries, brackets.

use std::ops::Range;

use kladde_core::Document;

/// Lines with more content bytes than this are split into segments.
pub(crate) const LONG_LINE: u64 = 64 << 10;
/// Target size of a long-line segment.
pub(crate) const SEGMENT: u64 = 4 << 10;
/// Word motions and double-click look at most this far.
const WORD_WINDOW: u64 = 4 << 10;
/// Bracket matching looks at most this far in either direction.
const BRACKET_WINDOW: u64 = 64 << 10;
const SCAN_CHUNK: u64 = 4 << 10;

pub(crate) fn bytes(doc: &Document, range: Range<u64>) -> Vec<u8> {
    let mut out = Vec::new();
    doc.read(range, &mut out);
    out
}

/// Last '\n' in `[off - limit, off)`.
pub(crate) fn rfind_newline(doc: &Document, off: u64, limit: u64) -> Option<u64> {
    let floor = off.saturating_sub(limit);
    let mut end = off.min(doc.len());
    let mut found = None;
    let mut buf = Vec::new();
    while found.is_none() && end > floor {
        let start = end.saturating_sub(SCAN_CHUNK).max(floor);
        buf.clear();
        doc.read(start..end, &mut buf);
        found = buf.iter().rposition(|&b| b == b'\n').map(|p| start + p as u64);
        end = start;
    }
    found
}

/// First '\n' in `[off, off + limit)`.
pub(crate) fn find_newline(doc: &Document, off: u64, limit: u64) -> Option<u64> {
    let ceil = off.saturating_add(limit).min(doc.len());
    let mut start = off;
    let mut found = None;
    let mut buf = Vec::new();
    while found.is_none() && start < ceil {
        let end = start.saturating_add(SCAN_CHUNK).min(ceil);
        buf.clear();
        doc.read(start..end, &mut buf);
        found = buf.iter().position(|&b| b == b'\n').map(|p| start + p as u64);
        start = end;
    }
    found
}

/// Content end of a line whose '\n' is at `nl`: before a preceding '\r' unless that is before `floor`.
pub(crate) fn content_end(doc: &Document, nl: u64, floor: u64) -> u64 {
    if nl > floor && doc.byte_at(nl - 1) == Some(b'\r') {
        nl - 1
    } else {
        nl
    }
}

pub(crate) fn is_continuation(b: u8) -> bool {
    (0x80..0xC0).contains(&b)
}

/// Decodes the char at the start of `b` (non-empty); invalid bytes decode as U+FFFD of length 1.
pub(crate) fn decode(b: &[u8]) -> (char, usize) {
    if b[0] < 0x80 {
        (char::from(b[0]), 1)
    } else {
        decode_multibyte(b)
    }
}

fn decode_multibyte(b: &[u8]) -> (char, usize) {
    let n = match b[0] {
        0..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    };
    let valid = (n > 0 && b.len() >= n).then(|| std::str::from_utf8(&b[..n]).ok()).flatten();
    valid.and_then(|s| s.chars().next()).map_or(('\u{FFFD}', 1), |c| (c, n))
}

/// Iterates `(byte index, char, byte length)` over possibly invalid UTF-8.
pub(crate) struct Chars<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Chars<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Chars<'a> {
        Chars { bytes, pos: 0 }
    }
}

impl Iterator for Chars<'_> {
    type Item = (usize, char, usize);

    fn next(&mut self) -> Option<Self::Item> {
        (self.pos < self.bytes.len()).then(|| {
            let (c, n) = decode(&self.bytes[self.pos..]);
            let item = (self.pos, c, n);
            self.pos += n;
            item
        })
    }
}

/// Display width of `c` at visual column `col`.
pub(crate) fn char_cols(c: char, col: usize, tab: usize) -> usize {
    if c == '\t' {
        tab - col % tab
    } else {
        1
    }
}

/// Visual column after `text` starting at column 0 (tabs expanded).
pub(crate) fn text_cols(text: &[u8], tab: usize) -> usize {
    Chars::new(text).fold(0, |col, (_, c, _)| col + char_cols(c, col, tab))
}

/// Byte index in `text` nearest to visual column `goal`, plus the column reached there.
pub(crate) fn index_at_col(text: &[u8], goal: usize, tab: usize) -> (usize, usize) {
    let mut col = 0;
    let mut found = None;
    let mut it = Chars::new(text);
    while let (None, Some((i, c, n))) = (found, it.next()) {
        let w = char_cols(c, col, tab);
        if col + w > goal {
            // Inside a wide char (tab): pick the nearer edge.
            found = Some(if goal - col > w / 2 { (i + n, col + w) } else { (i, col) });
        }
        col += w;
    }
    found.unwrap_or((text.len(), col))
}

/// Moves `off` back to the start of the char containing it; never splits CRLF.
pub(crate) fn snap(doc: &Document, off: u64) -> u64 {
    let off = off.min(doc.len());
    let start = off.saturating_sub(3);
    let window = bytes(doc, start..off + 4);
    let at = (off - start) as usize;
    let char_start = match window.get(at) {
        Some(&b) if is_continuation(b) => (0..at)
            .rev()
            .find(|&i| !is_continuation(window[i]))
            .filter(|&i| decode(&window[i..]).1 > at - i)
            .map_or(off, |i| start + i as u64),
        Some(b'\n') if at > 0 && window[at - 1] == b'\r' => off - 1,
        _ => off,
    };
    char_start
}

/// Offset after the char (or CRLF) at `off`.
pub(crate) fn next_boundary(doc: &Document, off: u64) -> u64 {
    let len = doc.len();
    if off >= len {
        len
    } else {
        let window = bytes(doc, off..off + 4);
        if window.starts_with(b"\r\n") {
            off + 2
        } else {
            off + decode(&window).1 as u64
        }
    }
}

/// Offset of the char (or CRLF) ending at `off`.
pub(crate) fn prev_boundary(doc: &Document, off: u64) -> u64 {
    let off = off.min(doc.len());
    if off == 0 {
        0
    } else {
        let start = off.saturating_sub(4);
        let window = bytes(doc, start..off);
        let n = window.len();
        if window.ends_with(b"\r\n") {
            off - 2
        } else {
            let lead = (0..n).rev().find(|&i| !is_continuation(window[i]));
            lead.filter(|&i| decode(&window[i..]).1 == n - i).map_or(off - 1, |i| start + i as u64)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Word,
    Space,
    Newline,
    Punct,
}

pub(crate) fn class(c: char) -> Class {
    match c {
        '\n' | '\r' => Class::Newline,
        ' ' | '\t' => Class::Space,
        c if c.is_alphanumeric() || c == '_' => Class::Word,
        c if c.is_whitespace() => Class::Space,
        _ => Class::Punct,
    }
}

/// Ctrl+Right: past the current run of word/punctuation chars and following blanks, or over a line break.
pub(crate) fn word_right(doc: &Document, off: u64) -> u64 {
    let len = doc.len();
    if off >= len {
        len
    } else {
        let window = bytes(doc, off..off + WORD_WINDOW);
        let items: Vec<(usize, char, usize)> = Chars::new(&window).collect();
        let first = class(items[0].1);
        let pos = if first == Class::Newline {
            next_boundary(doc, off) - off
        } else {
            let run = items.iter().position(|x| class(x.1) != first).unwrap_or(items.len());
            let end = if first == Class::Space {
                run
            } else {
                run + items[run..].iter().position(|x| class(x.1) != Class::Space).unwrap_or(items.len() - run)
            };
            items.get(end).map_or(window.len(), |x| x.0) as u64
        };
        snap(doc, off + pos)
    }
}

/// Ctrl+Left: back over blanks, then over the run of word/punctuation chars, or over a line break.
pub(crate) fn word_left(doc: &Document, off: u64) -> u64 {
    let off = off.min(doc.len());
    if off == 0 {
        0
    } else {
        let start = off.saturating_sub(WORD_WINDOW);
        let window = bytes(doc, start..off);
        let items: Vec<(usize, char, usize)> = Chars::new(&window).collect();
        let last = class(items[items.len() - 1].1);
        if last == Class::Newline {
            prev_boundary(doc, off)
        } else {
            let blanks = items.iter().rposition(|x| class(x.1) != Class::Space).map_or(0, |p| p + 1);
            let stop = match blanks.checked_sub(1).map(|i| class(items[i].1)) {
                None | Some(Class::Newline) => blanks,
                Some(c) => items[..blanks].iter().rposition(|x| class(x.1) != c).map_or(0, |p| p + 1),
            };
            let pos = items.get(stop).map_or(window.len(), |x| x.0) as u64;
            snap(doc, start + pos)
        }
    }
}

/// Double-click: the run of same-class chars at `off` (or before it at a line end).
pub(crate) fn word_at(doc: &Document, off: u64) -> Range<u64> {
    let off = snap(doc, off);
    let start = off.saturating_sub(WORD_WINDOW);
    let left = bytes(doc, start..off);
    let right = bytes(doc, off..off + WORD_WINDOW);
    let left_items: Vec<(usize, char, usize)> = Chars::new(&left).collect();
    let right_items: Vec<(usize, char, usize)> = Chars::new(&right).collect();
    let here = right_items.first().map(|x| class(x.1)).filter(|&c| c != Class::Newline);
    let target = here.or_else(|| left_items.last().map(|x| class(x.1)).filter(|&c| c != Class::Newline));
    match target {
        None => off..off,
        Some(c) => {
            let l = left_items.iter().rposition(|x| class(x.1) != c).map_or(0, |p| p + 1);
            let r = right_items.iter().position(|x| class(x.1) != c).unwrap_or(right_items.len());
            let a = start + left_items.get(l).map_or(left.len(), |x| x.0) as u64;
            let b = off + right_items.get(r).map_or(right.len(), |x| x.0) as u64;
            snap(doc, a)..snap(doc, b)
        }
    }
}

fn bracket_pair(b: u8) -> Option<(u8, u8, bool)> {
    match b {
        b'(' => Some((b'(', b')', true)),
        b'[' => Some((b'[', b']', true)),
        b'{' => Some((b'{', b'}', true)),
        b')' => Some((b'(', b')', false)),
        b']' => Some((b'[', b']', false)),
        b'}' => Some((b'{', b'}', false)),
        _ => None,
    }
}

/// Bracket at `pos` and its partner, searching at most 64 KiB away.
fn match_at(doc: &Document, pos: u64) -> Option<(u64, u64)> {
    let (open, close, forward) = doc.byte_at(pos).and_then(bracket_pair)?;
    let found = if forward {
        let window = bytes(doc, pos..pos + BRACKET_WINDOW);
        let mut depth = 0i64;
        window
            .iter()
            .position(|&b| {
                depth += i64::from(b == open) - i64::from(b == close);
                depth == 0
            })
            .map(|i| pos + i as u64)
    } else {
        let start = pos.saturating_sub(BRACKET_WINDOW);
        let window = bytes(doc, start..pos + 1);
        let mut depth = 0i64;
        window
            .iter()
            .rposition(|&b| {
                depth += i64::from(b == close) - i64::from(b == open);
                depth == 0
            })
            .map(|i| start + i as u64)
    };
    found.map(|other| (pos, other))
}

/// Bracket pair touching the caret: the char after it first, then the char before it.
pub(crate) fn match_bracket(doc: &Document, caret: u64) -> Option<(u64, u64)> {
    match_at(doc, caret).or_else(|| caret.checked_sub(1).and_then(|p| match_at(doc, p)))
}

/// Line start of `off` if it is within 64 KiB, else the start of the bounded window.
pub(crate) fn near_line_start(doc: &Document, off: u64) -> u64 {
    rfind_newline(doc, off, LONG_LINE).map_or(off.saturating_sub(LONG_LINE), |p| p + 1)
}

/// Visual column of `off` counted from its line start (bounded to 64 KiB before `off`).
pub(crate) fn line_col(doc: &Document, off: u64, tab: usize) -> usize {
    let ls = near_line_start(doc, off);
    text_cols(&bytes(doc, ls..off), tab)
}

/// Offset at visual column `col` of the line starting at `ls` (clamped to the line end).
pub(crate) fn line_offset_at_col(doc: &Document, ls: u64, col: usize, tab: usize) -> u64 {
    let end =
        find_newline(doc, ls, LONG_LINE + 2).map_or_else(|| doc.len().min(ls + LONG_LINE), |q| content_end(doc, q, ls));
    let text = bytes(doc, ls..end);
    ls + index_at_col(&text, col, tab).0 as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::from_bytes(s.as_bytes().to_vec())
    }

    #[test]
    fn word_right_stops_at_word_starts_and_line_breaks() {
        let d = doc("foo bar_baz  (x)\r\nnext");
        let stops: Vec<u64> =
            std::iter::successors(Some(0u64), |&o| (o < d.len()).then(|| word_right(&d, o))).collect();
        assert_eq!(stops, vec![0, 4, 13, 14, 15, 16, 18, 22]);
    }

    #[test]
    fn word_left_mirrors_word_right() {
        let d = doc("foo bar_baz  (x)\r\nnext");
        let stops: Vec<u64> = std::iter::successors(Some(d.len()), |&o| (o > 0).then(|| word_left(&d, o))).collect();
        assert_eq!(stops, vec![22, 18, 16, 15, 14, 13, 4, 0]);
    }

    #[test]
    fn word_motion_steps_over_multibyte_chars_whole() {
        let d = doc("größe übel");
        assert_eq!(word_right(&d, 0), "größe ".len() as u64);
        assert_eq!(word_left(&d, d.len()), "größe ".len() as u64);
        assert_eq!(word_at(&d, 2), 0.."größe".len() as u64);
    }

    #[test]
    fn char_boundaries_keep_crlf_and_utf8_whole() {
        let d = doc("a€\r\nb");
        assert_eq!(next_boundary(&d, 1), 4);
        assert_eq!(next_boundary(&d, 4), 6);
        assert_eq!(prev_boundary(&d, 6), 4);
        assert_eq!(prev_boundary(&d, 4), 1);
        assert_eq!(snap(&d, 2), 1);
        assert_eq!(snap(&d, 5), 4);
    }

    #[test]
    fn brackets_match_across_nesting() {
        let d = doc("f(a[1], {b})");
        assert_eq!(match_bracket(&d, 1), Some((1, 11)));
        assert_eq!(match_bracket(&d, 12), Some((11, 1)));
        assert_eq!(match_bracket(&d, 3), Some((3, 5)));
        assert_eq!(match_bracket(&d, 0), None);
    }

    #[test]
    fn tab_columns_snap_to_nearest_edge() {
        assert_eq!(text_cols(b"a\tb", 4), 5);
        assert_eq!(index_at_col(b"a\tb", 1, 4), (1, 1));
        assert_eq!(index_at_col(b"a\tb", 3, 4), (2, 4));
        assert_eq!(index_at_col(b"a\tb", 9, 4), (3, 5));
    }
}
