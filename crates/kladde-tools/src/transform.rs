use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use quick_xml::events::Event;
use quick_xml::{Reader, Writer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineTool {
    SortAsc,
    SortDesc,
    SortAscCi,
    Dedupe,
    TrimTrailing,
    TrimLeading,
    TrimBoth,
    Upper,
    Lower,
    Title,
    Join,
    Reverse,
    RemoveEmpty,
}

impl LineTool {
    pub fn all() -> &'static [LineTool] {
        &[
            LineTool::SortAsc,
            LineTool::SortDesc,
            LineTool::SortAscCi,
            LineTool::Dedupe,
            LineTool::TrimTrailing,
            LineTool::TrimLeading,
            LineTool::TrimBoth,
            LineTool::Upper,
            LineTool::Lower,
            LineTool::Title,
            LineTool::Join,
            LineTool::Reverse,
            LineTool::RemoveEmpty,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            LineTool::SortAsc => "Sort lines ascending",
            LineTool::SortDesc => "Sort lines descending",
            LineTool::SortAscCi => "Sort lines ascending (ignore case)",
            LineTool::Dedupe => "Remove duplicate lines",
            LineTool::TrimTrailing => "Trim trailing whitespace",
            LineTool::TrimLeading => "Trim leading whitespace",
            LineTool::TrimBoth => "Trim leading and trailing whitespace",
            LineTool::Upper => "UPPERCASE",
            LineTool::Lower => "lowercase",
            LineTool::Title => "Title Case",
            LineTool::Join => "Join lines",
            LineTool::Reverse => "Reverse line order",
            LineTool::RemoveEmpty => "Remove empty lines",
        }
    }
}

/// Applies a line tool; the text's line ending style and a final line break are preserved.
pub fn apply_line_tool(text: &str, tool: LineTool) -> String {
    let eol = if text.contains("\r\n") {
        "\r\n"
    } else if text.contains('\r') {
        "\r"
    } else {
        "\n"
    };
    let body = text.strip_suffix(eol).unwrap_or(text);
    let trailing = if body.len() < text.len() { eol } else { "" };
    let mut lines: Vec<String> = body.split(eol).map(str::to_string).collect();
    match tool {
        LineTool::SortAsc => lines.sort(),
        LineTool::SortDesc => lines.sort_by(|a, b| b.cmp(a)),
        LineTool::SortAscCi => lines.sort_by_cached_key(|l| l.to_lowercase()),
        LineTool::Dedupe => {
            let mut seen = std::collections::HashSet::new();
            lines.retain(|l| seen.insert(l.clone()));
        }
        LineTool::TrimTrailing => lines.iter_mut().for_each(|l| *l = l.trim_end().to_string()),
        LineTool::TrimLeading => lines.iter_mut().for_each(|l| *l = l.trim_start().to_string()),
        LineTool::TrimBoth => lines.iter_mut().for_each(|l| *l = l.trim().to_string()),
        LineTool::Upper => lines.iter_mut().for_each(|l| *l = l.to_uppercase()),
        LineTool::Lower => lines.iter_mut().for_each(|l| *l = l.to_lowercase()),
        LineTool::Title => lines.iter_mut().for_each(|l| *l = title_case(l)),
        LineTool::Join => {
            let joined = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
            lines = vec![joined];
        }
        LineTool::Reverse => lines.reverse(),
        LineTool::RemoveEmpty => lines.retain(|l| !l.trim().is_empty()),
    }
    let mut out = lines.join(eol);
    out.push_str(trailing);
    out
}

fn title_case(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut word_start = true;
    for c in line.chars() {
        if word_start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        word_start = c.is_whitespace();
    }
    out
}

/// Re-indents valid JSON (two spaces); strings and numbers are kept exactly as written.
pub fn json_pretty(text: &str) -> Result<String, String> {
    reformat_json(text, true)
}

/// Removes all insignificant whitespace from valid JSON.
pub fn json_minify(text: &str) -> Result<String, String> {
    reformat_json(text, false)
}

fn reformat_json(text: &str, pretty: bool) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(text).map_err(|e| e.to_string())?;
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if !c.is_whitespace() {
            let next_closes = chars.clone().find(|n| !n.is_whitespace()).is_some_and(|n| n == '}' || n == ']');
            match c {
                '"' => {
                    in_string = true;
                    out.push(c);
                }
                '{' | '[' if pretty && !next_closes => {
                    depth += 1;
                    out.push(c);
                    newline(&mut out, depth);
                }
                '{' | '[' => {
                    // Empty containers stay compact; skip to the closer.
                    out.push(c);
                    if next_closes {
                        out.extend(chars.by_ref().find(|n| !n.is_whitespace()));
                    }
                }
                '}' | ']' if pretty => {
                    depth -= 1;
                    newline(&mut out, depth);
                    out.push(c);
                }
                ',' if pretty => {
                    out.push(c);
                    newline(&mut out, depth);
                }
                ':' if pretty => out.push_str(": "),
                _ => out.push(c),
            }
        }
    }
    Ok(out)
}

