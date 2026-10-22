//! Data inspector (H5): the bytes at the caret interpreted as numbers, a char, a timestamp and bits.

use kladde_core::Document;

use crate::format::utf8_decode;
use crate::HexState;

const NOT_AVAILABLE: &str = "-";

/// Grid of interpretations of the bytes at the caret, with an LE/BE switch.
pub fn show_inspector(ui: &mut egui::Ui, doc: &Document, state: &HexState, big_endian: &mut bool) {
    let cursor = state.cursor();
    let mut bytes = Vec::with_capacity(8);
    doc.read(cursor..cursor.saturating_add(8), &mut bytes);
    ui.horizontal(|ui| {
        ui.label(format!("Offset {cursor:#X} ({cursor})"));
        ui.selectable_value(big_endian, false, "Little endian");
        ui.selectable_value(big_endian, true, "Big endian");
    });
    egui::Grid::new("kladde_hex_inspector").num_columns(2).striped(true).show(ui, |ui| {
        inspect(&bytes, *big_endian).into_iter().for_each(|(name, value)| {
            ui.label(name);
            ui.monospace(value);
            ui.end_row();
        });
    });
}

/// Label and value of every inspector row; values needing more bytes than available show "-".
pub(crate) fn inspect(bytes: &[u8], be: bool) -> Vec<(&'static str, String)> {
    let int_rows = [("Int8", 1), ("Int16", 2), ("Int32", 4), ("Int64", 8)].into_iter().flat_map(|(name, size)| {
        [(name, show(int(bytes, size, be))), (unsigned_label(size), show(uint(bytes, size, be)))]
    });
    let mut rows: Vec<(&'static str, String)> = int_rows.collect();
    rows.push(("Float32", show(uint(bytes, 4, be).map(|v| float_text(f64::from(f32::from_bits(v as u32)))))));
    rows.push(("Float64", show(uint(bytes, 8, be).map(|v| float_text(f64::from_bits(v))))));
    rows.push(("UTF-8", utf8_decode(bytes, 0).map_or_else(|| "invalid".to_string(), |(c, _)| char_text(c))));
    rows.push(("Unix time (32-bit)", show(int(bytes, 4, be).map(unix_time_utc))));
    rows.push(("Unix time (64-bit)", show(int(bytes, 8, be).map(unix_time_utc))));
    rows.push(("Binary", show(bytes.first().map(|b| format!("{b:08b}")))));
    rows
}

fn unsigned_label(size: usize) -> &'static str {
    match size {
        1 => "UInt8",
        2 => "UInt16",
        4 => "UInt32",
        _ => "UInt64",
    }
}

fn show(value: Option<impl ToString>) -> String {
    value.map_or_else(|| NOT_AVAILABLE.to_string(), |v| v.to_string())
}

/// Unsigned value of the first `size` bytes.
pub(crate) fn uint(bytes: &[u8], size: usize, be: bool) -> Option<u64> {
    let b = bytes.get(..size)?;
    let fold = |acc: u64, &x: &u8| (acc << 8) | u64::from(x);
    Some(if be { b.iter().fold(0, fold) } else { b.iter().rev().fold(0, fold) })
}

/// Two's complement value of the first `size` bytes.
pub(crate) fn int(bytes: &[u8], size: usize, be: bool) -> Option<i64> {
    let shift = 64 - 8 * size as u32;
    uint(bytes, size, be).map(|v| ((v << shift) as i64) >> shift)
}

fn float_text(v: f64) -> String {
    let plain = v == 0.0 || !v.is_finite() || (1e-4..1e15).contains(&v.abs());
    if plain {
        format!("{v}")
    } else {
        format!("{v:e}")
    }
}

fn char_text(c: char) -> String {
    if c.is_control() {
        format!("U+{:04X}", u32::from(c))
    } else {
        format!("{c} (U+{:04X})", u32::from(c))
    }
}

/// Seconds since 1970-01-01 as UTC "YYYY-MM-DD HH:MM:SS" (proleptic Gregorian calendar).
pub(crate) fn unix_time_utc(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", sod / 3600, sod / 60 % 60, sod % 60)
}

