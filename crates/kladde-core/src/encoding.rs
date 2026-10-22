use encoding_rs::{CoderResult, Decoder, Encoder, EncoderResult, Encoding};

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16BE_BOM: &[u8] = &[0xFE, 0xFF];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextEncoding {
    pub encoding: &'static Encoding,
    pub bom: bool,
}

impl TextEncoding {
    pub const UTF8: TextEncoding = TextEncoding { encoding: &encoding_rs::UTF_8_INIT, bom: false };

    /// Display name such as "UTF-8", "UTF-8 BOM", "UTF-16LE BOM" or "windows-1252".
    pub fn name(&self) -> String {
        if self.bom {
            format!("{} BOM", self.encoding.name())
        } else {
            self.encoding.name().to_string()
        }
    }

    /// Encodings offered for "reopen with" / "convert to".
    pub fn choices() -> Vec<TextEncoding> {
        let plain = |encoding| TextEncoding { encoding, bom: false };
        let with_bom = |encoding| TextEncoding { encoding, bom: true };
        vec![
            plain(encoding_rs::UTF_8),
            with_bom(encoding_rs::UTF_8),
            with_bom(encoding_rs::UTF_16LE),
            with_bom(encoding_rs::UTF_16BE),
            plain(encoding_rs::UTF_16LE),
            plain(encoding_rs::UTF_16BE),
            plain(encoding_rs::WINDOWS_1252),
            plain(encoding_rs::ISO_8859_2),
            plain(encoding_rs::ISO_8859_3),
            plain(encoding_rs::ISO_8859_4),
            plain(encoding_rs::ISO_8859_5),
            plain(encoding_rs::ISO_8859_6),
            plain(encoding_rs::ISO_8859_7),
            plain(encoding_rs::ISO_8859_8),
            plain(encoding_rs::ISO_8859_10),
            plain(encoding_rs::ISO_8859_13),
            plain(encoding_rs::ISO_8859_14),
            plain(encoding_rs::ISO_8859_15),
            plain(encoding_rs::ISO_8859_16),
        ]
    }

    pub(crate) fn is_utf8(&self) -> bool {
        self.encoding == encoding_rs::UTF_8
    }

    pub(crate) fn is_utf16(&self) -> bool {
        self.encoding == encoding_rs::UTF_16LE || self.encoding == encoding_rs::UTF_16BE
    }

    pub(crate) fn bom_bytes(&self) -> &'static [u8] {
        match (self.bom, self.encoding.name()) {
            (true, "UTF-8") => UTF8_BOM,
            (true, "UTF-16LE") => UTF16LE_BOM,
            (true, "UTF-16BE") => UTF16BE_BOM,
            _ => &[],
        }
    }
}

/// BOM first, then UTF-16 without BOM (NUL pattern), then UTF-8 validity, else windows-1252.
pub fn detect_encoding(sample: &[u8]) -> TextEncoding {
    let bom = |encoding| TextEncoding { encoding, bom: true };
    if sample.starts_with(UTF8_BOM) {
        bom(encoding_rs::UTF_8)
    } else if sample.starts_with(UTF16LE_BOM) {
        bom(encoding_rs::UTF_16LE)
    } else if sample.starts_with(UTF16BE_BOM) {
        bom(encoding_rs::UTF_16BE)
    } else if let Some(encoding) = guess_utf16(sample) {
        TextEncoding { encoding, bom: false }
    } else if is_utf8_prefix(sample) {
        TextEncoding::UTF8
    } else {
        TextEncoding { encoding: encoding_rs::WINDOWS_1252, bom: false }
    }
}

/// NULs or control bytes that UTF-16 text would not explain.
pub fn looks_binary(sample: &[u8]) -> bool {
    let utf16 = sample.starts_with(UTF16LE_BOM) || sample.starts_with(UTF16BE_BOM) || guess_utf16(sample).is_some();
    let controls = sample.iter().filter(|&&b| matches!(b, 0..=8 | 0x0E..=0x1A | 0x1C..=0x1F)).count();
    !utf16 && (sample.contains(&0) || controls * 32 > sample.len())
}

/// A sample cut mid-character is still UTF-8; only real invalid sequences count.
fn is_utf8_prefix(sample: &[u8]) -> bool {
    match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// Mostly-ASCII UTF-16 has a zero in every other byte.
fn guess_utf16(sample: &[u8]) -> Option<&'static Encoding> {
    let pairs = sample.len() / 2;
    let even = sample.chunks_exact(2).filter(|p| p[0] == 0).count();
    let odd = sample.chunks_exact(2).filter(|p| p[1] == 0).count();
    if pairs < 2 {
        None
    } else if odd * 10 >= pairs * 4 && even * 10 <= pairs {
        Some(encoding_rs::UTF_16LE)
    } else if even * 10 >= pairs * 4 && odd * 10 <= pairs {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            LineEnding::Lf => b"\n",
            LineEnding::CrLf => b"\r\n",
            LineEnding::Cr => b"\r",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
            LineEnding::Cr => "CR",
        }
    }

    pub fn platform() -> Self {
        if cfg!(windows) {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }
}

