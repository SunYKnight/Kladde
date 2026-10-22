//! Editing commands as pure plans (edits in original coordinates plus resulting selections).

use std::ops::Range;

use kladde_core::{CoreError, Document};
use kladde_syntax::Language;

use crate::text::{
    bytes, content_end, find_newline, line_col, line_offset_at_col, near_line_start, next_boundary, prev_boundary,
    word_left, word_right, LONG_LINE,
};
use crate::Selection;

/// Column selections never span more lines than this.
const MAX_BOX_LINES: usize = 100_000;
/// Comment detection looks at this much of each line.
const LINE_HEAD: u64 = 4 << 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edit {
    pub range: Range<u64>,
    pub text: Vec<u8>,
}

/// Sorted, disjoint edits against the current document and the selections after applying them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub edits: Vec<Edit>,
    pub sels: Vec<Selection>,
}

fn apply_one(doc: &mut Document, e: &Edit) -> Result<(), CoreError> {
    if e.range.is_empty() {
        doc.insert(e.range.start, &e.text)
    } else if e.text.is_empty() {
        doc.delete(e.range.clone())
    } else {
        doc.replace(e.range.clone(), &e.text)
    }
}

/// Applies edits last first so earlier offsets stay valid; several edits form one undo group. A single edit
/// stays ungrouped so the core can merge consecutive typing into one undo step.
pub(crate) fn apply(doc: &mut Document, edits: &[Edit]) -> Result<(), CoreError> {
    match edits {
        [e] => apply_one(doc, e),
        _ => {
            doc.begin_undo_group();
            let result = edits.iter().rev().try_for_each(|e| apply_one(doc, e));
            doc.end_undo_group();
            result
        }
    }
}

/// Position of `pos` after `edits`; `right` moves it past text inserted exactly at `pos`.
pub(crate) fn map_offset(edits: &[Edit], pos: u64, right: bool) -> u64 {
    let before = |e: &&Edit| e.range.end < pos || (e.range.end == pos && (e.range.start < pos || right));
    let shift: i128 =
        edits.iter().filter(before).map(|e| e.text.len() as i128 - (e.range.end - e.range.start) as i128).sum();
    let inside = edits.iter().find(|e| e.range.start < pos && pos < e.range.end);
    let base = inside.map_or(pos as i128, |e| e.range.start as i128 + if right { e.text.len() as i128 } else { 0 });
    (base + shift) as u64
}

fn overlaps(a: &Selection, b: &Selection) -> bool {
    let (ra, rb) = (a.range(), b.range());
    rb.start < ra.end || rb.start == ra.start || (rb.start == ra.end && (a.is_empty() || b.is_empty()))
}

/// Sorted by position with overlapping selections merged; the one holding the old primary caret comes first.
pub(crate) fn normalize(sels: Vec<Selection>) -> Vec<Selection> {
    let primary = sels[0];
    let mut sorted = sels;
    sorted.sort_by_key(|s| (s.range().start, s.range().end));
    let mut merged = sorted.into_iter().fold(Vec::<Selection>::new(), |mut acc, s| {
        match acc.last_mut() {
            Some(last) if overlaps(last, &s) => {
                let r = last.range().start.min(s.range().start)..last.range().end.max(s.range().end);
                *last = if last.caret >= last.anchor {
                    Selection { anchor: r.start, caret: r.end }
                } else {
                    Selection { anchor: r.end, caret: r.start }
                };
            }
            _ => acc.push(s),
        }
        acc
    });
    let p = merged.iter().position(|m| m.range().contains(&primary.caret) || m.caret == primary.caret).unwrap_or(0);
    let first = merged.remove(p);
    merged.insert(0, first);
    merged
}

fn merge_edits(mut sorted: Vec<Edit>) -> Vec<Edit> {
    sorted.sort_by_key(|e| (e.range.start, e.range.end));
    sorted.into_iter().fold(Vec::new(), |mut acc: Vec<Edit>, e| {
        match acc.last_mut() {
            Some(last) if e.range.start < last.range.end => {
                last.range.end = last.range.end.max(e.range.end);
                last.text.extend_from_slice(&e.text);
            }
            _ => acc.push(e),
        }
        acc
    })
}

