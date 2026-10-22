/// Parsed CSV; rows may have different lengths.
pub struct CsvTable {
    pub delimiter: u8,
    pub rows: Vec<Vec<String>>,
}

const DELIMITERS: [u8; 4] = *b",;\t|";
/// Lines inspected for delimiter detection.
const SAMPLE_LINES: usize = 20;

/// Parses up to `max_rows` records; the delimiter (`,` `;` tab `|`) is detected from the first lines.
pub fn parse_csv(text: &str, max_rows: usize) -> Result<CsvTable, String> {
    let delimiter = detect_delimiter(text);
    let mut reader =
        csv::ReaderBuilder::new().delimiter(delimiter).has_headers(false).flexible(true).from_reader(text.as_bytes());
    let rows = reader
        .records()
        .take(max_rows)
        .map(|record| record.map(|r| r.iter().map(str::to_string).collect()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<Vec<String>>, String>>()?;
    Ok(CsvTable { delimiter, rows })
}

/// Picks the candidate that appears in most sample lines with the same count as in the first line.
fn detect_delimiter(text: &str) -> u8 {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).take(SAMPLE_LINES).collect();
    DELIMITERS
        .iter()
        .map(|&d| {
            let counts: Vec<usize> = lines.iter().map(|l| count_unquoted(l, d)).collect();
            let first = counts.first().copied().unwrap_or(0);
            let consistent = counts.iter().filter(|&&c| c == first).count();
            (d, if first == 0 { (0, 0) } else { (consistent, first) })
        })
        .fold((b',', (0, 0)), |best, cand| if cand.1 > best.1 { cand } else { best })
        .0
}

fn count_unquoted(line: &str, delimiter: u8) -> usize {
    line.bytes()
        .fold((0usize, false), |(count, quoted), b| match b {
            b'"' => (count, !quoted),
            _ if b == delimiter && !quoted => (count + 1, quoted),
            _ => (count, quoted),
        })
        .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_delimiter() {
        for (d, text) in [
            (b',', "a,b,c\n1,2,3\n"),
            (b';', "name;price\n\"x,y\";1,50\nz;2,00\n"),
            (b'\t', "a\tb\n1,5\t2\n"),
            (b'|', "a|b|c\n1|2|3\n"),
        ] {
            let table = parse_csv(text, 100).expect("parses");
            assert_eq!(table.delimiter, d, "{text:?}");
            assert_eq!(table.rows[0].len(), table.rows[1].len(), "{text:?}");
        }
    }

    #[test]
    fn quoted_fields_ragged_rows_and_limit() {
        let table = parse_csv("a;\"b;\"\"q\"\"\nline\"\n1\n2;3\n", 2).expect("parses");
        assert_eq!(table.delimiter, b';');
        assert_eq!(table.rows, vec![vec!["a".to_string(), "b;\"q\"\nline".to_string()], vec!["1".to_string()]]);
        let single = parse_csv("just text\n", 10).expect("parses");
        assert_eq!(single.delimiter, b',');
        assert_eq!(single.rows, vec![vec!["just text".to_string()]]);
        assert!(parse_csv("", 10).expect("parses").rows.is_empty());
    }
}
