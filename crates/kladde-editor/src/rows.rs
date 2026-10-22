//! Visual rows anchored by byte offset: soft wrap, long-line segments and hidden (folded) ranges.

use std::ops::Range;

use kladde_core::Document;

use crate::text::{
    bytes, char_cols, content_end, find_newline, index_at_col, is_continuation, prev_boundary, rfind_newline,
    text_cols, Chars, LONG_LINE, SEGMENT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// Soft-wrap width in columns.
    pub wrap: Option<usize>,
    pub tab: usize,
}

/// One visual row: content `start..end`, then the terminator (if any) up to `next`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub start: u64,
    pub end: u64,
    pub next: u64,
    /// The row starts a document line (gets a line number).
    pub line_start: bool,
    /// Part of a line longer than 64 KiB.
    pub long: bool,
}

/// Unit rows are computed from: a whole line up to 64 KiB, else one 4 KiB segment of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Block {
    start: u64,
    end: u64,
    next: u64,
    line_start: bool,
    long: bool,
}

pub(crate) struct Rows<'a> {
    pub doc: &'a Document,
    pub layout: Layout,
    /// Sorted, disjoint ranges starting and ending at line starts.
    pub hidden: &'a [Range<u64>],
}

fn is_line_start(doc: &Document, p: u64) -> bool {
    p == 0 || doc.byte_at(p - 1) == Some(b'\n')
}

/// Byte offsets (relative to `text`) where soft-wrapped rows start; the first is 0.
pub(crate) fn wrap_points(text: &[u8], cols: usize, tab: usize) -> Vec<usize> {
    let mut points = vec![0];
    let mut col = 0;
    let mut blank_end: Option<usize> = None;
    for (i, c, n) in Chars::new(text) {
        let row_start = *points.last().expect("starts with 0");
        let blank = c == ' ' || c == '\t';
        // One blank may overhang so rows break after spaces.
        if col > 0 && col + char_cols(c, col, tab) > cols && !(blank && col <= cols) {
            let cut = blank_end.filter(|&p| p > row_start && p <= i).unwrap_or(i);
            points.push(cut);
            col = text_cols(&text[cut..i], tab);
            blank_end = None;
        }
        col += char_cols(c, col, tab);
        if blank {
            blank_end = Some(i + n);
        }
    }
    points
}

