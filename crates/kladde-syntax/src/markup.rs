//! XML/HTML lexer.

use crate::{char_len, push, Sub, Token, TokenKind as K};

const TEXT: u8 = 0;
const TAG: u8 = 1;
const COMMENT: u8 = 2;
const CDATA: u8 = 3;
const PI: u8 = 4;
const DECL: u8 = 5;
const ATTR_STRING: u8 = 6; // aux = quote byte

pub(crate) fn lex(sub: Sub, line: &str, out: &mut Vec<Token>) -> Sub {
    let mut x = Xml { s: line, b: line.as_bytes(), pos: 0, mode: sub.mode, aux: sub.aux, after_eq: false, out };
    while x.pos < x.b.len() {
        x.step();
    }
    Sub { mode: x.mode, aux: x.aux, ..Sub::default() }
}

pub(crate) fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b':' || c >= 0x80
}

pub(crate) fn is_name(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_:.-".contains(&c) || c >= 0x80
}

struct Xml<'a> {
    s: &'a str,
    b: &'a [u8],
    pos: usize,
    mode: u8,
    aux: u16,
    after_eq: bool,
    out: &'a mut Vec<Token>,
}

impl Xml<'_> {
    fn at(&self, p: usize) -> u8 {
        self.b.get(p).copied().unwrap_or(0)
    }

    fn step(&mut self) {
        match self.mode {
            TEXT => self.text(),
            TAG => self.tag(),
            COMMENT => self.until("-->", K::Comment),
            CDATA => self.until("]]>", K::String),
            PI => self.until("?>", K::Preprocessor),
            DECL => self.until(">", K::Preprocessor),
            ATTR_STRING => self.quoted(self.pos, self.aux as u8),
            _ => self.mode = TEXT,
        }
    }

    fn until(&mut self, close: &str, kind: K) {
        let found = self.s[self.pos..].find(close);
        let end = found.map_or(self.b.len(), |q| self.pos + q + close.len());
        push(self.out, self.pos, end, kind);
        self.pos = end;
        if found.is_some() {
            self.mode = TEXT;
        }
    }

    fn open(&mut self, len: usize, kind: K, mode: u8) {
        push(self.out, self.pos, self.pos + len, kind);
        self.pos += len;
        self.mode = mode;
    }

    fn text(&mut self) {
        let found = self.b[self.pos..].iter().position(|&c| c == b'<' || c == b'&');
        match found {
            None => self.pos = self.b.len(),
            Some(q) => {
                self.pos += q;
                let rest = &self.s[self.pos..];
                let name_at = self.pos + 1 + usize::from(self.at(self.pos + 1) == b'/');
                if rest.starts_with('&') {
                    self.entity();
                } else if rest.starts_with("<!--") {
                    self.open(4, K::Comment, COMMENT);
                } else if rest.starts_with("<![CDATA[") {
                    self.open(9, K::Tag, CDATA);
                } else if rest.starts_with("<?") {
                    self.open(2, K::Preprocessor, PI);
                } else if rest.starts_with("<!") {
                    self.open(2, K::Preprocessor, DECL);
                } else if is_name_start(self.at(name_at)) {
                    let mut end = name_at;
                    while end < self.b.len() && is_name(self.b[end]) {
                        end += 1;
                    }
                    self.after_eq = false;
                    self.open(end - self.pos, K::Tag, TAG);
                } else {
                    self.pos += 1;
                }
            }
        }
    }

    fn entity(&mut self) {
        let limit = self.b.len().min(self.pos + 32);
        let body = self.b[self.pos + 1..limit].iter().take_while(|&&c| c.is_ascii_alphanumeric() || c == b'#').count();
        let semi = self.pos + 1 + body;
        if body > 0 && self.at(semi) == b';' {
            push(self.out, self.pos, semi + 1, K::Escape);
            self.pos = semi + 1;
        } else {
            self.pos += 1;
        }
    }

    fn quoted(&mut self, from: usize, quote: u8) {
        let found = self.b[self.pos..].iter().position(|&c| c == quote);
        let end = found.map_or(self.b.len(), |q| self.pos + q + 1);
        push(self.out, from, end, K::String);
        self.pos = end;
        self.after_eq = false;
        match found {
            Some(_) => self.mode = TAG,
            None => {
                self.mode = ATTR_STRING;
                self.aux = u16::from(quote);
            }
        }
    }

    fn tag(&mut self) {
        let c = self.b[self.pos];
        match c {
            b'>' => self.open(1, K::Tag, TEXT),
            b'/' if self.at(self.pos + 1) == b'>' => self.open(2, K::Tag, TEXT),
            b'"' | b'\'' => {
                let start = self.pos;
                self.pos += 1;
                self.quoted(start, c);
            }
            b'=' => {
                push(self.out, self.pos, self.pos + 1, K::Operator);
                self.pos += 1;
                self.after_eq = true;
            }
            b'<' => self.mode = TEXT,
            _ if c.is_ascii_whitespace() || c == b'/' => self.pos += 1,
            _ => {
                let stop = |x: u8| x.is_ascii_whitespace() || b"=>\"'/<".contains(&x);
                let mut end = self.pos;
                while end < self.b.len() && !stop(self.b[end]) {
                    end += char_len(self.b[end]);
                }
                push(self.out, self.pos, end, if self.after_eq { K::String } else { K::Attribute });
                self.after_eq = false;
                self.pos = end;
            }
        }
    }
}
