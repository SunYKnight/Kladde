//! Table-driven lexer for programming and configuration languages.

use crate::{char_len, push, Sub, Token, TokenKind as K};

/// A string delimiter pair.
pub(crate) struct Str {
    pub open: &'static str,
    pub close: &'static str,
    /// Escape byte, 0 for none.
    pub escape: u8,
    pub multiline: bool,
    /// `$name` inside the string is highlighted as a variable.
    pub interp: bool,
    /// The closer only counts at the very start of a line (PowerShell here-strings).
    pub line_start_close: bool,
}

pub(crate) const fn st(open: &'static str, close: &'static str, escape: u8, multiline: bool) -> Str {
    Str { open, close, escape, multiline, interp: false, line_start_close: false }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vars {
    None,
    Shell,
    PowerShell,
    Batch,
    Make,
    CMake,
    Sql,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    Generic,
    C,
    Cpp,
    Rust,
    Python,
    Json,
    Css,
    Ini,
    Toml,
    Yaml,
    Batch,
    Make,
    PowerShell,
}

pub(crate) struct CodeSpec {
    pub line_comments: &'static [&'static str],
    pub block_comments: &'static [(&'static str, &'static str)],
    pub nested_comments: bool,
    /// Longer openers must come before their prefixes (`"""` before `"`).
    pub strings: &'static [Str],
    pub keywords: &'static [&'static str],
    pub types: &'static [&'static str],
    pub constants: &'static [&'static str],
    /// Keyword lists are lowercase and match case-insensitively.
    pub ignore_case: bool,
    /// Line comments only start at line start or after whitespace (`#` in shell, YAML, INI).
    pub comment_needs_space: bool,
    /// Quotes only open strings when not preceded by an alphanumeric (`it's` in YAML values).
    pub quote_needs_boundary: bool,
    /// `CamelCase` identifiers are types, `UPPER_CASE` ones constants.
    pub upper_types: bool,
    /// `@name` is an annotation or decorator.
    pub annotations: bool,
    /// `-` joins letters inside identifiers (`Get-ChildItem`, CSS properties).
    pub ident_dash: bool,
    /// Extra identifier bytes such as `$` in JavaScript.
    pub ident_extra: &'static [u8],
    /// `#` at the first non-blank column starts a directive.
    pub preprocessor: bool,
    pub vars: Vars,
    pub flavor: Flavor,
}

pub(crate) const BASE: CodeSpec = CodeSpec {
    line_comments: &[],
    block_comments: &[],
    nested_comments: false,
    strings: &[],
    keywords: &[],
    types: &[],
    constants: &[],
    ignore_case: false,
    comment_needs_space: false,
    quote_needs_boundary: false,
    upper_types: false,
    annotations: false,
    ident_dash: false,
    ident_extra: &[],
    preprocessor: false,
    vars: Vars::None,
    flavor: Flavor::Generic,
};

pub(crate) const NORMAL: u8 = 0;
const BLOCK: u8 = 1; // aux = block comment index, depth = nesting
const STRING: u8 = 2; // aux = string index, depth 1 = raw (no escapes)
const RUST_RAW: u8 = 3; // depth = number of `#`
const CPP_RAW: u8 = 4; // depth = delimiter length, aux = delimiter hash

const OPERATORS: &[u8] = b"+-*/%=<>!&|^~?:";
const PS_OPERATORS: &[&str] = &[
    "and",
    "as",
    "band",
    "bnot",
    "bor",
    "bxor",
    "ceq",
    "cge",
    "cgt",
    "cle",
    "clike",
    "clt",
    "cmatch",
    "cne",
    "contains",
    "creplace",
    "csplit",
    "eq",
    "f",
    "ge",
    "gt",
    "ieq",
    "ilike",
    "imatch",
    "in",
    "ine",
    "ireplace",
    "is",
    "isnot",
    "isplit",
    "join",
    "le",
    "like",
    "lt",
    "match",
    "ne",
    "not",
    "notcontains",
    "notin",
    "notlike",
    "notmatch",
    "or",
    "replace",
    "shl",
    "shr",
    "split",
    "xor",
];
const _: () = assert!(crate::langs::sorted(PS_OPERATORS));

pub(crate) fn lex(spec: &'static CodeSpec, sub: Sub, line: &str, out: &mut Vec<Token>) -> Sub {
    let b = line.as_bytes();
    let first = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let mut lx = Lx { spec, s: line, b, pos: 0, first, sub, out };
    if lx.sub.mode == NORMAL {
        lx.line_start();
    }
    while lx.pos < b.len() {
        if lx.sub.mode == NORMAL {
            lx.normal();
        } else {
            lx.resume();
        }
    }
    lx.end_of_line()
}