/// One replacement per selection; each caret ends after its replacement text.
pub(crate) fn per_selection(sels: &[Selection], mut f: impl FnMut(&Selection) -> (Range<u64>, Vec<u8>)) -> Plan {
    let items: Vec<(Range<u64>, Vec<u8>)> = sels.iter().map(&mut f).collect();
    let edits = merge_edits(
        items
            .iter()
            .filter(|(r, t)| !r.is_empty() || !t.is_empty())
            .map(|(r, t)| Edit { range: r.clone(), text: t.clone() })
            .collect(),
    );
    let sels = items
        .iter()
        .map(|(r, t)| {
            let p = map_offset(&edits, r.start, false) + t.len() as u64;
            Selection { anchor: p, caret: p }
        })
        .collect();
    Plan { edits, sels }
}

/// Maps selections through edits; a selection start keeps text inserted at it, its end takes it in.
fn map_sels(sels: &[Selection], edits: &[Edit], left_only: bool) -> Vec<Selection> {
    sels.iter()
        .map(|s| {
            if s.is_empty() {
                let p = map_offset(edits, s.caret, !left_only);
                Selection { anchor: p, caret: p }
            } else {
                let r = s.range();
                let a = map_offset(edits, r.start, false);
                let b = map_offset(edits, r.end, !left_only);
                if s.caret >= s.anchor {
                    Selection { anchor: a, caret: b }
                } else {
                    Selection { anchor: b, caret: a }
                }
            }
        })
        .collect()
}

pub(crate) fn type_text(sels: &[Selection], text: &[u8]) -> Plan {
    per_selection(sels, |s| (s.range(), text.to_vec()))
}

/// Pastes `text` (line breaks converted to `eol`); with one line per cursor, each cursor gets its own line.
pub(crate) fn paste(sels: &[Selection], text: &str, eol: &[u8]) -> Plan {
    let lines: Vec<&str> = text.lines().collect();
    let eol_str = String::from_utf8_lossy(eol).into_owned();
    let whole = lines.join(&eol_str) + if text.ends_with('\n') { eol_str.as_str() } else { "" };
    let mut order: Vec<usize> = (0..sels.len()).collect();
    order.sort_by_key(|&i| sels[i].range().start);
    let rank: Vec<usize> = (0..sels.len()).map(|i| order.iter().position(|&o| o == i).unwrap_or(0)).collect();
    let spread = sels.len() > 1 && lines.len() == sels.len();
    let mut index = 0;
    per_selection(sels, |s| {
        let t = if spread { lines[rank[index]].as_bytes().to_vec() } else { whole.as_bytes().to_vec() };
        index += 1;
        (s.range(), t)
    })
}

pub(crate) fn backspace(doc: &Document, sels: &[Selection], word: bool) -> Plan {
    per_selection(sels, |s| {
        let from = if !s.is_empty() {
            s.range().start
        } else if word {
            word_left(doc, s.caret)
        } else {
            prev_boundary(doc, s.caret)
        };
        (from..s.range().end, Vec::new())
    })
}

pub(crate) fn delete_forward(doc: &Document, sels: &[Selection], word: bool) -> Plan {
    per_selection(sels, |s| {
        let to = if !s.is_empty() {
            s.range().end
        } else if word {
            word_right(doc, s.caret)
        } else {
            next_boundary(doc, s.caret)
        };
        (s.range().start..to, Vec::new())
    })
}

fn leading_blanks(text: &[u8]) -> usize {
    text.iter().take_while(|&&b| b == b' ' || b == b'\t').count()
}

/// Enter: line ending plus the indentation of the current line.
pub(crate) fn newline(doc: &Document, sels: &[Selection], eol: &[u8]) -> Plan {
    per_selection(sels, |s| {
        let r = s.range();
        let ls = near_line_start(doc, r.start);
        let lead = bytes(doc, ls..r.start);
        let mut text = eol.to_vec();
        text.extend_from_slice(&lead[..leading_blanks(&lead)]);
        (r, text)
    })
}

/// Tab without a multi-line selection: a tab, or spaces up to the next tab stop.
pub(crate) fn insert_tab(doc: &Document, sels: &[Selection], tab: usize, spaces: bool) -> Plan {
    per_selection(sels, |s| {
        let text = if spaces { vec![b' '; tab - line_col(doc, s.range().start, tab) % tab] } else { b"\t".to_vec() };
        (s.range(), text)
    })
}