/// Most frequent terminator in `sample`; None if it has no line break.
pub fn detect_line_ending(sample: &[u8]) -> Option<LineEnding> {
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let mut at = 0;
    while let Some(p) = memchr::memchr2(b'\r', b'\n', &sample[at..]).map(|p| at + p) {
        let pair = sample[p] == b'\r' && sample.get(p + 1) == Some(&b'\n');
        match (pair, sample[p]) {
            (true, _) => crlf += 1,
            (false, b'\n') => lf += 1,
            // A CR as the very last byte may be the first half of a cut CRLF.
            _ => cr += usize::from(p + 1 < sample.len()),
        }
        at = p + if pair { 2 } else { 1 };
    }
    let best = crlf.max(lf).max(cr);
    if best == 0 {
        None
    } else if best == crlf {
        Some(LineEnding::CrLf)
    } else if best == lf {
        Some(LineEnding::Lf)
    } else {
        Some(LineEnding::Cr)
    }
}

/// Rewrites every CRLF, LF and lone CR in `bytes` as `le`.
pub(crate) fn convert_line_endings(bytes: &[u8], le: LineEnding) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut start = 0;
    while let Some(p) = memchr::memchr2(b'\r', b'\n', &bytes[start..]).map(|p| start + p) {
        out.extend_from_slice(&bytes[start..p]);
        out.extend_from_slice(le.as_bytes());
        start = p + if bytes[p] == b'\r' && bytes.get(p + 1) == Some(&b'\n') { 2 } else { 1 };
    }
    out.extend_from_slice(&bytes[start..]);
    out
}

/// Appends the UTF-8 decoding of `src` (a chunk of a stream) to `out`.
pub(crate) fn decode_chunk(decoder: &mut Decoder, src: &[u8], last: bool, out: &mut Vec<u8>) {
    let max = decoder.max_utf8_buffer_length(src.len()).expect("chunk sizes cannot overflow usize");
    let start = out.len();
    out.resize(start + max, 0);
    let (result, read, written, _replaced) = decoder.decode_to_utf8(src, &mut out[start..], last);
    assert!(result == CoderResult::InputEmpty && read == src.len(), "buffer sized by max_utf8_buffer_length");
    out.truncate(start + written);
}

enum Mode {
    Raw,
    Utf16 { big_endian: bool },
    Legacy(Encoder),
}

/// Streams UTF-8 document bytes into the target encoding, BOM first.
pub(crate) struct Reencoder {
    mode: Mode,
    utf8: Decoder,
    bom: &'static [u8],
    unmappable: u64,
}

impl Reencoder {
    /// `None` writes bytes unchanged (binary documents).
    pub(crate) fn new(target: Option<TextEncoding>) -> Reencoder {
        let mode = match target {
            None => Mode::Raw,
            Some(t) if t.is_utf8() => Mode::Raw,
            Some(t) if t.is_utf16() => Mode::Utf16 { big_endian: t.encoding == encoding_rs::UTF_16BE },
            Some(t) => Mode::Legacy(t.encoding.new_encoder()),
        };
        Reencoder {
            mode,
            utf8: encoding_rs::UTF_8.new_decoder_without_bom_handling(),
            bom: target.map_or(&[], |t| t.bom_bytes()),
            unmappable: 0,
        }
    }

    /// Characters the target encoding could not represent; written as '?'.
    pub(crate) fn unmappable(&self) -> u64 {
        self.unmappable
    }

    /// Replaces `out` with the encoding of `input`; `last` flushes split sequences.
    pub(crate) fn encode(&mut self, input: &[u8], last: bool, out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(std::mem::take(&mut self.bom));
        match &mut self.mode {
            Mode::Raw => out.extend_from_slice(input),
            Mode::Utf16 { big_endian } => {
                let max = self.utf8.max_utf16_buffer_length(input.len()).expect("chunk sizes cannot overflow usize");
                let mut units = vec![0u16; max];
                let (result, read, written, _replaced) = self.utf8.decode_to_utf16(input, &mut units, last);
                assert!(result == CoderResult::InputEmpty && read == input.len(), "buffer sized by max length");
                let be = *big_endian;
                out.extend(units[..written].iter().flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() }));
            }
            Mode::Legacy(encoder) => {
                let max = self.utf8.max_utf8_buffer_length(input.len()).expect("chunk sizes cannot overflow usize");
                let mut text = String::with_capacity(max);
                let (result, read, _replaced) = self.utf8.decode_to_string(input, &mut text, last);
                assert!(result == CoderResult::InputEmpty && read == input.len(), "buffer sized by max length");
                self.unmappable += encode_legacy(encoder, &text, last, out);
            }
        }
    }
}

/// Appends `text` encoded by `encoder` to `out`; returns the number of '?' substitutions.
fn encode_legacy(encoder: &mut Encoder, text: &str, last: bool, out: &mut Vec<u8>) -> u64 {
    let mut rest = text;
    let mut done = false;
    let mut unmappable = 0;
    while !done {
        let max = encoder
            .max_buffer_length_from_utf8_without_replacement(rest.len())
            .expect("chunk sizes cannot overflow usize");
        let start = out.len();
        out.resize(start + max, 0);
        let (result, read, written) = encoder.encode_from_utf8_without_replacement(rest, &mut out[start..], last);
        out.truncate(start + written);
        rest = &rest[read..];
        match result {
            EncoderResult::InputEmpty => done = true,
            EncoderResult::OutputFull => {}
            EncoderResult::Unmappable(_) => {
                out.push(b'?');
                unmappable += 1;
            }
        }
    }
    unmappable
}
