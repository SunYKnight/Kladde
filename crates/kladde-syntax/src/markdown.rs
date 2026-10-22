//! Markdown/Quarto lexer with fenced code highlighted in the fence language and YAML front matter.

use crate::langs::{index_of, language_by_id, language_for_fence, languages, Mode};
use crate::{char_len, lex_sub, push, LineState, Sub, Token, TokenKind as K};

pub(crate) const START: u8 = 0;
pub(crate) const NORMAL: u8 = 1;
pub(crate) const FRONT: u8 = 2;
/// Inside a fence: depth = fence length, aux = (language index + 1) << 8 | fence byte.
pub(crate) const FENCE: u8 = 3;
pub(crate) const COMMENT: u8 = 4;

pub(crate) fn lex(state: LineState, line: &str, out: &mut Vec<Token>) -> LineState {
    let mode = state.outer.mode;
    match mode {
        FRONT => front_matter(state, line, out),
        FENCE => fence_body(state, line, out),
        COMMENT => {
            let next = html_comment(line, 0, out);
            inline(line, next, out)
        }
        START if line.trim_end() == "---" => {
            push(out, 0, line.len(), K::Preprocessor);
            outer(Sub { mode: FRONT, ..Sub::default() })
        }
        _ => block(line, out),
    }
}

fn outer(sub: Sub) -> LineState {
    LineState { outer: sub, inner: Sub::default() }
}

fn normal() -> LineState {
    outer(Sub { mode: NORMAL, ..Sub::default() })
}

fn front_matter(state: LineState, line: &str, out: &mut Vec<Token>) -> LineState {
    let end = line.trim_end();
    if end == "---" || end == "..." {
        push(out, 0, line.len(), K::Preprocessor);
        normal()
    } else {
        let yaml = language_by_id("yaml").expect("yaml is part of the language table");
        LineState { outer: state.outer, inner: lex_sub(yaml, state.inner, line, out) }
    }
}

/// Fence run at the start of `line` (up to 3 spaces indent): (fence byte, run length, end of run).
fn fence_run(line: &str) -> Option<(u8, usize, usize)> {
    let b = line.as_bytes();
    let indent = b.iter().take(4).take_while(|&&c| c == b' ').count();
    let c = b.get(indent).copied().unwrap_or(0);
    let run = b[indent..].iter().take_while(|&&x| x == c).count();
    (indent < 4 && (c == b'`' || c == b'~') && run >= 3).then_some((c, run, indent + run))
}

fn fence_body(state: LineState, line: &str, out: &mut Vec<Token>) -> LineState {
    let fence_byte = (state.outer.aux & 0xFF) as u8;
    let closing = fence_run(line).filter(|&(c, run, end)| {
        c == fence_byte && run >= usize::from(state.outer.depth) && line[end..].trim().is_empty()
    });
    let lang = usize::from(state.outer.aux >> 8).checked_sub(1).and_then(|i| languages().get(i));
    match (closing, lang) {
        (Some(_), _) => {
            push(out, 0, line.len(), K::Code);
            normal()
        }
        (None, Some(l)) => LineState { outer: state.outer, inner: lex_sub(l, state.inner, line, out) },
        (None, None) => {
            push(out, 0, line.len(), K::Code);
            state
        }
    }
}