fn is_line_start(doc: &Document, p: u64) -> bool {
    p == 0 || doc.byte_at(p - 1) == Some(b'\n')
}

/// Merged `(first line start, last line start)` spans touched by the selections; a selection ending at a line
/// start does not include that line.
fn spans(doc: &Document, sels: &[Selection]) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = sels
        .iter()
        .map(|s| {
            let r = s.range();
            let last = if r.end > r.start && is_line_start(doc, r.end) { r.end - 1 } else { r.end };
            (doc.line_start_at(r.start), doc.line_start_at(last))
        })
        .collect();
    v.sort_unstable();
    v.into_iter().fold(Vec::new(), |mut acc: Vec<(u64, u64)>, (f, l)| {
        match acc.last_mut() {
            Some(last) if f <= last.1 => last.1 = last.1.max(l),
            _ => acc.push((f, l)),
        }
        acc
    })
}

fn lines_of(doc: &Document, first: u64, last: u64) -> Vec<u64> {
    std::iter::successors(Some(first), |&ls| (ls < last).then(|| doc.next_line_start(ls))).collect()
}

fn all_lines(doc: &Document, sels: &[Selection]) -> Vec<u64> {
    spans(doc, sels).into_iter().flat_map(|(f, l)| lines_of(doc, f, l)).collect()
}

/// Up to 4 KiB of the content of the line starting at `ls`.
fn line_head(doc: &Document, ls: u64) -> Vec<u8> {
    let end =
        find_newline(doc, ls, LINE_HEAD).map_or_else(|| (ls + LINE_HEAD).min(doc.len()), |q| content_end(doc, q, ls));
    bytes(doc, ls..end)
}

/// Whether Tab should indent whole lines.
pub(crate) fn spans_lines(doc: &Document, sels: &[Selection]) -> bool {
    sels.iter().any(|s| !s.is_empty() && spans(doc, std::slice::from_ref(s)).iter().any(|(f, l)| f != l))
}

pub(crate) fn indent(doc: &Document, sels: &[Selection], tab: usize, spaces: bool) -> Plan {
    let unit = if spaces { vec![b' '; tab] } else { b"\t".to_vec() };
    let edits: Vec<Edit> = spans(doc, sels)
        .into_iter()
        .flat_map(|(f, l)| {
            let lines = lines_of(doc, f, l);
            let multi = lines.len() > 1;
            lines.into_iter().filter(move |&ls| !multi || doc.line_end_at(ls) > ls)
        })
        .map(|ls| Edit { range: ls..ls, text: unit.clone() })
        .collect();
    Plan { sels: map_sels(sels, &edits, false), edits }
}

pub(crate) fn outdent(doc: &Document, sels: &[Selection], tab: usize) -> Plan {
    let edits: Vec<Edit> = all_lines(doc, sels)
        .into_iter()
        .filter_map(|ls| {
            let lead = bytes(doc, ls..ls + tab as u64);
            let n = if lead.first() == Some(&b'\t') { 1 } else { lead.iter().take_while(|&&b| b == b' ').count() };
            (n > 0).then(|| Edit { range: ls..ls + n as u64, text: Vec::new() })
        })
        .collect();
    Plan { sels: map_sels(sels, &edits, false), edits }
}

/// Line comment if the language has one, else block comment around each selection (or trimmed line).
pub(crate) fn toggle_comment(doc: &Document, sels: &[Selection], lang: &Language) -> Plan {
    match (lang.line_comment(), lang.block_comment()) {
        (Some(prefix), _) => line_comment(doc, sels, prefix.as_bytes()),
        (None, Some((open, close))) => block_comment(doc, sels, open.as_bytes(), close.as_bytes()),
        (None, None) => Plan { edits: Vec::new(), sels: sels.to_vec() },
    }
}

