//! Line-oriented lexers: CSV/TSV rainbow columns and unified diffs.

use crate::{push, Sub, Token, TokenKind as K, RAINBOW_COLUMNS};

const IN_QUOTES: u8 = 1; // aux = column of the open quoted field

pub(crate) fn csv(delim: u8, sub: Sub, line: &str, out: &mut Vec<Token>) -> Sub {
    let b = line.as_bytes();
    let continued = sub.mode == IN_QUOTES;
    let mut column = if continued { sub.aux } else { 0 };
    let mut quoted = continued;
    let mut field_start = 0;
    let mut p = 0;
    while p < b.len() {
        let c = b[p];
        if quoted && c == b'"' && b.get(p + 1) == Some(&b'"') {
            p += 2;
        } else {
            if c == b'"' && (quoted || p == field_start) {
                quoted = !quoted;
            } else if c == delim && !quoted {
                push(out, field_start, p, column_kind(column));
                column = column.wrapping_add(1);
                field_start = p + 1;
            }
            p += 1;
        }
    }
    push(out, field_start, b.len(), column_kind(column));
    if quoted {
        Sub { mode: IN_QUOTES, aux: column, ..Sub::default() }
    } else {
        Sub::default()
    }
}

fn column_kind(column: u16) -> K {
    K::CsvColumn((column % u16::from(RAINBOW_COLUMNS)) as u8)
}

pub(crate) fn diff(line: &str, out: &mut Vec<Token>) -> Sub {
    let headers = ["diff ", "index ", "+++", "---", "new file", "deleted file", "similarity", "rename ", "Index: "];
    let kind = if headers.iter().any(|h| line.starts_with(h)) || line.starts_with("===") {
        Some(K::Heading)
    } else if line.starts_with("@@") {
        Some(K::Preprocessor)
    } else if line.starts_with('+') || line.starts_with('>') {
        Some(K::String)
    } else if line.starts_with('-') || line.starts_with('<') {
        Some(K::Constant)
    } else if line.starts_with('\\') {
        Some(K::Comment)
    } else {
        None
    };
    if let Some(k) = kind {
        push(out, 0, line.len(), k);
    }
    Sub::default()
}