fn hash(bytes: &[u8]) -> u16 {
    bytes.iter().fold(7u16, |h, &c| h.wrapping_mul(31).wrapping_add(u16::from(c)))
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Looks `word` up in a byte-sorted list; case-insensitive lists are lowercase.
fn matches(list: &[&str], word: &str, ignore_case: bool) -> bool {
    let found = if ignore_case {
        list.binary_search_by(|k| k.bytes().cmp(word.bytes().map(|c| c.to_ascii_lowercase())))
    } else {
        list.binary_search(&word)
    };
    found.is_ok()
}

struct Lx<'a> {
    spec: &'static CodeSpec,
    s: &'a str,
    b: &'a [u8],
    pos: usize,
    first: usize,
    sub: Sub,
    out: &'a mut Vec<Token>,
}

impl Lx<'_> {
    fn at(&self, p: usize) -> u8 {
        self.b.get(p).copied().unwrap_or(0)
    }

    fn emit(&mut self, start: usize, end: usize, kind: K) {
        push(self.out, start, end, kind);
    }

    fn set_mode(&mut self, mode: u8, depth: u8, aux: u16) {
        self.sub = Sub { mode, depth, aux, nest: self.sub.nest };
    }

    fn ident_start(&self, c: u8) -> bool {
        c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || self.spec.ident_extra.contains(&c)
    }

    fn ident_cont(&self, p: usize) -> bool {
        let c = self.at(p);
        let dash = self.spec.ident_dash
            && c == b'-'
            && p > 0
            && self.at(p - 1).is_ascii_alphanumeric()
            && self.at(p + 1).is_ascii_alphabetic();
        is_word(c) || self.spec.ident_extra.contains(&c) || dash
    }

    fn ident_end(&self, from: usize) -> usize {
        let mut p = from;
        while p < self.b.len() && self.ident_cont(p) {
            p += 1;
        }
        p
    }

    fn skip_blanks(&self, from: usize) -> usize {
        let mut p = from;
        while p < self.b.len() && (self.b[p] == b' ' || self.b[p] == b'\t') {
            p += 1;
        }
        p
    }

    fn prev_is_space(&self) -> bool {
        self.pos == 0 || self.b[self.pos - 1].is_ascii_whitespace()
    }

    /// End of an escape sequence starting at the escape byte `p`.
    fn escape_end(&self, p: usize) -> usize {
        let len = self.b.len();
        let next = self.at(p + 1);
        let hex_run = |from: usize, max: usize| {
            let mut q = from;
            while q < len && q - from < max && self.b[q].is_ascii_hexdigit() {
                q += 1;
            }
            q
        };
        match next {
            0 if p + 1 >= len => p + 1,
            b'x' if self.b[p] == b'\\' => hex_run(p + 2, 2),
            b'u' if self.b[p] == b'\\' && self.at(p + 2) == b'{' => {
                let q = hex_run(p + 3, 8);
                q + usize::from(self.at(q) == b'}')
            }
            b'u' if self.b[p] == b'\\' => hex_run(p + 2, 4),
            b'U' if self.b[p] == b'\\' => hex_run(p + 2, 8),
            b'0'..=b'7' if self.b[p] == b'\\' => {
                let mut q = p + 1;
                while q < len && q - p <= 3 && (b'0'..=b'7').contains(&self.b[q]) {
                    q += 1;
                }
                q
            }
            c => p + 1 + char_len(c),
        }
    }

    // ---- line start hooks -------------------------------------------------------------------------------------

    fn line_start(&mut self) {
        match self.spec.flavor {
            Flavor::Ini => self.ini_line(),
            Flavor::Toml => self.toml_line(),
            Flavor::Yaml => self.yaml_line(),
            Flavor::Batch => self.batch_line(),
            Flavor::Make => self.make_line(),
            _ if self.spec.preprocessor && self.at(self.first) == b'#' => self.directive(),
            _ => {}
        }
    }

    fn directive(&mut self) {
        let word_start = self.skip_blanks(self.first + 1);
        let word_end = self.ident_end(word_start);
        self.emit(self.first, word_end, K::Preprocessor);
        self.pos = word_end;
        let word = &self.s[word_start..word_end];
        let target = self.skip_blanks(word_end);
        if (word == "include" || word == "import") && self.at(target) == b'<' {
            let close = self.b[target..].iter().position(|&c| c == b'>');
            let end = close.map_or(self.b.len(), |c| target + c + 1);
            self.emit(target, end, K::String);
            self.pos = end;
        }
    }

    fn section_header(&mut self) {
        let close = self.b[self.first..].iter().position(|&c| c == b']');
        let end = close.map_or(self.b.len(), |c| {
            let at = self.first + c;
            at + self.b[at..].iter().take_while(|&&x| x == b']').count()
        });
        self.emit(self.first, end, K::Heading);
        self.pos = end;
    }

    fn ini_line(&mut self) {
        let c = self.at(self.first);
        let key_end = self.b.iter().position(|&c| c == b'=' || c == b':');
        match key_end {
            _ if c == b'[' => self.section_header(),
            _ if c == b';' || c == b'#' => {}
            Some(eq) if eq > self.first => {
                let end = self.first + self.s[self.first..eq].trim_end().len();
                self.emit(self.first, end, K::Attribute);
                self.pos = eq;
            }
            _ => {}
        }
    }

    fn toml_line(&mut self) {
        let eq = self.b.iter().position(|&c| c == b'=');
        match eq {
            _ if self.at(self.first) == b'[' => self.section_header(),
            Some(eq) if eq > self.first => {
                let key = &self.b[self.first..eq];
                let quotes = key.iter().filter(|&&c| c == b'"' || c == b'\'').count();
                let bare = key.iter().all(|&c| is_word(c) || b"-. \t\"'".contains(&c));
                if bare && quotes % 2 == 0 {
                    let end = self.first + self.s[self.first..eq].trim_end().len();
                    self.emit(self.first, end, K::Attribute);
                    self.pos = eq;
                }
            }
            _ => {}
        }
    }

    fn yaml_line(&mut self) {
        let len = self.b.len();
        let doc_marker = self.s.starts_with("---") || self.s.starts_with("...");
        if doc_marker && (len == 3 || self.b[3].is_ascii_whitespace()) {
            self.emit(0, 3, K::Preprocessor);
            self.pos = 3;
        } else if self.at(0) == b'%' {
            self.emit(0, len, K::Preprocessor);
            self.pos = len;
        } else {
            let mut p = self.first;
            while self.at(p) == b'-' && (p + 1 >= len || self.b[p + 1] == b' ') {
                self.emit(p, p + 1, K::Operator);
                p = self.skip_blanks(p + 1);
            }
            self.pos = p;
            self.yaml_key(p);
        }
    }

    fn yaml_key(&mut self, p: usize) {
        let len = self.b.len();
        let colon_ok = |q: usize| self.at(q) == b':' && (q + 1 >= len || self.b[q + 1].is_ascii_whitespace());
        let c = self.at(p);
        if c == b'"' || c == b'\'' {
            let close = self.b[p + 1..].iter().position(|&x| x == c).map(|q| p + 1 + q + 1);
            if let Some(end) = close.filter(|&e| colon_ok(self.skip_blanks(e))) {
                self.emit(p, end, K::Attribute);
                self.pos = end;
            }
        } else if p < len && !b"[{#&*!|>%@`".contains(&c) {
            let mut q = p;
            let mut found = None;
            while q < len && found.is_none() && !(self.b[q] == b'#' && self.b[q - 1].is_ascii_whitespace()) {
                found = colon_ok(q).then_some(q);
                q += 1;
            }
            if let Some(colon) = found {
                let end = p + self.s[p..colon].trim_end().len();
                self.emit(p, end, K::Attribute);
                self.pos = colon;
            }
        }
    }

    fn batch_line(&mut self) {
        let mut p = self.first;
        if self.at(p) == b'@' {
            self.emit(p, p + 1, K::Operator);
            p += 1;
        }
        let rest = &self.s[p..];
        let rem = rest.len() >= 3
            && rest.as_bytes()[..3].eq_ignore_ascii_case(b"rem")
            && (rest.len() == 3 || rest.as_bytes()[3].is_ascii_whitespace() || rest.as_bytes()[3] == b'.');
        let len = self.b.len();
        if rem || rest.starts_with("::") {
            self.emit(p, len, K::Comment);
            self.pos = len;
        } else if rest.starts_with(':') {
            let end = self.b[p..].iter().position(|c| c.is_ascii_whitespace()).map_or(len, |q| p + q);
            self.emit(p, end, K::Function);
            self.pos = end;
        } else {
            self.pos = p;
        }
    }

    fn make_line(&mut self) {
        let word_end = self.ident_end(0);
        let keyword = matches(self.spec.keywords, &self.s[..word_end], false);
        let sep = self.b.iter().position(|&c| c == b':' || c == b'=' || c == b'#');
        match sep {
            _ if self.first != 0 || keyword => {}
            Some(q) if self.b[q] == b'=' || (self.b[q] == b':' && self.at(q + 1) == b'=') => {
                let name = self.s[..q].trim_end_matches(['?', '+', ':', '!', ' ', '\t']);
                self.emit(0, name.len(), K::Variable);
                self.pos = name.len();
            }
            Some(q) if self.b[q] == b':' => {
                let name = self.s[..q].trim_end();
                self.emit(0, name.len(), K::Function);
                self.pos = name.len();
            }
            _ => {}
        }
    }

    // ---- multi-line constructs --------------------------------------------------------------------------------

    fn resume(&mut self) {
        match self.sub.mode {
            BLOCK => self.block_comment(),
            STRING => self.string_body(),
            RUST_RAW => self.rust_raw_body(),
            CPP_RAW => self.cpp_raw_body(),
            _ => self.set_mode(NORMAL, 0, 0),
        }
    }

    fn block_comment(&mut self) {
        match self.spec.block_comments.get(usize::from(self.sub.aux)) {
            Some(&(open, close)) => {
                let start = self.pos;
                let mut p = self.pos;
                let mut depth = self.sub.depth.max(1);
                while p < self.b.len() && depth > 0 {
                    if self.s[p..].starts_with(close) {
                        depth -= 1;
                        p += close.len();
                    } else if self.spec.nested_comments && self.s[p..].starts_with(open) {
                        depth = depth.saturating_add(1);
                        p += open.len();
                    } else {
                        p += char_len(self.b[p]);
                    }
                }
                self.emit(start, p, K::Comment);
                self.pos = p;
                if depth == 0 {
                    self.set_mode(NORMAL, 0, 0);
                } else {
                    self.sub.depth = depth;
                }
            }
            None => self.set_mode(NORMAL, 0, 0),
        }
    }

    fn string_body(&mut self) {
        match self.spec.strings.get(usize::from(self.sub.aux)) {
            Some(st) if st.line_start_close && self.pos == 0 && self.s.starts_with(st.close) => {
                self.emit(0, st.close.len(), K::String);
                self.pos = st.close.len();
                self.set_mode(NORMAL, 0, 0);
            }
            Some(st) => {
                let escape = if self.sub.depth & 1 == 1 { 0 } else { st.escape };
                if self.scan_string(st, escape) {
                    self.set_mode(NORMAL, 0, 0);
                }
            }
            None => self.set_mode(NORMAL, 0, 0),
        }
    }

    /// Scans string content from `pos`; true when the closer was consumed.
    fn scan_string(&mut self, st: &Str, escape: u8) -> bool {
        let len = self.b.len();
        let mut p = self.pos;
        let mut seg = p;
        let mut closed = false;
        while p < len && !closed {
            let c = self.b[p];
            let var = if st.interp && c == b'$' { self.var_len(p) } else { 0 };
            if escape != 0 && c == escape {
                let end = self.escape_end(p).min(len);
                self.emit(seg, p, K::String);
                self.emit(p, end, K::Escape);
                p = end;
                seg = p;
            } else if !st.line_start_close && self.s[p..].starts_with(st.close) {
                p += st.close.len();
                closed = true;
            } else if var > 0 {
                self.emit(seg, p, K::String);
                self.emit(p, p + var, K::Variable);
                p += var;
                seg = p;
            } else {
                p += char_len(c);
            }
        }
        self.emit(seg, p, K::String);
        self.pos = p;
        closed
    }

    fn rust_raw_body(&mut self) {
        let hashes = usize::from(self.sub.depth);
        let len = self.b.len();
        let closes =
            |p: usize| self.b[p] == b'"' && p + hashes < len && self.b[p + 1..=p + hashes].iter().all(|&c| c == b'#');
        let found = (self.pos..len).find(|&p| closes(p));
        let end = found.map_or(len, |p| p + 1 + hashes);
        self.emit(self.pos, end, K::String);
        self.pos = end;
        if found.is_some() {
            self.set_mode(NORMAL, 0, 0);
        }
    }

    fn cpp_raw_body(&mut self) {
        let dlen = usize::from(self.sub.depth);
        let len = self.b.len();
        let aux = self.sub.aux;
        let closes = |p: usize| {
            let delim_end = p + 1 + dlen;
            self.b[p] == b')' && delim_end < len && self.b[delim_end] == b'"' && hash(&self.b[p + 1..delim_end]) == aux
        };
        let found = (self.pos..len).find(|&p| closes(p));
        let end = found.map_or(len, |p| p + 2 + dlen);
        self.emit(self.pos, end, K::String);
        self.pos = end;
        if found.is_some() {
            self.set_mode(NORMAL, 0, 0);
        }
    }

    fn end_of_line(&self) -> Sub {
        let single_line =
            self.sub.mode == STRING && self.spec.strings.get(usize::from(self.sub.aux)).is_none_or(|st| !st.multiline);
        if single_line || self.sub.mode == NORMAL {
            Sub { nest: self.sub.nest, ..Sub::default() }
        } else {
            self.sub
        }
    }

    // ---- normal mode ------------------------------------------------------------------------------------------

    fn normal(&mut self) {
        let handled = self.try_blanks()
            || self.try_comment()
            || self.try_flavor()
            || self.try_string()
            || self.try_var()
            || self.try_number()
            || self.try_ident()
            || self.try_annotation()
            || self.try_operator();
        if !handled {
            self.pos += char_len(self.b[self.pos]);
        }
    }

    fn try_blanks(&mut self) -> bool {
        let end = self.skip_blanks(self.pos);
        let moved = end > self.pos;
        self.pos = end;
        moved
    }

    fn try_comment(&mut self) -> bool {
        let rest = &self.s[self.pos..];
        let block = self.spec.block_comments.iter().position(|(open, _)| rest.starts_with(open));
        match block {
            Some(i) => {
                let end = self.pos + self.spec.block_comments[i].0.len();
                self.emit(self.pos, end, K::Comment);
                self.pos = end;
                self.set_mode(BLOCK, 1, i as u16);
                true
            }
            None => {
                let line = self.spec.line_comments.iter().any(|c| rest.starts_with(c))
                    && (!self.spec.comment_needs_space || self.prev_is_space());
                if line {
                    self.emit(self.pos, self.b.len(), K::Comment);
                    self.pos = self.b.len();
                }
                line
            }
        }
    }

    fn try_string(&mut self) -> bool {
        let rest = &self.s[self.pos..];
        let boundary = !self.spec.quote_needs_boundary || self.pos == 0 || !is_word(self.b[self.pos - 1]);
        let found = self.spec.strings.iter().position(|st| rest.starts_with(st.open));
        match found {
            Some(i) if boundary => {
                self.open_string(self.pos, i, false);
                true
            }
            _ => false,
        }
    }

    /// Opens string `i` whose opener sits at `pos`; `start` may be earlier to include a prefix.
    fn open_string(&mut self, start: usize, i: usize, raw: bool) {
        let end = self.pos + self.spec.strings[i].open.len();
        self.emit(start, end, K::String);
        self.pos = end;
        self.set_mode(STRING, u8::from(raw), i as u16);
    }

    fn string_at(&self, p: usize) -> Option<usize> {
        let rest = &self.s[p..];
        self.spec.strings.iter().position(|st| rest.starts_with(st.open))
    }

    fn try_flavor(&mut self) -> bool {
        let c = self.b[self.pos];
        match self.spec.flavor {
            Flavor::Rust if c == b'#' && (self.at(self.pos + 1) == b'[' || self.s[self.pos..].starts_with("#![")) => {
                self.rust_attribute();
                true
            }
            Flavor::Rust if c == b'\'' => {
                self.rust_quote(self.pos);
                true
            }
            Flavor::Json if c == b'"' => self.json_key(),
            Flavor::Css => self.css(c),
            Flavor::PowerShell => self.powershell(c),
            Flavor::Batch if c == b'^' => {
                let end = self.escape_end(self.pos).min(self.b.len());
                self.emit(self.pos, end, K::Escape);
                self.pos = end;
                true
            }
            _ => false,
        }
    }

    fn rust_attribute(&mut self) {
        let len = self.b.len();
        let mut p = self.pos + 1;
        let mut depth = 0u32;
        let mut done = false;
        while p < len && !done {
            match self.b[p] {
                b'[' => depth += 1,
                b']' => {
                    depth = depth.saturating_sub(1);
                    done = depth == 0;
                }
                _ => {}
            }
            p += 1;
        }
        self.emit(self.pos, p, K::Preprocessor);
        self.pos = p;
    }

    /// Char literal or lifetime; `start` may include a `b` prefix before the quote at `pos`.
    fn rust_quote(&mut self, start: usize) {
        let p = self.pos + 1;
        let c = self.at(p);
        let char_end = if c == b'\\' { self.escape_end(p) } else { p + char_len(c) };
        if p < self.b.len() && self.at(char_end) == b'\'' {
            self.emit(start, p, K::String);
            if c == b'\\' {
                self.emit(p, char_end, K::Escape);
            }
            self.emit(if c == b'\\' { char_end } else { p }, char_end + 1, K::String);
            self.pos = char_end + 1;
        } else if self.ident_start(c) {
            let end = self.ident_end(p);
            self.emit(self.pos, end, K::Type);
            self.pos = end;
        } else {
            self.pos = p;
        }
    }

    fn json_key(&mut self) -> bool {
        let len = self.b.len();
        let mut p = self.pos + 1;
        while p < len && self.b[p] != b'"' {
            p += if self.b[p] == b'\\' { 2 } else { 1 };
        }
        let is_key = p < len && self.at(self.skip_blanks(p + 1)) == b':';
        if is_key {
            self.emit(self.pos, p + 1, K::Attribute);
            self.pos = p + 1;
        }
        is_key
    }

    fn css(&mut self, c: u8) -> bool {
        let start = self.pos;
        let word_end = |lx: &Self, from: usize| {
            let mut q = from;
            while q < lx.b.len() && (is_word(lx.b[q]) || lx.b[q] == b'-') {
                q += 1;
            }
            q
        };
        let selector = self.sub.nest == 0;
        let next = self.at(start + 1);
        let kind = match c {
            b'{' => {
                self.sub.nest = self.sub.nest.saturating_add(1);
                None
            }
            b'}' => {
                self.sub.nest = self.sub.nest.saturating_sub(1);
                None
            }
            b'@' if next.is_ascii_alphabetic() => Some(K::Keyword),
            b'!' if next.is_ascii_alphabetic() => Some(K::Keyword),
            b'#' if is_word(next) || next == b'-' => Some(if selector { K::Constant } else { K::Number }),
            b'.' if selector && (next.is_ascii_alphabetic() || next == b'_' || next == b'-') => Some(K::Type),
            b':' if selector && (next.is_ascii_alphabetic() || next == b':') => Some(K::Keyword),
            _ => None,
        };
        match kind {
            Some(k) => {
                let from = start + 1 + usize::from(c == b':' && next == b':');
                let end = word_end(self, from);
                self.emit(start, end, k);
                self.pos = end;
                true
            }
            None if c == b'{' || c == b'}' => {
                self.pos += 1;
                true
            }
            None => false,
        }
    }

    fn powershell(&mut self, c: u8) -> bool {
        let start = self.pos;
        let prev = if start > 0 { self.b[start - 1] } else { b' ' };
        let next = self.at(start + 1);
        match c {
            b'-' if next.is_ascii_alphabetic() && !is_word(prev) => {
                let mut end = start + 1;
                while end < self.b.len() && is_word(self.b[end]) {
                    end += 1;
                }
                let op = matches(PS_OPERATORS, &self.s[start + 1..end], true);
                self.emit(start, end, if op { K::Operator } else { K::Attribute });
                self.pos = end;
                true
            }
            b'[' if next.is_ascii_alphabetic() && !is_word(prev) && prev != b']' && prev != b')' => {
                let mut end = start + 1;
                while end < self.b.len() && (is_word(self.b[end]) || self.b[end] == b'.') {
                    end += 1;
                }
                while self.s[end..].starts_with("[]") {
                    end += 2;
                }
                let is_type = self.at(end) == b']';
                if is_type {
                    self.emit(start, end + 1, K::Type);
                    self.pos = end + 1;
                }
                is_type
            }
            b'`' => {
                let end = self.escape_end(start).min(self.b.len());
                self.emit(start, end, K::Escape);
                self.pos = end;
                true
            }
            _ => false,
        }
    }

    // ---- variables --------------------------------------------------------------------------------------------

    /// Length of the variable reference starting at `p`, 0 if there is none.
    fn var_len(&self, p: usize) -> usize {
        let c = self.at(p);
        let next = self.at(p + 1);
        let word_run = |from: usize| {
            let mut q = from;
            while q < self.b.len() && is_word(self.b[q]) {
                q += 1;
            }
            q
        };
        let braced = |from: usize, close: u8| {
            let q = word_run(from);
            q + usize::from(self.at(q) == close) - p
        };
        match (self.spec.vars, c) {
            (Vars::Shell | Vars::PowerShell, b'$') if next == b'{' => braced(p + 2, b'}'),
            (Vars::Shell | Vars::PowerShell, b'$') if is_word(next) => {
                let end = word_run(p + 1);
                let scoped = self.spec.vars == Vars::PowerShell && self.at(end) == b':' && is_word(self.at(end + 1));
                if scoped {
                    word_run(end + 1) - p
                } else {
                    end - p
                }
            }
            (Vars::Shell, b'$') if b"@*#?-$!".contains(&next) && next != 0 => 2,
            (Vars::PowerShell, b'$') if b"$?^_".contains(&next) && next != 0 => 2,
            (Vars::Make, b'$') if next == b'(' => braced(p + 2, b')'),
            (Vars::Make, b'$') if next == b'{' => braced(p + 2, b'}'),
            (Vars::Make, b'$') if next.is_ascii_graphic() => 2,
            (Vars::CMake, b'$') if next == b'{' => braced(p + 2, b'}'),
            (Vars::CMake, b'$') if self.s[p + 1..].starts_with("ENV{") => braced(p + 5, b'}'),
            (Vars::Sql, b'@') if next == b'@' || is_word(next) => word_run(p + 1 + usize::from(next == b'@')) - p,
            (Vars::Batch, b'%') if next == b'%' && is_word(self.at(p + 2)) => 3,
            (Vars::Batch, b'%') if next == b'~' => word_run(p + 2) - p,
            (Vars::Batch, b'%') if next.is_ascii_digit() || next == b'*' => 2,
            (Vars::Batch, b'%' | b'!') => self.batch_var(p, c),
            _ => 0,
        }
    }

    fn batch_var(&self, p: usize, close: u8) -> usize {
        let limit = self.b.len().min(p + 128);
        let allowed = |x: u8| is_word(x) || (close == b'%' && b":~,=-.$# ".contains(&x));
        let mut q = p + 1;
        while q < limit && allowed(self.b[q]) {
            q += 1;
        }
        if q > p + 1 && q < limit && self.b[q] == close && self.b[p + 1] != b' ' {
            q + 1 - p
        } else {
            0
        }
    }

    fn try_var(&mut self) -> bool {
        let len = self.var_len(self.pos);
        if len > 0 {
            let start = self.pos;
            let name = self.s[start + 1..start + len].trim_matches(['{', '}']);
            let constant = self.spec.vars == Vars::PowerShell && matches(&["false", "null", "true"], name, true);
            self.emit(start, start + len, if constant { K::Constant } else { K::Variable });
            self.pos += len;
        }
        len > 0
    }

    // ---- numbers, identifiers, operators ----------------------------------------------------------------------

    fn try_number(&mut self) -> bool {
        let c = self.b[self.pos];
        let start = self.pos;
        let is_num = c.is_ascii_digit() || (c == b'.' && self.at(start + 1).is_ascii_digit());
        if is_num {
            let hex = c == b'0' && matches!(self.at(start + 1), b'x' | b'X');
            let mut seen_dot = c == b'.';
            let mut p = start + 1;
            let more = |lx: &Self, q: usize, seen_dot: bool| {
                let x = lx.b[q];
                let exp_sign = (x == b'+' || x == b'-') && !hex && matches!(lx.b[q - 1], b'e' | b'E');
                let dot = x == b'.' && !seen_dot && !hex && lx.at(q + 1).is_ascii_digit();
                x.is_ascii_alphanumeric() || x == b'_' || exp_sign || dot
            };
            while p < self.b.len() && more(self, p, seen_dot) {
                seen_dot |= self.b[p] == b'.';
                p += 1;
            }
            self.emit(start, p, K::Number);
            self.pos = p;
        }
        is_num
    }

    fn try_ident(&mut self) -> bool {
        let c = self.b[self.pos];
        let is_ident = self.ident_start(c);
        if is_ident {
            let start = self.pos;
            let end = self.ident_end(start + 1);
            self.pos = end;
            if !self.string_prefix(start, end) {
                self.classify(start, end);
            }
        }
        is_ident
    }

    /// Handles string prefixes like `r"`, `b'`, `R"(`; true when a string was opened.
    fn string_prefix(&mut self, start: usize, end: usize) -> bool {
        let word = &self.s[start..end];
        let next = self.at(end);
        let quoted = next == b'"' || next == b'\'';
        match self.spec.flavor {
            Flavor::Rust if matches!(word, "r" | "br" | "cr") && (next == b'#' || next == b'"') => {
                let hashes = self.b[end..].iter().take_while(|&&x| x == b'#').count();
                let raw = self.at(end + hashes) == b'"' && hashes < 256;
                if raw {
                    self.emit(start, end + hashes + 1, K::String);
                    self.pos = end + hashes + 1;
                    self.set_mode(RUST_RAW, hashes as u8, 0);
                }
                raw
            }
            Flavor::Rust if word == "b" && next == b'\'' => {
                self.rust_quote(start);
                true
            }
            Flavor::Rust if matches!(word, "b" | "c") && next == b'"' => self.prefixed(start, end, false),
            Flavor::Python if quoted && word.len() <= 2 && word.bytes().all(|x| b"rRuUbBfFtT".contains(&x)) => {
                self.prefixed(start, end, word.contains(['r', 'R']))
            }
            Flavor::Cpp if next == b'"' && matches!(word, "R" | "u8R" | "uR" | "UR" | "LR") => self.cpp_raw(start, end),
            Flavor::C | Flavor::Cpp if quoted && matches!(word, "L" | "u" | "U" | "u8") => {
                self.prefixed(start, end, false)
            }
            _ => false,
        }
    }

    fn prefixed(&mut self, start: usize, end: usize, raw: bool) -> bool {
        match self.string_at(end) {
            Some(i) => {
                self.open_string(start, i, raw);
                true
            }
            None => false,
        }
    }

    fn cpp_raw(&mut self, start: usize, end: usize) -> bool {
        let delim_start = end + 1;
        let valid = |x: u8| x.is_ascii_graphic() && !b"()\\\"".contains(&x);
        let dlen = self.b[delim_start..].iter().take_while(|&&x| valid(x)).count();
        let is_raw = dlen <= 16 && self.at(delim_start + dlen) == b'(';
        if is_raw {
            let open_end = delim_start + dlen + 1;
            let delim_hash = hash(&self.b[delim_start..delim_start + dlen]);
            self.emit(start, open_end, K::String);
            self.pos = open_end;
            self.set_mode(CPP_RAW, dlen as u8, delim_hash);
        }
        is_raw
    }

    fn classify(&mut self, start: usize, end: usize) {
        let spec = self.spec;
        let word = &self.s[start..end];
        let ic = spec.ignore_case;
        let call = self.at(self.skip_blanks(end)) == b'(';
        let first_upper = word.as_bytes()[0].is_ascii_uppercase();
        let upper = |x: u8| x.is_ascii_uppercase() || x.is_ascii_digit() || x == b'_';
        let all_upper = word.len() > 1 && word.bytes().all(upper);
        let macro_call = spec.flavor == Flavor::Rust && self.at(end) == b'!' && self.at(end + 1) != b'=';
        let kind = if matches(spec.keywords, word, ic) {
            Some(K::Keyword)
        } else if matches(spec.types, word, ic) {
            Some(K::Type)
        } else if matches(spec.constants, word, ic) {
            Some(K::Constant)
        } else if macro_call {
            self.pos = end + 1;
            Some(K::Function)
        } else if call {
            Some(K::Function)
        } else if spec.flavor == Flavor::Css && self.sub.nest == 0 {
            Some(K::Tag)
        } else if spec.flavor == Flavor::Css && self.at(self.skip_blanks(end)) == b':' {
            Some(K::Attribute)
        } else if spec.upper_types && all_upper {
            Some(K::Constant)
        } else if spec.upper_types && first_upper {
            Some(K::Type)
        } else if spec.flavor == Flavor::PowerShell && word.contains('-') {
            Some(K::Function)
        } else {
            None
        };
        if let Some(k) = kind {
            self.emit(start, self.pos, k);
        }
    }

    fn try_annotation(&mut self) -> bool {
        let p = self.pos;
        let is_annotation = self.spec.annotations && self.b[p] == b'@' && self.ident_start(self.at(p + 1));
        if is_annotation {
            let mut end = self.ident_end(p + 1);
            while self.at(end) == b'.' && self.ident_start(self.at(end + 1)) {
                end = self.ident_end(end + 1);
            }
            self.emit(p, end, K::Preprocessor);
            self.pos = end;
        }
        is_annotation
    }

    fn try_operator(&mut self) -> bool {
        let is_op = OPERATORS.contains(&self.b[self.pos]);
        if is_op {
            self.emit(self.pos, self.pos + 1, K::Operator);
            self.pos += 1;
        }
        is_op
    }
}