fn block(line: &str, out: &mut Vec<Token>) -> LineState {
    let b = line.as_bytes();
    let first = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let rest = &line[first..];
    let hashes = rest.bytes().take_while(|&c| c == b'#').count();
    let heading = first < 4 && (1..=6).contains(&hashes) && rest.as_bytes().get(hashes).map_or(true, |&c| c == b' ');
    let rule_byte = rest.bytes().next().unwrap_or(0);
    let rule = first < 4
        && b"-*_=".contains(&rule_byte)
        && rest.bytes().filter(|&c| c == rule_byte).count() >= 3
        && rest.bytes().all(|c| c == rule_byte || c == b' ');
    match fence_run(line) {
        Some((c, run, end)) if c == b'~' || !line[end..].contains('`') => {
            push(out, 0, line.len(), K::Code);
            let lang = language_for_fence(&line[end..]).filter(|l| !matches!(l.mode, Mode::Markdown));
            let lang_index = lang.map_or(0, |l| index_of(l) + 1) as u16;
            let depth = run.min(255) as u8;
            outer(Sub { mode: FENCE, depth, aux: (lang_index << 8) | u16::from(c), ..Sub::default() })
        }
        _ if heading => {
            push(out, 0, line.len(), K::Heading);
            normal()
        }
        _ if rule => {
            push(out, 0, line.len(), if rule_byte == b'=' { K::Heading } else { K::Operator });
            normal()
        }
        _ if rest.starts_with(":::") => {
            push(out, 0, line.len(), K::Preprocessor);
            normal()
        }
        _ => {
            let p = markers(line, first, out);
            inline(line, Cursor { pos: p, mode: NORMAL }, out)
        }
    }
}

/// Block quote and list markers; returns where inline content starts.
fn markers(line: &str, first: usize, out: &mut Vec<Token>) -> usize {
    let b = line.as_bytes();
    let mut p = first;
    while b.get(p) == Some(&b'>') {
        push(out, p, p + 1, K::Comment);
        p += 1;
        p += b[p..].iter().take_while(|&&c| c == b' ').count();
    }
    let digits = b[p..].iter().take(9).take_while(|c| c.is_ascii_digit()).count();
    let bullet = b.get(p).is_some_and(|c| b"-*+".contains(c)) && b.get(p + 1).map_or(true, |&c| c == b' ');
    let ordered = digits > 0
        && b.get(p + digits).is_some_and(|&c| c == b'.' || c == b')')
        && b.get(p + digits + 1).map_or(true, |&c| c == b' ');
    let marker = if bullet {
        1
    } else if ordered {
        digits + 1
    } else {
        0
    };
    push(out, p, p + marker, K::Operator);
    p + marker
}

struct Cursor {
    pos: usize,
    mode: u8,
}

/// Consumes the rest of an HTML comment from `from`.
fn html_comment(line: &str, from: usize, out: &mut Vec<Token>) -> Cursor {
    let found = line[from..].find("-->");
    let end = found.map_or(line.len(), |q| from + q + 3);
    push(out, from, end, K::Comment);
    Cursor { pos: end, mode: if found.is_some() { NORMAL } else { COMMENT } }
}

/// Next position of one byte; caches the last answer so repeated lookups stay linear.
struct Finder {
    byte: u8,
    from: usize,
    at: Option<usize>,
}

impl Finder {
    fn new(byte: u8) -> Finder {
        Finder { byte, from: usize::MAX, at: None }
    }

    fn next(&mut self, b: &[u8], from: usize) -> Option<usize> {
        let valid = self.from <= from && self.at.map_or(true, |a| a >= from);
        if !valid {
            self.from = from;
            self.at = b.get(from..).and_then(|s| s.iter().position(|&c| c == self.byte)).map(|q| from + q);
        }
        self.at
    }
}

struct Inline<'a, 'o> {
    s: &'a str,
    b: &'a [u8],
    pos: usize,
    out: &'o mut Vec<Token>,
    backtick: Finder,
    star: Finder,
    underscore: Finder,
    bracket: Finder,
    paren: Finder,
    angle: Finder,
}

fn inline(line: &str, cursor: Cursor, out: &mut Vec<Token>) -> LineState {
    let mut x = Inline {
        s: line,
        b: line.as_bytes(),
        pos: cursor.pos,
        out,
        backtick: Finder::new(b'`'),
        star: Finder::new(b'*'),
        underscore: Finder::new(b'_'),
        bracket: Finder::new(b']'),
        paren: Finder::new(b')'),
        angle: Finder::new(b'>'),
    };
    let mut mode = cursor.mode;
    while x.pos < x.b.len() && mode != COMMENT {
        if x.s[x.pos..].starts_with("<!--") {
            let c = html_comment(line, x.pos, x.out);
            x.pos = c.pos;
            mode = c.mode;
        } else {
            x.step();
        }
    }
    outer(Sub { mode: if mode == COMMENT { COMMENT } else { NORMAL }, ..Sub::default() })
}