fn line_comment(doc: &Document, sels: &[Selection], prefix: &[u8]) -> Plan {
    let lines: Vec<(u64, Vec<u8>)> = all_lines(doc, sels)
        .into_iter()
        .map(|ls| (ls, line_head(doc, ls)))
        .filter(|(_, t)| t.iter().any(|b| !b.is_ascii_whitespace()))
        .collect();
    let commented = !lines.is_empty() && lines.iter().all(|(_, t)| t[leading_blanks(t)..].starts_with(prefix));
    let edits: Vec<Edit> = if commented {
        lines
            .iter()
            .map(|(ls, t)| {
                let at = leading_blanks(t);
                let n = prefix.len() + usize::from(t.get(at + prefix.len()) == Some(&b' '));
                Edit { range: ls + at as u64..ls + (at + n) as u64, text: Vec::new() }
            })
            .collect()
    } else {
        let at = lines.iter().map(|(_, t)| leading_blanks(t)).min().unwrap_or(0) as u64;
        let mut text = prefix.to_vec();
        text.push(b' ');
        lines.iter().map(|(ls, _)| Edit { range: ls + at..ls + at, text: text.clone() }).collect()
    };
    Plan { sels: map_sels(sels, &edits, false), edits }
}

/// The selection, or the line content without surrounding blanks.
fn comment_range(doc: &Document, s: &Selection) -> Range<u64> {
    if s.is_empty() {
        let ls = near_line_start(doc, s.caret);
        let end = find_newline(doc, ls, LONG_LINE)
            .map_or_else(|| (ls + LONG_LINE).min(doc.len()), |q| content_end(doc, q, ls));
        let text = bytes(doc, ls..end);
        let first = text.iter().position(|b| !b.is_ascii_whitespace());
        let last = text.iter().rposition(|b| !b.is_ascii_whitespace());
        match (first, last) {
            (Some(a), Some(b)) => ls + a as u64..ls + b as u64 + 1,
            _ => s.caret..s.caret,
        }
    } else {
        s.range()
    }
}

fn block_comment(doc: &Document, sels: &[Selection], open: &[u8], close: &[u8]) -> Plan {
    let edits = merge_edits(
        sels.iter()
            .flat_map(|s| {
                let r = comment_range(doc, s);
                let (o, c) = (open.len() as u64, close.len() as u64);
                let head = bytes(doc, r.start..(r.start + o + 1).min(r.end));
                let tail = bytes(doc, r.end.saturating_sub(c + 1).max(r.start)..r.end);
                let wrapped = r.end - r.start >= o + c && head.starts_with(open) && tail.ends_with(close);
                if wrapped {
                    let sp1 = u64::from(head.get(open.len()) == Some(&b' ') && r.start + o < r.end - c);
                    let inner = r.start + o + sp1;
                    let sp2 = u64::from(tail.len() > close.len() && tail[0] == b' ' && r.end - c > inner);
                    vec![
                        Edit { range: r.start..inner, text: Vec::new() },
                        Edit { range: r.end - c - sp2..r.end, text: Vec::new() },
                    ]
                } else {
                    let mut start = open.to_vec();
                    start.push(b' ');
                    let mut end = vec![b' '];
                    end.extend_from_slice(close);
                    vec![Edit { range: r.start..r.start, text: start }, Edit { range: r.end..r.end, text: end }]
                }
            })
            .collect(),
    );
    Plan { sels: map_sels(sels, &edits, false), edits }
}

/// Ctrl+D: copies the touched lines below themselves.
pub(crate) fn duplicate_lines(doc: &Document, sels: &[Selection], eol: &[u8]) -> Plan {
    let edits: Vec<Edit> = spans(doc, sels)
        .into_iter()
        .map(|(f, l)| {
            let end = doc.line_end_at(l);
            let next = doc.next_line_start(l);
            if next > end {
                Edit { range: next..next, text: bytes(doc, f..next) }
            } else {
                let mut text = eol.to_vec();
                text.extend_from_slice(&bytes(doc, f..end));
                Edit { range: end..end, text }
            }
        })
        .collect();
    Plan { sels: map_sels(sels, &edits, true), edits }
}

/// Ctrl+Shift+L: removes the touched lines including their terminators.
pub(crate) fn delete_lines(doc: &Document, sels: &[Selection]) -> Plan {
    let edits = merge_edits(
        spans(doc, sels)
            .into_iter()
            .map(|(f, l)| {
                let next = doc.next_line_start(l);
                let has_terminator = next > doc.line_end_at(l);
                let start = if !has_terminator && f > 0 { content_end(doc, f - 1, 0) } else { f };
                Edit { range: start..next, text: Vec::new() }
            })
            .collect(),
    );
    let sels = sels
        .iter()
        .map(|s| {
            let p = map_offset(&edits, s.range().start, false);
            Selection { anchor: p, caret: p }
        })
        .collect();
    Plan { edits, sels }
}