/// Date of a day number relative to 1970-01-01 (Howard Hinnant's algorithm, valid for negative days).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(rows: &[(&str, String)], name: &str) -> String {
        rows.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone()).expect("row exists")
    }

    #[test]
    fn integers_in_both_byte_orders() {
        let bytes = [0xFE, 0xFF, 0x00, 0x80, 0x01, 0x02, 0x03, 0x04];
        assert_eq!(int(&bytes, 1, false), Some(-2));
        assert_eq!(uint(&bytes, 1, true), Some(0xFE));
        assert_eq!(int(&bytes, 2, false), Some(-2));
        assert_eq!(int(&bytes, 2, true), Some(-257));
        assert_eq!(uint(&bytes, 2, true), Some(0xFEFF));
        assert_eq!(uint(&bytes, 4, false), Some(0x8000_FFFE));
        assert_eq!(int(&bytes, 4, false), Some(0x8000_FFFE_u32 as i32 as i64));
        assert_eq!(uint(&bytes, 8, true), Some(0xFEFF_0080_0102_0304));
        assert_eq!(uint(&bytes, 8, false), Some(0x0403_0201_8000_FFFE));
        assert_eq!(int(&[0xFF; 8], 8, true), Some(-1));
        assert_eq!(uint(&[0xFF; 8], 8, true), Some(u64::MAX));
        assert_eq!(uint(&bytes[..3], 4, false), None);
    }

    #[test]
    fn inspector_rows_respect_endianness_and_length() {
        let le = inspect(&[0x00, 0x00, 0x80, 0x3F], false);
        assert_eq!(value(&le, "Float32"), "1");
        assert_eq!(value(&le, "UInt16"), "0");
        assert_eq!(value(&le, "Int64"), NOT_AVAILABLE);
        let be = inspect(&[0x3F, 0x80, 0x00, 0x00], true);
        assert_eq!(value(&be, "Float32"), "1");
        assert_eq!(value(&be, "Binary"), "00111111");
        let f64_be = inspect(&(-2.5f64).to_be_bytes(), true);
        assert_eq!(value(&f64_be, "Float64"), "-2.5");
        assert_eq!(value(&inspect(&[0xE2, 0x82, 0xAC], false), "UTF-8"), "€ (U+20AC)");
        assert_eq!(value(&inspect(&[0xE2, 0x82], false), "UTF-8"), "invalid");
        assert_eq!(value(&inspect(&[], false), "Binary"), NOT_AVAILABLE);
    }

    #[test]
    fn unix_time_conversion() {
        assert_eq!(unix_time_utc(0), "1970-01-01 00:00:00");
        assert_eq!(unix_time_utc(-1), "1969-12-31 23:59:59");
        assert_eq!(unix_time_utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(unix_time_utc(1_709_210_096), "2024-02-29 12:34:56");
        assert_eq!(unix_time_utc(i64::from(i32::MAX)), "2038-01-19 03:14:07");
        assert_eq!(unix_time_utc(i64::from(i32::MIN)), "1901-12-13 20:45:52");
        // 1900 is not a leap year, 1600 is.
        assert_eq!(unix_time_utc(-2_203_891_200 - 86_400), "1900-02-28 00:00:00");
        assert_eq!(unix_time_utc(-11_670_955_200), "1600-02-29 12:00:00");
        assert_eq!(unix_time_utc(-62_135_596_800), "0001-01-01 00:00:00");
        assert_eq!(unix_time_utc(253_402_300_799), "9999-12-31 23:59:59");
    }

    #[test]
    fn unix_time_rows_use_chosen_byte_order() {
        let secs = 1_709_210_096u32;
        assert_eq!(value(&inspect(&secs.to_le_bytes(), false), "Unix time (32-bit)"), "2024-02-29 12:34:56");
        assert_eq!(value(&inspect(&secs.to_be_bytes(), true), "Unix time (32-bit)"), "2024-02-29 12:34:56");
        let before_epoch = (-1i64).to_le_bytes();
        assert_eq!(value(&inspect(&before_epoch, false), "Unix time (64-bit)"), "1969-12-31 23:59:59");
    }
}