impl Inline<'_, '_> {
    fn at(&self, p: usize) -> u8 {
        self.b.get(p).copied().unwrap_or(0)
    }

    fn run(&self, p: usize) -> usize {
        let c = self.at(p);
        self.b[p..].iter().take_while(|&&x| x == c).count()
    }

    fn emit(&mut self, end: usize, kind: K) {
        push(self.out, self.pos, end, kind);
        self.pos = end;
    }

    fn step(&mut self) {
        let c = self.b[self.pos];
        let next = self.at(self.pos + 1);
        let handled = match c {
            b'\\' if next.is_ascii_punctuation() => {
                self.emit(self.pos + 2, K::Escape);
                true
            }
            b'`' => self.code_span(),
            b'*' | b'_' => self.emphasis(c),
            b'[' => self.link(self.pos + 1),
            b'!' if next == b'[' => self.link(self.pos + 2),
            b'<' => self.angle_link(),
            b'h' => self.bare_url(),
            b'{' if self.s[self.pos..].starts_with("{{<") => {
                let end = self.s[self.pos..].find(">}}").map_or(self.b.len(), |q| self.pos + q + 3);
                self.emit(end, K::Preprocessor);
                true
            }
            _ => false,
        };
        if !handled {
            self.pos += char_len(c);
        }
    }

    fn code_span(&mut self) -> bool {
        let n = self.run(self.pos);
        let close = self.backtick.next(self.b, self.pos + n);
        let matched = close.filter(|&q| self.run(q) == n);
        match matched {
            Some(q) => self.emit(q + n, K::Code),
            None => self.pos += n,
        }
        true
    }

    fn emphasis(&mut self, c: u8) -> bool {
        let n = self.run(self.pos);
        let opens = !self.at(self.pos + n).is_ascii_whitespace()
            && self.pos + n < self.b.len()
            && (c == b'*' || self.pos == 0 || !self.b[self.pos - 1].is_ascii_alphanumeric());
        let finder = if c == b'*' { &mut self.star } else { &mut self.underscore };
        let close = if opens { finder.next(self.b, self.pos + n) } else { None };
        match close {
            Some(q) => {
                let end = q + self.run(q);
                self.emit(end, K::Emphasis);
            }
            None => self.pos += n,
        }
        true
    }

    /// `[text](url)`, `[text][ref]` and images; `text_from` is just after the `[`.
    fn link(&mut self, text_from: usize) -> bool {
        let close = self.bracket.next(self.b, text_from);
        let target = close.and_then(|q| match self.at(q + 1) {
            b'(' => self.paren.next(self.b, q + 2),
            b'[' => self.bracket.next(self.b, q + 2),
            _ => None,
        });
        match target {
            Some(end) => self.emit(end + 1, K::Link),
            None => self.pos = text_from,
        }
        true
    }

    fn angle_link(&mut self) -> bool {
        let close = self.angle.next(self.b, self.pos + 1);
        let inner = close.map(|q| &self.s[self.pos + 1..q]);
        let autolink = inner.is_some_and(|t| !t.is_empty() && !t.contains(' ') && (t.contains(':') || t.contains('@')));
        let tag = inner.is_some_and(|t| t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/'));
        match close {
            Some(q) if autolink => self.emit(q + 1, K::Link),
            Some(q) if tag => self.emit(q + 1, K::Tag),
            _ => self.pos += 1,
        }
        true
    }

    fn bare_url(&mut self) -> bool {
        let rest = &self.s[self.pos..];
        let boundary = self.pos == 0 || !self.b[self.pos - 1].is_ascii_alphanumeric();
        let is_url = boundary && (rest.starts_with("http://") || rest.starts_with("https://"));
        if is_url {
            let len = rest.find(|c: char| c.is_whitespace() || c == ')' || c == '>' || c == ']').unwrap_or(rest.len());
            self.emit(self.pos + len, K::Link);
        }
        is_url
    }
}