/// Ctrl+Shift+Up/Down: swaps the block of touched lines with the line above or below.
pub(crate) fn move_lines(doc: &Document, sels: &[Selection], up: bool) -> Plan {
    let lo = sels.iter().map(|s| s.range().start).min().unwrap_or(0);
    let hi = sels.iter().map(|s| s.range().end).max().unwrap_or(0);
    let span = spans(doc, &[Selection { anchor: lo, caret: hi }]);
    let (f, l) = span[0];
    let span_end = doc.line_end_at(l);
    let swap = if up {
        (f > 0).then(|| {
            let prev = doc.line_start_at(f - 1);
            let prev_end = doc.line_end_at(prev);
            let mut text = bytes(doc, f..span_end);
            text.extend_from_slice(&bytes(doc, prev_end..f));
            text.extend_from_slice(&bytes(doc, prev..prev_end));
            (Edit { range: prev..span_end, text }, -((f - prev) as i128))
        })
    } else {
        let next = doc.next_line_start(l);
        (next > span_end && next < doc.len()).then(|| {
            let next_end = doc.line_end_at(next);
            let mut text = bytes(doc, next..next_end);
            text.extend_from_slice(&bytes(doc, span_end..next));
            text.extend_from_slice(&bytes(doc, f..span_end));
            (Edit { range: f..next_end, text }, (next_end - span_end) as i128)
        })
    };
    match swap {
        Some((edit, shift)) => {
            let moved = |p: u64| (p as i128 + shift) as u64;
            let sels = sels.iter().map(|s| Selection { anchor: moved(s.anchor), caret: moved(s.caret) }).collect();
            Plan { edits: vec![edit], sels }
        }
        None => Plan { edits: Vec::new(), sels: sels.to_vec() },
    }
}