impl Rows<'_> {
    /// First char boundary at or after `b`, moved past a line terminator that starts there.
    fn boundary(&self, b: u64) -> u64 {
        let len = self.doc.len();
        if b >= len {
            len
        } else {
            let w = bytes(self.doc, b..b + 6);
            let lead = w.iter().take(4).position(|&x| !is_continuation(x)).unwrap_or(0);
            let rest = &w[lead..];
            let term = if rest.starts_with(b"\r\n") { 2 } else { usize::from(rest.starts_with(b"\n")) };
            (b + (lead + term) as u64).min(len)
        }
    }

    fn block_at(&self, off: u64) -> Block {
        let doc = self.doc;
        let len = doc.len();
        let off = off.min(len);
        let line_start = rfind_newline(doc, off, LONG_LINE + 1).map(|p| p + 1).or((off <= LONG_LINE + 1).then_some(0));
        let short = line_start.and_then(|ls| {
            let end_next = match find_newline(doc, ls, LONG_LINE + 2) {
                Some(q) => Some((content_end(doc, q, ls), q + 1)),
                None => (ls + LONG_LINE + 2 >= len).then_some((len, len)),
            };
            end_next.filter(|&(end, _)| end - ls <= LONG_LINE).map(|(end, next)| Block {
                start: ls,
                end,
                next,
                line_start: true,
                long: false,
            })
        });
        short.unwrap_or_else(|| self.segment(off, line_start))
    }

    /// Segment of a long line; boundaries sit at absolute 4 KiB multiples so they do not depend on
    /// knowing where the line starts.
    fn segment(&self, off: u64, line_start: Option<u64>) -> Block {
        let doc = self.doc;
        let len = doc.len();
        let floor = line_start.unwrap_or(0);
        let probe = if off == len && off > floor { off - 1 } else { off };
        let k = probe / SEGMENT;
        let k = if k > 0 && self.boundary(k * SEGMENT) > probe { k - 1 } else { k };
        let start = if k == 0 { floor } else { self.boundary(k * SEGMENT).max(floor) };
        let seg_end = self.boundary((k + 1) * SEGMENT);
        let line_start = is_line_start(doc, start);
        match find_newline(doc, start, seg_end - start) {
            Some(q) => Block { start, end: content_end(doc, q, start), next: q + 1, line_start, long: true },
            None => Block { start, end: seg_end, next: seg_end, line_start, long: true },
        }
    }

    fn block_rows(&self, b: &Block) -> Vec<Row> {
        let points = match self.layout.wrap {
            Some(cols) if b.end > b.start => wrap_points(&bytes(self.doc, b.start..b.end), cols, self.layout.tab),
            _ => vec![0],
        };
        let n = points.len();
        (0..n)
            .map(|i| {
                let start = b.start + points[i] as u64;
                let last = i + 1 == n;
                let end = if last { b.end } else { b.start + points[i + 1] as u64 };
                Row {
                    start,
                    end,
                    next: if last { b.next } else { end },
                    line_start: i == 0 && b.line_start,
                    long: b.long,
                }
            })
            .collect()
    }

    /// The row containing `off`; an offset on a wrap boundary belongs to the later row.
    pub(crate) fn row_at(&self, off: u64) -> Row {
        self.row_in(&self.block_at(off), off)
    }

    fn skip_hidden(&self, p: u64) -> u64 {
        self.hidden.iter().find(|h| h.start == p).map_or(p, |h| h.end)
    }

    /// Start of the row after `r`.
    fn follow(&self, r: &Row) -> Option<u64> {
        let len = self.doc.len();
        if r.end >= len {
            None
        } else {
            let p = self.skip_hidden(r.next);
            (p < len || is_line_start(self.doc, len)).then_some(p)
        }
    }

    pub(crate) fn next(&self, r: &Row) -> Option<Row> {
        self.follow(r).map(|p| self.row_in(&self.block_after(r, p), p))
    }

    /// Block containing `p`, the start of the row after `r`; a long line continues without a backward scan.
    fn block_after(&self, r: &Row, p: u64) -> Block {
        if r.long && r.next == r.end && p == r.next {
            self.segment(p, None)
        } else {
            self.block_at(p)
        }
    }

    fn row_in(&self, b: &Block, off: u64) -> Row {
        let rows = self.block_rows(b);
        let i = rows.iter().rposition(|r| r.start <= off).unwrap_or(0);
        rows[i]
    }

    pub(crate) fn prev(&self, r: &Row) -> Option<Row> {
        let p = self.hidden.iter().find(|h| h.end == r.start).map_or(r.start, |h| h.start);
        (p > 0).then(|| self.row_at(p - 1))
    }

    /// Start of the row `n` rows above `r` (or the first row).
    pub(crate) fn back(&self, r: Row, n: usize) -> u64 {
        (0..n).try_fold(r, |row, _| self.prev(&row).ok_or(row)).unwrap_or_else(|first| first).start
    }

    /// Start of the row `n` rows below `r` (or the last row).
    pub(crate) fn forward(&self, r: Row, n: usize) -> u64 {
        (0..n).try_fold(r, |row, _| self.next(&row).ok_or(row)).unwrap_or_else(|last| last).start
    }

    pub(crate) fn iter_from(&self, off: u64) -> RowIter<'_> {
        let buf = self.block_rows(&self.block_at(off));
        let idx = buf.iter().rposition(|r| r.start <= off).unwrap_or(0);
        RowIter { rows: self, buf, idx }
    }

    /// Visual column of `off` within `r` (tabs expanded from the row start).
    pub(crate) fn col_of(&self, r: &Row, off: u64) -> usize {
        text_cols(&bytes(self.doc, r.start..off.clamp(r.start, r.end)), self.layout.tab)
    }

    /// Offset in `r` nearest to visual column `col`; never the end of a row that continues on the next one.
    pub(crate) fn off_at_col(&self, r: &Row, col: usize) -> u64 {
        let text = bytes(self.doc, r.start..r.end);
        let off = r.start + index_at_col(&text, col, self.layout.tab).0 as u64;
        if off == r.end && r.next == r.end && r.end > r.start && r.end < self.doc.len() {
            prev_boundary(self.doc, r.end)
        } else {
            off
        }
    }
}

/// Rows in document order; computes each block once.
pub(crate) struct RowIter<'a> {
    rows: &'a Rows<'a>,
    buf: Vec<Row>,
    idx: usize,
}

