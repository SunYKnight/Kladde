//! Pure conversions between bytes and their hex/text representations.

use std::ops::Range;

use crate::{HexConfig, HexMode, TextColumn};

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// Cell shown for UTF-8 continuation bytes of a sequence decoded in an earlier cell.
pub(crate) const CONTINUATION: char = ' ';

/// Placeholder for bytes without a printable representation.
pub(crate) const NON_PRINTABLE: char = '.';

/// Uppercase hex pairs separated by single spaces, e.g. "DE AD BE EF".
pub(crate) fn format_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    bytes.iter().enumerate().for_each(|(i, &b)| {
        if i > 0 {
            out.push(' ');
        }
        out.push(HEX_DIGITS[usize::from(b >> 4)] as char);
        out.push(HEX_DIGITS[usize::from(b & 0x0F)] as char);
    });
    out
}

/// Parses hex digits; whitespace and `0x` prefixes of whitespace-separated tokens are ignored.
pub(crate) fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let digits: String =
        text.split_whitespace().map(|t| t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t)).collect();
    match digits.chars().find(|c| !c.is_ascii_hexdigit()) {
        Some(c) => Err(format!("'{c}' is not a hex digit")),
        None if digits.len() % 2 != 0 => Err("odd number of hex digits".to_string()),
        None => Ok(digits.as_bytes().chunks(2).map(|pair| (hex_value(pair[0]) << 4) | hex_value(pair[1])).collect()),
    }
}

fn hex_value(digit: u8) -> u8 {
    let value = char::from(digit).to_digit(16).expect("caller checked for a hex digit");
    value as u8
}

/// Bytes for `text` typed or pasted into the text column.
pub(crate) fn encode_text(text: &str, enc: TextColumn) -> Result<Vec<u8>, String> {
    match enc {
        TextColumn::Utf8 => Ok(text.as_bytes().to_vec()),
        TextColumn::Latin1 => text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).map_err(|_| format!("'{c}' is not representable in Latin-1")))
            .collect(),
        TextColumn::Ascii => text
            .chars()
            .map(|c| if c.is_ascii() { Ok(c as u8) } else { Err(format!("'{c}' is not representable in ASCII")) })
            .collect(),
    }
}

/// Text copied from the text column.
pub(crate) fn decode_text(bytes: &[u8], enc: TextColumn) -> String {
    match enc {
        TextColumn::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        TextColumn::Latin1 => bytes.iter().map(|&b| char::from(b)).collect(),
        TextColumn::Ascii => bytes.iter().map(|&b| if b.is_ascii() { char::from(b) } else { NON_PRINTABLE }).collect(),
    }
}

/// One display char per byte of `data[row]`. `data` may hold up to 3 bytes before and after the row so UTF-8
/// sequences crossing the row edges are recognised.
pub(crate) fn text_cells(data: &[u8], row: Range<usize>, enc: TextColumn) -> Vec<char> {
    match enc {
        TextColumn::Ascii => data[row].iter().map(|&b| single_byte_cell(b, false)).collect(),
        TextColumn::Latin1 => data[row].iter().map(|&b| single_byte_cell(b, true)).collect(),
        TextColumn::Utf8 => utf8_cells(data, row),
    }
}

fn single_byte_cell(b: u8, latin1: bool) -> char {
    let printable = (0x20..0x7F).contains(&b) || (latin1 && b >= 0xA1 && b != 0xAD);
    if printable {
        char::from(b)
    } else {
        NON_PRINTABLE
    }
}

fn utf8_cells(data: &[u8], row: Range<usize>) -> Vec<char> {
    // Bytes at the row start that complete a sequence begun in the previous row.
    let carried = (row.start.saturating_sub(3)..row.start)
        .find_map(|i| utf8_at(data, i).map(|(_, n)| (i + n).saturating_sub(row.start)).filter(|&k| k > 0))
        .unwrap_or(0);
    let first = (row.start + carried).min(row.end);
    let mut cells: Vec<char> = std::iter::repeat_n(CONTINUATION, first - row.start).collect();
    let mut i = first;
    while i < row.end {
        let (c, n) = utf8_at(data, i).unwrap_or((NON_PRINTABLE, 1));
        cells.push(c);
        cells.extend(std::iter::repeat_n(CONTINUATION, (i + n).min(row.end) - i - 1));
        i += n;
    }
    cells
}

/// Printable char encoded at `data[i..]` and its byte length.
pub(crate) fn utf8_at(data: &[u8], i: usize) -> Option<(char, usize)> {
    utf8_decode(data, i).filter(|(c, _)| !c.is_control())
}

/// Char encoded at `data[i..]` and its byte length; None for invalid or truncated sequences.
pub(crate) fn utf8_decode(data: &[u8], i: usize) -> Option<(char, usize)> {
    let n = match *data.get(i)? {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    };
    let bytes = data.get(i..i + n).filter(|_| n > 0)?;
    let c = std::str::from_utf8(bytes).ok()?.chars().next()?;
    Some((c, n))
}