/// Alt+drag box: one selection per line between the two line starts, from `anchor_col` to `caret_col`.
pub(crate) fn column_selections(
    doc: &Document,
    anchor_ls: u64,
    anchor_col: usize,
    caret_ls: u64,
    caret_col: usize,
    tab: usize,
) -> Vec<Selection> {
    let (top, bottom) = (anchor_ls.min(caret_ls), anchor_ls.max(caret_ls));
    std::iter::successors(Some(top), |&ls| (ls < bottom).then(|| doc.next_line_start(ls)))
        .take(MAX_BOX_LINES)
        .map(|ls| Selection {
            anchor: line_offset_at_col(doc, ls, anchor_col, tab),
            caret: line_offset_at_col(doc, ls, caret_col, tab),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::from_bytes(s.as_bytes().to_vec())
    }

    fn text(d: &Document) -> String {
        d.read_string_lossy(0..d.len())
    }

    fn caret(p: u64) -> Selection {
        Selection { anchor: p, caret: p }
    }

    fn run(d: &mut Document, plan: &Plan) {
        apply(d, &plan.edits).expect("writable document");
    }

    #[test]
    fn multi_cursor_insert_and_backspace_are_single_undo_steps() {
        let mut d = doc("ab\ncd\nef");
        let sels = vec![caret(1), caret(4), caret(7)];
        let plan = type_text(&sels, b"XY");
        run(&mut d, &plan);
        assert_eq!(text(&d), "aXYb\ncXYd\neXYf");
        assert_eq!(plan.sels, vec![caret(3), caret(8), caret(13)]);
        let back = backspace(&d, &plan.sels, false);
        run(&mut d, &back);
        assert_eq!(text(&d), "aXb\ncXd\neXf");
        assert_eq!(back.sels, vec![caret(2), caret(6), caret(10)]);
        d.undo();
        assert_eq!(text(&d), "aXYb\ncXYd\neXYf");
        d.undo();
        assert_eq!(text(&d), "ab\ncd\nef");
    }

    #[test]
    fn overlapping_word_deletes_merge() {
        let mut d = doc("hello world");
        let plan = backspace(&d, &[caret(3), caret(5)], true);
        run(&mut d, &plan);
        assert_eq!(text(&d), " world");
        assert_eq!(normalize(plan.sels), vec![caret(0)]);
    }

    #[test]
    fn newline_keeps_indentation_and_paste_spreads_lines() {
        let mut d = doc("  ab\n\tcd");
        let plan = newline(&d, &[caret(3)], b"\r\n");
        run(&mut d, &plan);
        assert_eq!(text(&d), "  a\r\n  b\n\tcd");
        let mut d = doc("x\ny");
        let plan = paste(&[caret(1), caret(3)], "1\n2", b"\n");
        run(&mut d, &plan);
        assert_eq!(text(&d), "x1\ny2");
        let mut d = doc("x");
        run(&mut d, &paste(&[caret(1)], "1\n2\n", b"\r\n"));
        assert_eq!(text(&d), "x1\r\n2\r\n");
    }

    #[test]
    fn indent_and_outdent_round_trip() {
        let original = "fn a() {\n  x();\n\n}\n";
        let mut d = doc(original);
        let sel = vec![Selection { anchor: 0, caret: 18 }];
        let plan = indent(&d, &sel, 4, true);
        run(&mut d, &plan);
        assert_eq!(text(&d), "    fn a() {\n      x();\n\n    }\n");
        assert_eq!(plan.sels, vec![Selection { anchor: 0, caret: 30 }]);
        let plan = outdent(&d, &plan.sels, 4);
        run(&mut d, &plan);
        assert_eq!(text(&d), original);
        let mut d = doc("a\nb");
        let p = indent(&d, &[Selection { anchor: 0, caret: 3 }], 4, false);
        run(&mut d, &p);
        assert_eq!(text(&d), "\ta\n\tb");
        let mut d = doc("ab");
        let p = insert_tab(&d, &[caret(1)], 4, true);
        run(&mut d, &p);
        assert_eq!(text(&d), "a   b");
    }

    #[test]
    fn toggle_line_comment_round_trip() {
        let rust = kladde_syntax::language_by_id("rust").expect("rust language");
        let original = "fn a() {\n    x();\n\n}";
        let mut d = doc(original);
        let sel = vec![Selection { anchor: 0, caret: d.len() }];
        let plan = toggle_comment(&d, &sel, rust);
        run(&mut d, &plan);
        assert_eq!(text(&d), "// fn a() {\n//     x();\n\n// }");
        let plan = toggle_comment(&d, &plan.sels, rust);
        run(&mut d, &plan);
        assert_eq!(text(&d), original);
    }

    #[test]
    fn toggle_block_comment_round_trip() {
        let css = kladde_syntax::language_by_id("css").expect("css language");
        assert_eq!(css.line_comment(), None);
        let original = "  a { b: c; }\n";
        let mut d = doc(original);
        let plan = toggle_comment(&d, &[caret(5)], css);
        run(&mut d, &plan);
        assert_eq!(text(&d), "  /* a { b: c; } */\n");
        let plan = toggle_comment(&d, &plan.sels, css);
        run(&mut d, &plan);
        assert_eq!(text(&d), original);
    }

    #[test]
    fn line_operations() {
        let mut d = doc("a\nb\nc");
        let p = duplicate_lines(&d, &[caret(2)], b"\n");
        run(&mut d, &p);
        assert_eq!(text(&d), "a\nb\nb\nc");
        let mut d = doc("a\nb\nc");
        let p = duplicate_lines(&d, &[caret(5)], b"\n");
        run(&mut d, &p);
        assert_eq!(text(&d), "a\nb\nc\nc");
        let mut d = doc("a\nb\nc");
        let p = delete_lines(&d, &[caret(4)]);
        run(&mut d, &p);
        assert_eq!(text(&d), "a\nb");
        let mut d = doc("a\nb\nc");
        let plan = move_lines(&d, &[caret(2)], true);
        run(&mut d, &plan);
        assert_eq!(text(&d), "b\na\nc");
        assert_eq!(plan.sels, vec![caret(0)]);
        let plan = move_lines(&d, &plan.sels, false);
        run(&mut d, &plan);
        assert_eq!(text(&d), "a\nb\nc");
        assert_eq!(plan.sels, vec![caret(2)]);
    }

    #[test]
    fn column_selection_follows_visual_columns() {
        let d = doc("abcdef\n\tx\nab\n");
        let sels = column_selections(&d, 10, 1, 0, 5, 4);
        let ranges: Vec<(u64, u64)> = sels.iter().map(|s| (s.anchor, s.caret)).collect();
        // Column 1 in the tab snaps to its start; short lines clamp.
        assert_eq!(ranges, vec![(1, 5), (7, 9), (11, 12)]);
    }
}
