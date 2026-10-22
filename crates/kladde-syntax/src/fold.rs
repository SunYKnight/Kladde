//! Fold ranges per language family, reusing the lexers so delimiters in strings and comments are ignored.

use crate::langs::{FoldStyle, Language};
use crate::{code, lex_line, markdown, markup, LineState, Token, TokenKind as K};

/// Foldable line range; `start_line` holds the opener, `end_line` the last line of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoldRange {
    pub start_line: u64,
    pub end_line: u64,
}

const HTML_VOID: &[&str] =
    &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

/// Fold ranges sorted by start line, at most one (the outermost) per start line, each spanning at least 2 lines.
pub fn compute_folds(lang: &'static Language, text: &str) -> Vec<FoldRange> {
    let mut walker = Walker { lang, state: LineState::default(), tokens: Vec::new(), ranges: Vec::new() };
    let lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    match lang.fold {
        FoldStyle::None => {}
        FoldStyle::Braces => walker.braces(lines, false),
        FoldStyle::Brackets => walker.braces(lines, true),
        FoldStyle::Indent => walker.indent(lines),
        FoldStyle::Tags { html } => walker.tags(lines, html),
        FoldStyle::Headings => walker.headings(lines),
        FoldStyle::Sections => walker.sections(lines),
    }
    let mut ranges = walker.ranges;
    ranges.retain(|r| r.end_line > r.start_line);
    ranges.sort_by(|a, b| a.start_line.cmp(&b.start_line).then(b.end_line.cmp(&a.end_line)));
    ranges.dedup_by_key(|r| r.start_line);
    ranges
}

struct Walker {
    lang: &'static Language,
    state: LineState,
    tokens: Vec<Token>,
    ranges: Vec<FoldRange>,
}

/// Byte positions of `line` not covered by string, comment or similar tokens.
fn structural<'a>(line: &'a str, tokens: &'a [Token]) -> impl Iterator<Item = (usize, u8)> + 'a {
    let mut ti = 0;
    line.bytes().enumerate().filter(move |&(p, _)| {
        while ti < tokens.len() && tokens[ti].range.end <= p {
            ti += 1;
        }
        let blocked = tokens.get(ti).is_some_and(|t| {
            t.range.start <= p && matches!(t.kind, K::String | K::Comment | K::Escape | K::Preprocessor | K::Code)
        });
        !blocked
    })
}

impl Walker {
    /// Lexes `line`; returns the state at its start.
    fn lex(&mut self, line: &str) -> LineState {
        let start = self.state;
        self.tokens.clear();
        self.state = lex_line(self.lang, start, line, &mut self.tokens);
        start
    }

    fn add(&mut self, start_line: u64, end_line: u64) {
        self.ranges.push(FoldRange { start_line, end_line });
    }