fn newline(out: &mut String, depth: usize) {
    out.push('\n');
    out.extend(std::iter::repeat("  ").take(depth));
}

/// Re-indents well-formed XML (two spaces); whitespace-only text between tags is dropped.
pub fn xml_pretty(text: &str) -> Result<String, String> {
    reformat_xml(text, true)
}

/// Removes whitespace between tags of well-formed XML.
pub fn xml_minify(text: &str) -> Result<String, String> {
    reformat_xml(text, false)
}

fn reformat_xml(text: &str, pretty: bool) -> Result<String, String> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut writer = if pretty { Writer::new_with_indent(Vec::new(), b' ', 2) } else { Writer::new(Vec::new()) };
    let mut depth = 0usize;
    let mut done = false;
    while !done {
        let event = reader.read_event().map_err(|e| format!("{e} (at byte {})", reader.error_position()))?;
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            Event::Eof => done = true,
            _ => {}
        }
        if !done {
            writer.write_event(event).map_err(|e| e.to_string())?;
        }
    }
    if depth > 0 {
        Err(format!("{depth} unclosed element(s) at end of input"))
    } else {
        String::from_utf8(writer.into_inner()).map_err(|e| e.to_string())
    }
}

pub fn base64_encode(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

/// Decodes standard or URL-safe base64; line breaks and missing padding are accepted.
pub fn base64_decode(text: &str) -> Result<String, String> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let alphabet = if compact.contains(['-', '_']) { &alphabet::URL_SAFE } else { &alphabet::STANDARD };
    let config = GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let bytes = GeneralPurpose::new(alphabet, config).decode(compact).map_err(|e| e.to_string())?;
    utf8(bytes)
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Decodes %XX sequences; `+` stays a plus sign (RFC 3986, not form encoding).
pub fn url_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or_else(|| format!("incomplete escape at byte {i}"))?;
            out.push(hex_byte(hex).ok_or_else(|| format!("invalid escape at byte {i}"))?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    utf8(out)
}

/// Lowercase hex of the UTF-8 bytes, no separators.
pub fn hex_encode(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

/// Decodes hex digits; whitespace between bytes is ignored.
pub fn hex_decode(text: &str) -> Result<String, String> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let bytes = if digits.len() % 2 == 0 {
        digits
            .chunks(2)
            .map(|pair| hex_byte(pair).ok_or_else(|| format!("invalid hex digits: {}", String::from_utf8_lossy(pair))))
            .collect::<Result<Vec<u8>, String>>()
    } else {
        Err("odd number of hex digits".to_string())
    }?;
    utf8(bytes)
}

fn hex_byte(pair: &[u8]) -> Option<u8> {
    let digit = |b: u8| (b as char).to_digit(16);
    Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
}

fn utf8(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|e| format!("decoded bytes are not valid UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorting_and_dedupe_keep_crlf_and_final_newline() {
        let text = "b\r\nA\r\na\r\nb\r\n";
        assert_eq!(apply_line_tool(text, LineTool::SortAsc), "A\r\na\r\nb\r\nb\r\n");
        assert_eq!(apply_line_tool(text, LineTool::SortDesc), "b\r\nb\r\na\r\nA\r\n");
        assert_eq!(apply_line_tool("b\nB\na\nA", LineTool::SortAscCi), "a\nA\nb\nB");
        assert_eq!(apply_line_tool(text, LineTool::Dedupe), "b\r\nA\r\na\r\n");
        assert_eq!(apply_line_tool(text, LineTool::Reverse), "b\r\na\r\nA\r\nb\r\n");
    }

    #[test]
    fn trimming_case_and_empty_lines() {
        let text = "  one  \n\n \t\ntwo words \n";
        assert_eq!(apply_line_tool(text, LineTool::TrimTrailing), "  one\n\n\ntwo words\n");
        assert_eq!(apply_line_tool(text, LineTool::TrimLeading), "one  \n\n\ntwo words \n");
        assert_eq!(apply_line_tool(text, LineTool::TrimBoth), "one\n\n\ntwo words\n");
        assert_eq!(apply_line_tool(text, LineTool::RemoveEmpty), "  one  \ntwo words \n");
        assert_eq!(apply_line_tool("hELLO wörld\n", LineTool::Title), "Hello Wörld\n");
        assert_eq!(apply_line_tool("straße", LineTool::Upper), "STRASSE");
        assert_eq!(apply_line_tool("ÄB\rC", LineTool::Lower), "äb\rc");
        assert_eq!(apply_line_tool(" a \n\nb\n", LineTool::Join), "a b\n");
        assert_eq!(apply_line_tool("", LineTool::SortAsc), "");
    }

    #[test]
    fn json_pretty_and_minify_round_trip() {
        let compact = r#"{"a":[1,2.50,{"b":null}],"e":{},"l":[],"s":"x , \"y\": {[","n":1E5}"#;
        let pretty = json_pretty(compact).expect("valid json");
        assert_eq!(
            pretty,
            "{\n  \"a\": [\n    1,\n    2.50,\n    {\n      \"b\": null\n    }\n  ],\n  \"e\": {},\n  \"l\": [],\n  \
             \"s\": \"x , \\\"y\\\": {[\",\n  \"n\": 1E5\n}"
        );
        assert_eq!(json_minify(&pretty).expect("valid json"), compact);
        assert_eq!(json_pretty(&json_minify(&pretty).expect("minify")).expect("pretty"), pretty);
        assert_eq!(json_minify(" [ \"a\\\\\" , { } ] ").expect("valid"), "[\"a\\\\\",{}]");
        assert!(json_pretty("{\"a\":}").is_err());
        assert!(json_minify("[1,2").is_err());
    }

    #[test]
    fn xml_pretty_and_minify_round_trip() {
        let xml = "<?xml version=\"1.0\"?>\n<root a=\"1\">  <item>text &amp; more</item>\n<empty/><!-- c --></root>";
        let pretty = xml_pretty(xml).expect("well-formed");
        assert_eq!(
            pretty,
            "<?xml version=\"1.0\"?>\n<root a=\"1\">\n  <item>text &amp; more</item>\n  <empty/>\n  <!-- c -->\n</root>"
        );
        let minified = xml_minify(&pretty).expect("well-formed");
        assert_eq!(
            minified,
            "<?xml version=\"1.0\"?><root a=\"1\"><item>text &amp; more</item><empty/><!-- c --></root>"
        );
        assert_eq!(xml_pretty(&minified).expect("well-formed"), pretty);
        assert!(xml_pretty("<a><b></a>").is_err());
        assert!(xml_minify("<a><b>").is_err());
    }

    #[test]
    fn base64_round_trip_and_errors() {
        assert_eq!(base64_encode("Hällo"), "SMOkbGxv");
        assert_eq!(base64_decode("SMOkbGxv").expect("valid"), "Hällo");
        assert_eq!(base64_decode("SMOk\r\nbGxv\n").expect("line breaks ignored"), "Hällo");
        assert_eq!(base64_decode("YQ").expect("missing padding accepted"), "a");
        assert!(base64_decode("-_8").is_err(), "valid URL-safe alphabet but not UTF-8");
        assert_eq!(base64_decode(&base64_encode("?>?")).expect("valid"), "?>?");
        assert_eq!(base64_decode("Pz4_").expect("URL-safe accepted"), "?>?");
        assert!(base64_decode("@@@").is_err());
        assert!(base64_decode("/w==").is_err(), "0xFF is not UTF-8");
    }

    #[test]
    fn url_round_trip_and_errors() {
        let text = "a b&c=d/ü~-_.+";
        let encoded = url_encode(text);
        assert_eq!(encoded, "a%20b%26c%3Dd%2F%C3%BC~-_.%2B");
        assert_eq!(url_decode(&encoded).expect("valid"), text);
        assert_eq!(url_decode("a+b%2b").expect("valid"), "a+b+");
        assert!(url_decode("%4").is_err());
        assert!(url_decode("%zz").is_err());
        assert!(url_decode("%FF").is_err());
    }

    #[test]
    fn hex_round_trip_and_errors() {
        assert_eq!(hex_encode("Hi ü"), "486920c3bc");
        assert_eq!(hex_decode("486920c3bc").expect("valid"), "Hi ü");
        assert_eq!(hex_decode("48 69\n20 C3 BC").expect("whitespace and uppercase"), "Hi ü");
        assert!(hex_decode("486").is_err());
        assert!(hex_decode("4g").is_err());
        assert!(hex_decode("ff").is_err());
    }
}