impl Iterator for RowIter<'_> {
    type Item = Row;

    fn next(&mut self) -> Option<Row> {
        if self.idx >= self.buf.len() {
            let following = self.buf.last().and_then(|r| self.rows.follow(r));
            self.buf = following.zip(self.buf.last()).map_or_else(Vec::new, |(p, r)| {
                let b = self.rows.block_after(r, p);
                self.rows.block_rows(&b)
            });
            self.idx = 0;
        }
        let row = self.buf.get(self.idx).copied();
        self.idx += 1;
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_of<'a>(doc: &'a Document, wrap: Option<usize>, hidden: &'a [Range<u64>]) -> Rows<'a> {
        Rows { doc, layout: Layout { wrap, tab: 4 }, hidden }
    }

    fn long_doc() -> (Document, usize) {
        let unit = "aä€x𝄞";
        let line: String = unit.repeat(200_000 / unit.len());
        let text = format!("head\n{line}\r\nshort\n");
        (Document::from_bytes(text.into_bytes()), line.len())
    }

    #[test]
    fn long_lines_split_into_segments_on_char_boundaries() {
        let (doc, line_len) = long_doc();
        let rows = rows_of(&doc, None, &[]);
        let all: Vec<Row> = rows.iter_from(0).collect();
        assert_eq!(all.first().map(|r| (r.start, r.end)), Some((0, 4)));
        all.windows(2).for_each(|w| assert_eq!(w[0].next, w[1].start));
        let long: Vec<&Row> = all.iter().filter(|r| r.long).collect();
        assert!(long.len() >= line_len / SEGMENT as usize);
        long.iter().for_each(|r| {
            assert!(r.end - r.start <= SEGMENT + 6, "segment too long: {r:?}");
            assert!(!doc.byte_at(r.start).is_some_and(is_continuation), "split char at {}", r.start);
        });
        let tail: Vec<(u64, u64, bool)> = all.iter().rev().take(2).map(|r| (r.start, r.end, r.long)).collect();
        let len = doc.len();
        assert_eq!(tail, vec![(len, len, false), (len - 6, len - 1, false)]);
        // The last segment ends before the CR of the CRLF.
        assert_eq!(long.last().map(|r| (r.end, r.next)), Some((5 + line_len as u64, 7 + line_len as u64)));
    }

    #[test]
    fn row_at_agrees_with_iteration_and_prev_inverts_next() {
        let (doc, _) = long_doc();
        let rows = rows_of(&doc, None, &[]);
        let all: Vec<Row> = rows.iter_from(0).collect();
        all.iter().step_by(7).for_each(|r| {
            assert_eq!(rows.row_at(r.start), *r);
            assert_eq!(rows.row_at((r.start + r.end) / 2), *r);
        });
        all.windows(2).step_by(5).for_each(|w| {
            assert_eq!(rows.next(&w[0]), Some(w[1]));
            assert_eq!(rows.prev(&w[1]), Some(w[0]));
        });
        assert_eq!(rows.row_at(doc.len()), *all.last().expect("rows"));
    }

    #[test]
    fn lines_up_to_64k_are_not_segmented() {
        let doc = Document::from_bytes(vec![b'x'; LONG_LINE as usize]);
        let rows = rows_of(&doc, None, &[]);
        let all: Vec<Row> = rows.iter_from(0).collect();
        assert_eq!(all, vec![Row { start: 0, end: LONG_LINE, next: LONG_LINE, line_start: true, long: false }]);
    }

    #[test]
    fn soft_wrap_breaks_after_blanks_else_hard() {
        assert_eq!(wrap_points(b"aaa bbb ccc", 8, 4), vec![0, 8]);
        assert_eq!(wrap_points(b"abcdefghij", 4, 4), vec![0, 4, 8]);
        assert_eq!(wrap_points(b"\t\tab", 5, 4), vec![0, 2]);
        let doc = Document::from_bytes(b"aaa bbb ccc\nz".to_vec());
        let rows = rows_of(&doc, Some(8), &[]);
        let all: Vec<(u64, u64, bool)> = rows.iter_from(0).map(|r| (r.start, r.end, r.line_start)).collect();
        assert_eq!(all, vec![(0, 8, true), (8, 11, false), (12, 13, true)]);
        assert_eq!(rows.row_at(8).start, 8);
        assert_eq!(rows.off_at_col(&rows.row_at(0), 20), 7);
    }

    #[test]
    fn hidden_ranges_are_skipped_both_ways() {
        let doc = Document::from_bytes(b"a\nb\nc\nd\n".to_vec());
        let hidden: Vec<Range<u64>> = std::iter::once(2..6).collect();
        let rows = rows_of(&doc, None, &hidden);
        let starts: Vec<u64> = rows.iter_from(0).map(|r| r.start).collect();
        assert_eq!(starts, vec![0, 6, 8]);
        let d = rows.row_at(6);
        assert_eq!(rows.prev(&d).map(|r| r.start), Some(0));
        let to_end: Vec<Range<u64>> = std::iter::once(2..8).collect();
        let rows = rows_of(&doc, None, &to_end);
        assert_eq!(rows.iter_from(0).map(|r| r.start).collect::<Vec<_>>(), vec![0, 8]);
    }
}