    fn braces<'a>(&mut self, lines: impl Iterator<Item = &'a str>, square: bool) {
        let mut stack: Vec<u64> = Vec::new();
        let mut closed = Vec::new();
        let open = |c: u8| c == b'{' || (square && c == b'[');
        let close = |c: u8| c == b'}' || (square && c == b']');
        for (i, line) in lines.enumerate() {
            self.lex(line);
            closed.clear();
            for (_, c) in structural(line, &self.tokens).filter(|&(_, c)| open(c) || close(c)) {
                if open(c) {
                    stack.push(i as u64);
                } else if let Some(start) = stack.pop() {
                    closed.push(start);
                }
            }
            for &start in &closed {
                self.add(start, i as u64);
            }
        }
    }

    fn indent<'a>(&mut self, lines: impl Iterator<Item = &'a str>) {
        let mut stack: Vec<(usize, u64)> = Vec::new();
        let mut last_content = 0u64;
        for (i, line) in lines.enumerate() {
            let start = self.lex(line);
            let first = line.bytes().position(|c| c != b' ' && c != b'\t');
            let leading_comment = self.tokens.first().filter(|t| t.kind == K::Comment).map(|t| t.range.start);
            let comment_only = first.is_some() && first == leading_comment;
            let in_block = start.outer.mode != code::NORMAL;
            if let Some(f) = first.filter(|_| !comment_only && !in_block) {
                let width = line[..f].bytes().map(|c| if c == b'\t' { 4 } else { 1 }).sum::<usize>();
                while stack.last().is_some_and(|&(w, _)| w >= width) {
                    let (_, s) = stack.pop().expect("stack checked non-empty");
                    self.add(s, last_content);
                }
                stack.push((width, i as u64));
                last_content = i as u64;
            } else if first.is_some() {
                last_content = i as u64;
            }
        }
        for (_, s) in stack {
            self.add(s, last_content);
        }
    }

    fn tags<'a>(&mut self, lines: impl Iterator<Item = &'a str>, html: bool) {
        let mut stack: Vec<(String, u64)> = Vec::new();
        // The top element's start tag has not been closed by `>` or `/>` yet.
        let mut pending = false;
        for (i, line) in lines.enumerate() {
            self.lex(line);
            let b = line.as_bytes();
            let marks: Vec<usize> = structural(line, &self.tokens)
                .filter(|&(p, c)| c == b'<' || c == b'>' || (c == b'/' && b.get(p + 1) == Some(&b'>')))
                .map(|(p, _)| p)
                .collect();
            for p in marks {
                let closing = b.get(p + 1) == Some(&b'/');
                let name_at = (p + 1 + usize::from(closing)).min(b.len());
                let name_len = b[name_at..].iter().take_while(|&&c| markup::is_name(c)).count();
                let mut name = line[name_at..name_at + name_len].to_string();
                if html {
                    name.make_ascii_lowercase();
                }
                let opens = b[p] == b'<' && !closing && name_len > 0 && markup::is_name_start(b[name_at]);
                let pushes = opens && !(html && HTML_VOID.contains(&name.as_str()));
                if b[p] == b'/' && pending {
                    stack.pop();
                } else if b[p] == b'<' && closing && name_len > 0 {
                    if let Some(at) = stack.iter().rposition(|(n, _)| *n == name) {
                        let s = stack[at].1;
                        stack.truncate(at);
                        self.add(s, i as u64);
                    }
                } else if pushes {
                    stack.push((name, i as u64));
                }
                pending = pushes;
            }
        }
    }

    fn headings<'a>(&mut self, lines: impl Iterator<Item = &'a str>) {
        let mut stack: Vec<(usize, u64)> = Vec::new();
        let mut block_start = None;
        let mut last_content = 0u64;
        let is_text = |m: u8| m == markdown::START || m == markdown::NORMAL;
        for (i, line) in lines.enumerate() {
            let i = i as u64;
            let start = self.lex(line);
            let was_text = is_text(start.outer.mode);
            let now_text = is_text(self.state.outer.mode);
            let level = line.bytes().take_while(|&c| c == b'#').count();
            let heading_token = self.tokens.first().is_some_and(|t| t.kind == K::Heading);
            if was_text && heading_token && (1..=6).contains(&level) {
                while stack.last().is_some_and(|&(l, _)| l >= level) {
                    let (_, s) = stack.pop().expect("stack checked non-empty");
                    self.add(s, last_content);
                }
                stack.push((level, i));
            }
            match (was_text, now_text) {
                (true, false) => block_start = Some(i),
                (false, true) => {
                    if let Some(s) = block_start.take() {
                        self.add(s, i);
                    }
                }
                _ => {}
            }
            if !line.trim().is_empty() {
                last_content = i;
            }
        }
        for (_, s) in stack {
            self.add(s, last_content);
        }
    }

    fn sections<'a>(&mut self, lines: impl Iterator<Item = &'a str>) {
        let mut open: Option<u64> = None;
        let mut last_content = 0u64;
        for (i, line) in lines.enumerate() {
            let i = i as u64;
            let start = self.lex(line);
            let header = start.outer.mode == code::NORMAL && line.trim_start().starts_with('[');
            if header {
                if let Some(s) = open.replace(i) {
                    self.add(s, last_content);
                }
            }
            if !line.trim().is_empty() {
                last_content = i;
            }
        }
        if let Some(s) = open {
            self.add(s, last_content);
        }
    }
}