/// Hex digits of the offset column: at least 8, more for files beyond 4 GiB.
pub(crate) fn offset_digits(len: u64) -> usize {
    let bits = (u64::BITS - len.leading_zeros()) as usize;
    bits.div_ceil(4).max(8)
}

/// Character geometry of the byte column; all positions are in monospace character cells.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub bytes_per_row: u64,
    group: u64,
    /// Digits per byte: 2 (hex) or 8 (binary).
    pub digits: u8,
}

impl Layout {
    pub fn new(cfg: &HexConfig) -> Layout {
        Layout {
            bytes_per_row: u64::from(cfg.bytes_per_row.max(1)),
            group: u64::from(cfg.group.max(1)),
            digits: match cfg.mode {
                HexMode::Hex => 2,
                HexMode::Binary => 8,
            },
        }
    }

    /// Bits written by one typed digit.
    pub fn digit_bits(&self) -> u8 {
        8 / self.digits
    }

    /// Column of the first digit of byte `j` within the row.
    pub fn cell_x(&self, j: u64) -> u64 {
        let extra = if self.group > 1 { j / self.group } else { 0 };
        j * (u64::from(self.digits) + 1) + extra
    }

    /// Width of the byte column.
    pub fn hex_chars(&self) -> u64 {
        self.cell_x(self.bytes_per_row - 1) + u64::from(self.digits)
    }

    /// Digits of `b` as shown in the byte column.
    pub fn byte_text(&self, b: u8) -> String {
        match self.digits {
            2 => format_hex(&[b]),
            _ => format!("{b:08b}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip_with_spaces_and_prefixes() {
        let bytes = parse_hex("de ad\tBE\n0xEF 0X01 0203").expect("valid hex");
        assert_eq!(bytes, [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03]);
        assert_eq!(format_hex(&bytes), "DE AD BE EF 01 02 03");
        assert_eq!(parse_hex(&format_hex(&bytes)), Ok(bytes));
    }

    #[test]
    fn hex_parse_errors() {
        assert!(parse_hex("ABC").is_err());
        assert!(parse_hex("A B C").is_err());
        assert!(parse_hex("GG").is_err());
        assert_eq!(parse_hex("   "), Ok(Vec::new()));
        assert_eq!(format_hex(&[]), "");
    }

    #[test]
    fn ascii_and_latin1_cells() {
        let data = [b'A', 0x00, 0x7F, 0xE9, b'.', b' ', 0xAD, 0xA0];
        let ascii: String = text_cells(&data, 0..data.len(), TextColumn::Ascii).into_iter().collect();
        assert_eq!(ascii, "A.... ..");
        let latin1: String = text_cells(&data, 0..data.len(), TextColumn::Latin1).into_iter().collect();
        assert_eq!(latin1, "A..é. ..");
    }

    #[test]
    fn utf8_cells_decode_sequences() {
        // "é€" then an invalid lone continuation byte and a control char.
        let data = [0xC3, 0xA9, 0xE2, 0x82, 0xAC, 0x80, 0x0A, b'z'];
        let cells: String = text_cells(&data, 0..data.len(), TextColumn::Utf8).into_iter().collect();
        assert_eq!(cells, "é €  ..z");
    }

    #[test]
    fn utf8_sequences_crossing_row_edges() {
        let data = [b'A', 0xE2, 0x82, 0xAC, b'B', 0xF0, 0x9F, 0x98, 0x80];
        // Sequence starting in the row is decoded using bytes after the row end.
        let first: String = text_cells(&data, 0..2, TextColumn::Utf8).into_iter().collect();
        assert_eq!(first, "A€");
        // Its continuation bytes at the start of the next row stay blank.
        let second: String = text_cells(&data, 2..6, TextColumn::Utf8).into_iter().collect();
        assert_eq!(second, "  B😀");
        // Truncated data: lead byte without its continuation is non-printable.
        let truncated: String = text_cells(&data[..7], 5..7, TextColumn::Utf8).into_iter().collect();
        assert_eq!(truncated, "..");
    }

    #[test]
    fn text_encoding_for_typing() {
        assert_eq!(encode_text("é", TextColumn::Utf8), Ok(vec![0xC3, 0xA9]));
        assert_eq!(encode_text("é", TextColumn::Latin1), Ok(vec![0xE9]));
        assert!(encode_text("é", TextColumn::Ascii).is_err());
        assert!(encode_text("€", TextColumn::Latin1).is_err());
        assert_eq!(decode_text(&[0xE9, b'a'], TextColumn::Latin1), "éa");
    }

    #[test]
    fn offset_width_grows_with_size() {
        assert_eq!(offset_digits(0), 8);
        assert_eq!(offset_digits(0xFFFF_FFFF), 8);
        assert_eq!(offset_digits(0x1_0000_0000), 9);
        assert_eq!(offset_digits(u64::MAX), 16);
    }
}
