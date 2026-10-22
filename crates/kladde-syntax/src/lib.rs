//! Lightweight, table-driven, resumable syntax highlighting and folding (no syntect/tree-sitter).
//!
//! Every lexer works one line at a time in linear time and carries multi-line constructs (block comments,
//! multi-line strings, Markdown fences, ...) in a small [`LineState`]. Text between tokens is plain.

mod code;
mod fold;
mod langs;
mod markdown;
mod markup;
mod simple;

use std::ops::Range;

pub use fold::{compute_folds, FoldRange};
pub use langs::{language_by_id, language_for_fence, language_for_path, languages, Language};

use langs::Mode;

/// Token classes. Diff uses `Heading` for file headers, `Preprocessor` for hunk headers, `String` for added and
/// `Constant` for removed lines. CSV/TSV fields are `CsvColumn(column % RAINBOW_COLUMNS)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Plain,
    Keyword,
    Type,
    Function,
    String,
    Number,
    Comment,
    Preprocessor,
    Operator,
    Tag,
    Attribute,
    Heading,
    Emphasis,
    Link,
    Code,
    Variable,
    Constant,
    Escape,
    CsvColumn(u8),
}

/// Number of distinct `TokenKind::CsvColumn` values before the colours repeat.
pub const RAINBOW_COLUMNS: u8 = 8;

/// A highlighted byte range of one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

/// Lexer state at the start of a line; `LineState::default()` is the state of the first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct LineState {
    outer: Sub,
    // State of the language embedded in a Markdown fence or front matter.
    inner: Sub,
}

/// Resumable state of one lexer; the meaning of the fields depends on the lexer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Sub {
    pub mode: u8,
    pub depth: u8,
    pub nest: u8,
    pub aux: u16,
}

/// Highlights one line (without terminator), starting in `state`; returns the tokens and the state of the next line.
pub fn highlight_line(lang: &'static Language, state: LineState, line: &str) -> (Vec<Token>, LineState) {
    let mut out = Vec::new();
    let next = lex_line(lang, state, line, &mut out);
    (out, next)
}

pub(crate) fn lex_line(lang: &'static Language, state: LineState, line: &str, out: &mut Vec<Token>) -> LineState {
    match lang.mode {
        Mode::Markdown => markdown::lex(state, line, out),
        _ => LineState { outer: lex_sub(lang, state.outer, line, out), inner: Sub::default() },
    }
}

/// Lexes with a single-level state; Markdown cannot be embedded, so it degrades to plain text here.
pub(crate) fn lex_sub(lang: &'static Language, sub: Sub, line: &str, out: &mut Vec<Token>) -> Sub {
    match lang.mode {
        Mode::Code(spec) => code::lex(spec, sub, line, out),
        Mode::Xml => markup::lex(sub, line, out),
        Mode::Csv(delim) => simple::csv(delim, sub, line, out),
        Mode::Diff => simple::diff(line, out),
        Mode::Plain | Mode::Markdown => Sub::default(),
    }
}

/// Appends a token, merging it into the previous one when adjacent and of the same kind.
pub(crate) fn push(out: &mut Vec<Token>, start: usize, end: usize, kind: TokenKind) {
    if start < end {
        match out.last_mut() {
            Some(t) if t.range.end == start && t.kind == kind => t.range.end = end,
            _ => out.push(Token { range: start..end, kind }),
        }
    }
}

/// Length of the UTF-8 character starting with byte `c`.
pub(crate) fn char_len(c: u8) -> usize {
    match c {
        0..=0xBF => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Start-of-line states for lines `0..n`, extended lazily.
pub struct HighlightCache {
    lang: &'static Language,
    states: Vec<LineState>,
    scratch: Vec<Token>,
}

impl HighlightCache {
    pub fn new(lang: &'static Language) -> HighlightCache {
        HighlightCache { lang, states: vec![LineState::default()], scratch: Vec::new() }
    }

    pub fn language(&self) -> &'static Language {
        self.lang
    }

    /// Forgets the states of the lines after `line`; call when `line` or anything below it changed.
    pub fn invalidate_from_line(&mut self, line: u64) {
        let keep = usize::try_from(line).unwrap_or(usize::MAX).saturating_add(1);
        self.states.truncate(keep);
    }

    /// State at the start of `line`, lexing missing lines obtained from `get_line` (0-based, no terminator).
    pub fn state_for_line(&mut self, line: u64, get_line: &mut dyn FnMut(u64) -> String) -> LineState {
        let target = usize::try_from(line).unwrap_or(usize::MAX);
        while self.states.len() <= target {
            let n = self.states.len() - 1;
            let text = get_line(n as u64);
            self.scratch.clear();
            let next = lex_line(self.lang, self.states[n], &text, &mut self.scratch);
            self.states.push(next);
        }
        self.states[target]
    }
}
