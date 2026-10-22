use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::*;

/// RAM/mapping threshold small enough to force mapped and paged sources in tests.
const SMALL: u64 = 1024;

fn open_with(path: &Path, opts: OpenOptions, limit: u64) -> Document {
    Document::open_with_limit(path, &opts, &Progress::default(), limit).expect("open")
}

fn content(doc: &Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.read(0..doc.len(), &mut out);
    out
}

fn wait_index(doc: &mut Document) {
    while doc.line_index_progress().is_some() {
        doc.poll_line_index();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(doc.take_error().is_none());
}

/// Compares line_count / line_to_offset / offset_to_line with a naive scan.
fn check_lines(doc: &Document) {
    let bytes = content(doc);
    let starts: Vec<u64> = std::iter::once(0)
        .chain(bytes.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i as u64 + 1))
        .collect();
    assert_eq!(doc.line_count(), Some(starts.len() as u64));
    starts.iter().enumerate().for_each(|(l, &s)| assert_eq!(doc.line_to_offset(l as u64), Some(s), "line {l}"));
    assert_eq!(doc.line_to_offset(starts.len() as u64), None);
    let step = if bytes.len() > 4000 { 97 } else { 1 };
    let near_starts = starts.iter().flat_map(|&s| [s.saturating_sub(1), s]);
    (0..=bytes.len() as u64).step_by(step).chain(near_starts).for_each(|off| {
        let expected = starts.partition_point(|&s| s <= off) as u64 - 1;
        assert_eq!(doc.offset_to_line(off), Some(expected), "offset {off}");
    });
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n.max(1)
    }
}

/// Applies pseudo-random edits to `doc` and a plain Vec model, checking content after each.
fn random_edits(doc: &mut Document, model: &mut Vec<u8>, rounds: usize, seed: u64) {
    const SNIPPETS: [&[u8]; 6] = [b"a", b"\n", b"\r\n", b"xy\nz", b"hello world", b"\n\n"];
    let mut rng = Lcg(seed);
    (0..rounds).for_each(|_| {
        let len = model.len() as u64;
        let a = rng.next(len + 1);
        let b = (a + rng.next(8)).min(len);
        let snippet = SNIPPETS[rng.next(SNIPPETS.len() as u64) as usize];
        match rng.next(3) {
            0 => {
                doc.insert(a, snippet).expect("insert");
                model.splice(a as usize..a as usize, snippet.iter().copied());
            }
            1 => {
                doc.delete(a..b).expect("delete");
                model.drain(a as usize..b as usize);
            }
            _ => {
                doc.replace(a..b, snippet).expect("replace");
                model.splice(a as usize..b as usize, snippet.iter().copied());
            }
        }
        assert_eq!(&content(doc), model);
    });
}

fn sample_text(lines: usize) -> Vec<u8> {
    (0..lines)
        .flat_map(|i| {
            let eol: &[u8] = if i % 3 == 0 { b"\r\n" } else { b"\n" };
            format!("line {i} some text").into_bytes().into_iter().chain(eol.iter().copied())
        })
        .chain(b"no trailing newline".iter().copied())
        .collect()
}

#[test]
fn edits_undo_redo_and_dirty_round_trip() {
    let mut doc = Document::new_empty();
    assert!(!doc.is_dirty());
    doc.insert(0, b"hello world").unwrap();
    doc.delete(0..1).unwrap();
    doc.replace(0..4, b"J").unwrap();
    assert_eq!(content(&doc), b"J world");
    assert!(doc.is_dirty());
    assert_eq!(doc.undo(), Some(4));
    assert_eq!(content(&doc), b"ello world");
    doc.undo();
    doc.undo();
    assert_eq!(content(&doc), b"");
    assert!(!doc.is_dirty());
    assert!(!doc.can_undo());
    doc.redo();
    doc.redo();
    assert_eq!(content(&doc), b"ello world");
    assert!(doc.can_redo());
    doc.insert(0, b"H").unwrap();
    assert!(!doc.can_redo(), "a new edit drops the redo branch");
    assert_eq!(content(&doc), b"Hello world");
}

#[test]
fn undo_back_to_saved_state_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, b"abc").unwrap();
    let mut doc = Document::open(&path, &OpenOptions::default(), &Progress::default()).unwrap();
    doc.insert(3, b"d").unwrap();
    doc.save_blocking(&path).unwrap();
    assert!(!doc.is_dirty());
    doc.insert(4, b"e").unwrap();
    assert!(doc.is_dirty(), "typing after a save must not merge into the saved step");
    doc.undo();
    assert!(!doc.is_dirty());
    doc.undo();
    assert!(doc.is_dirty());
    assert_eq!(content(&doc), b"abc");
    doc.redo();
    assert!(!doc.is_dirty());
    doc.undo();
    doc.insert(0, b"x").unwrap();
    assert!(doc.is_dirty());
    doc.undo();
    assert!(doc.is_dirty(), "saved state was on the discarded redo branch");
}

#[test]
fn nested_undo_group_undoes_as_one() {
    let mut doc = Document::from_bytes(b"0123456789".to_vec());
    let rev = doc.revision();
    doc.begin_undo_group();
    doc.insert(10, b"ab").unwrap();
    doc.begin_undo_group();
    doc.delete(0..2).unwrap();
    doc.end_undo_group();
    doc.replace(3..5, b"XYZ").unwrap();
    doc.end_undo_group();
    doc.begin_undo_group();
    doc.end_undo_group();
    assert_eq!(content(&doc), b"234XYZ789ab");
    assert_eq!(doc.changed_since(rev), Some(0));
    doc.undo();
    assert_eq!(content(&doc), b"0123456789");
    assert!(!doc.can_undo(), "the empty group added no step");
    doc.redo();
    assert_eq!(content(&doc), b"234XYZ789ab");
}

#[test]
fn typed_characters_merge_until_whitespace_or_jump() {
    let mut doc = Document::new_empty();
    b"abc d".iter().enumerate().for_each(|(i, c)| doc.insert(i as u64, &[*c]).unwrap());
    assert_eq!(content(&doc), b"abc d");
    doc.undo();
    assert_eq!(content(&doc), b"abc ");
    doc.undo();
    assert_eq!(content(&doc), b"");
    assert!(!doc.can_undo());
    doc.insert(0, b"x").unwrap();
    doc.insert(1, "é".as_bytes()).unwrap();
    doc.insert(3, b"y").unwrap();
    doc.insert(0, b"z").unwrap();
    doc.undo();
    assert_eq!(content(&doc), "xéy".as_bytes(), "a caret jump starts a new step");
    doc.undo();
    assert_eq!(content(&doc), b"", "a multi-byte char continues the typing step");
}

#[test]
fn changed_since_reports_lowest_offset() {
    let mut doc = Document::from_bytes(vec![b'a'; 100]);
    let rev = doc.revision();
    assert_eq!(doc.changed_since(rev), None);
    doc.insert(50, b"x").unwrap();
    assert_eq!(doc.changed_since(rev), Some(50));
    let mid = doc.revision();
    doc.delete(10..12).unwrap();
    assert_eq!(doc.changed_since(rev), Some(10));
    assert_eq!(doc.changed_since(mid), Some(10));
    (0..300).for_each(|_| doc.insert(90, b"y").unwrap());
    assert_eq!(doc.changed_since(mid), Some(0), "history older than the ring");
}

#[test]
fn line_index_matches_naive_count_on_edited_memory_doc() {
    let text = sample_text(60);
    let mut doc = Document::from_bytes(text.clone());
    check_lines(&doc);
    let mut model = text.clone();
    random_edits(&mut doc, &mut model, 120, 7);
    check_lines(&doc);
    while doc.undo().is_some() {}
    assert_eq!(content(&doc), text);
    check_lines(&doc);
}

#[test]
fn line_index_matches_naive_count_on_edited_mapped_doc() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    let text = sample_text(3000);
    fs::write(&path, &text).unwrap();
    let mut doc = open_with(&path, OpenOptions::default(), SMALL);
    assert_eq!(doc.source_kind(), SourceKind::Mapped);
    let mut model = text.clone();
    random_edits(&mut doc, &mut model, 40, 11);
    wait_index(&mut doc);
    check_lines(&doc);
    random_edits(&mut doc, &mut model, 40, 12);
    check_lines(&doc);
    while doc.undo().is_some() {}
    assert_eq!(content(&doc), text);
    assert!(!doc.is_dirty());
    check_lines(&doc);
}

#[test]
fn line_navigation_at_boundaries() {
    let mut doc = Document::from_bytes(b"ab\r\ncd\n\nef".to_vec());
    let starts = [0, 0, 0, 0, 4, 4, 4, 7, 8, 8, 8];
    let ends = [2, 2, 2, 2, 6, 6, 6, 7, 10, 10, 10];
    let nexts = [4, 4, 4, 4, 7, 7, 7, 8, 10, 10, 10];
    let check = |doc: &Document| {
        (0..=10u64).for_each(|off| {
            assert_eq!(doc.line_start_at(off), starts[off as usize], "start {off}");
            assert_eq!(doc.line_end_at(off), ends[off as usize], "end {off}");
            assert_eq!(doc.next_line_start(off), nexts[off as usize], "next {off}");
        });
    };
    check(&doc);
    // Same text spread over several pieces.
    doc.delete(0..10).unwrap();
    [&b"ef"[..], b"\n", b"\ncd\n", b"\r", b"ab"].iter().for_each(|p| doc.insert(0, p).unwrap());
    assert_eq!(content(&doc), b"ab\r\ncd\n\nef");
    check(&doc);
    assert_eq!(doc.line_start_at(99), 8);
    let empty = Document::new_empty();
    assert_eq!((empty.line_start_at(0), empty.line_end_at(0), empty.next_line_start(0)), (0, 0, 0));
    assert_eq!(empty.line_count(), Some(1));
}

#[test]
fn utf16le_bom_round_trips_through_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u16.txt");
    let encode = |s: &str| -> Vec<u8> {
        [0xFF, 0xFE].into_iter().chain(s.encode_utf16().flat_map(|u| u.to_le_bytes())).collect()
    };
    fs::write(&path, encode("Grüße\r\nZeile 2")).unwrap();
    let mut doc = Document::open(&path, &OpenOptions::default(), &Progress::default()).unwrap();
    assert_eq!(doc.encoding().name(), "UTF-16LE BOM");
    assert!(!doc.is_binary());
    assert_eq!(doc.read_string_lossy(0..doc.len()), "Grüße\r\nZeile 2");
    assert_eq!(doc.line_ending(), LineEnding::CrLf);
    doc.insert(doc.len(), "€".as_bytes()).unwrap();
    doc.save_blocking(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), encode("Grüße\r\nZeile 2€"));
}

#[test]
fn windows_1252_round_trips_and_reports_unmappable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w.txt");
    fs::write(&path, b"caf\xe9 \x80\n").unwrap();
    let mut doc = Document::open(&path, &OpenOptions::default(), &Progress::default()).unwrap();
    assert_eq!(doc.encoding().name(), "windows-1252");
    assert_eq!(doc.read_string_lossy(0..doc.len()), "café €\n");
    let copy = dir.path().join("copy.txt");
    doc.save_blocking(&copy).unwrap();
    assert_eq!(fs::read(&copy).unwrap(), b"caf\xe9 \x80\n");
    assert_eq!(doc.path(), Some(copy.as_path()));
    doc.insert(0, "Ω".as_bytes()).unwrap();
    let prepared = doc.prepare_save(&copy).run(&Progress::default()).unwrap();
    assert_eq!(prepared.unmappable(), 1);
    doc.finish_save(prepared).unwrap();
    assert_eq!(fs::read(&copy).unwrap(), b"?caf\xe9 \x80\n");
}

#[test]
fn large_non_utf8_file_is_transcoded_through_a_mapped_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("latin.txt");
    let raw: Vec<u8> =
        (0..400).flat_map(|i| format!("Zeile {i} \u{fc}\n").chars().map(|c| c as u8).collect::<Vec<_>>()).collect();
    fs::write(&path, &raw).unwrap();
    let mut doc = open_with(&path, OpenOptions::default(), SMALL);
    assert_eq!(doc.source_kind(), SourceKind::Mapped);
    assert_eq!(doc.encoding().name(), "windows-1252");
    assert_eq!(doc.read_string_lossy(0..15), "Zeile 0 ü\nZeil");
    wait_index(&mut doc);
    assert_eq!(doc.line_count(), Some(401));
    doc.insert(0, b"#").unwrap();
    doc.save_blocking(&path).unwrap();
    let mut expected = b"#".to_vec();
    expected.extend_from_slice(&raw);
    assert_eq!(fs::read(&path).unwrap(), expected);
}

#[test]
fn utf8_bom_is_hidden_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bom.txt");
    fs::write(&path, b"\xEF\xBB\xBFline1\nline2").unwrap();
    for limit in [MEMORY_LIMIT, 4] {
        let mut doc = open_with(&path, OpenOptions::default(), limit);
        assert_eq!(doc.encoding().name(), "UTF-8 BOM");
        assert_eq!(content(&doc), b"line1\nline2");
        wait_index(&mut doc);
        check_lines(&doc);
        doc.save_blocking(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\xEF\xBB\xBFline1\nline2");
    }
}

#[test]
fn atomic_save_over_a_mapped_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mapped.txt");
    let text = sample_text(500);
    fs::write(&path, &text).unwrap();
    let mut doc = open_with(&path, OpenOptions::default(), SMALL);
    assert_eq!(doc.source_kind(), SourceKind::Mapped);
    doc.insert(100, b"INSERTED\n").unwrap();
    let mut expected = text.clone();
    expected.splice(100..100, b"INSERTED\n".iter().copied());
    doc.save_blocking(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), expected);
    assert_eq!(doc.source_kind(), SourceKind::Mapped);
    assert!(!doc.is_dirty());
    assert_eq!(content(&doc), expected);
    assert_eq!(doc.check_disk(), DiskState::Unchanged);
    wait_index(&mut doc);
    check_lines(&doc);

    // A pinning snapshot makes the save Busy, without temp leftovers.
    doc.delete(0..5).unwrap();
    let snapshot = doc.snapshot();
    let prepared = doc.prepare_save(&path).run(&Progress::default()).unwrap();
    assert!(matches!(doc.finish_save(prepared), Err(CoreError::Busy(_))));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(fs::read(&path).unwrap(), expected);
    drop(snapshot);
    doc.save_blocking(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), &expected[5..]);

    // Edits during a mapped save would lose their source bytes.
    let prepared = doc.prepare_save(&path).run(&Progress::default()).unwrap();
    doc.insert(0, b"late").unwrap();
    assert!(matches!(doc.finish_save(prepared), Err(CoreError::Busy(_))));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    assert!(content(&doc).starts_with(b"late"));
}

#[test]
fn snapshot_write_raw_is_byte_exact() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::from_bytes(vec![0, 159, 146, 150, b'\r']);
    doc.insert(1, &[0xFF]).unwrap();
    let dest = dir.path().join("raw.bin");
    doc.snapshot().write_raw(&dest, &Progress::default()).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), [0, 0xFF, 159, 146, 150, b'\r']);
}

#[test]
fn paged_source_reads_across_pages_and_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paged.txt");
    let text = sample_text(20_000);
    fs::write(&path, &text).unwrap();
    let opts = OpenOptions { no_lock: true, ..OpenOptions::default() };
    let mut doc = open_with(&path, opts, SMALL);
    assert_eq!(doc.source_kind(), SourceKind::Paged);
    assert!(doc.is_read_only());
    assert!(matches!(doc.insert(0, b"x"), Err(CoreError::ReadOnly)));
    assert_eq!(content(&doc), text);
    let mut part = Vec::new();
    doc.read(65_530..65_545, &mut part);
    assert_eq!(part, text[65_530..65_545]);
    wait_index(&mut doc);
    assert_eq!(doc.line_count(), Some(20_001));
    let line = doc.line_to_offset(12_345).unwrap();
    assert_eq!(doc.read_string_lossy(line..doc.line_end_at(line)), "line 12345 some text");
    assert_eq!(doc.offset_to_line(line + 3), Some(12_345));
}

#[test]
fn binary_files_are_detected_and_kept_raw() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.bin");
    let raw: Vec<u8> = (0..=255u8).collect();
    fs::write(&path, &raw).unwrap();
    let mut doc = Document::open(&path, &OpenOptions::default(), &Progress::default()).unwrap();
    assert!(doc.is_binary());
    assert_eq!(content(&doc), raw);
    doc.replace(0..1, &[0xAA]).unwrap();
    doc.save_blocking(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap()[..2], [0xAA, 1]);
}

#[test]
fn detection_heuristics() {
    assert_eq!(detect_encoding("grüß".as_bytes()), TextEncoding::UTF8);
    assert_eq!(detect_encoding(&"grüß".as_bytes()[..5]), TextEncoding::UTF8, "truncated tail is still UTF-8");
    assert_eq!(detect_encoding(b"gr\xfc\xdf").name(), "windows-1252");
    let le: Vec<u8> = "hello world".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let be: Vec<u8> = "hello world".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    assert_eq!(detect_encoding(&le).name(), "UTF-16LE");
    assert_eq!(detect_encoding(&be).name(), "UTF-16BE");
    assert!(!looks_binary(&le));
    assert!(looks_binary(b"MZ\x90\x00\x03\x00\x00\x00"));
    assert!(!looks_binary(b"plain text\twith tab\r\n"));
    assert_eq!(detect_line_ending(b"a\r\nb\r\nc\n"), Some(LineEnding::CrLf));
    assert_eq!(detect_line_ending(b"a\nb\rc\n"), Some(LineEnding::Lf));
    assert_eq!(detect_line_ending(b"no breaks"), None);
}

#[test]
fn convert_line_endings_is_one_undo_step() {
    let mut doc = Document::from_bytes(b"a\r\nb\nc\rd".to_vec());
    doc.convert_line_endings(LineEnding::CrLf).unwrap();
    assert_eq!(content(&doc), b"a\r\nb\r\nc\r\nd");
    assert_eq!(doc.line_ending(), LineEnding::CrLf);
    doc.convert_line_endings(LineEnding::Lf).unwrap();
    assert_eq!(content(&doc), b"a\nb\nc\nd");
    doc.undo();
    doc.undo();
    assert_eq!(content(&doc), b"a\r\nb\nc\rd");
    let mut big = Document::from_bytes(vec![b'\n'; 2048]);
    big.memory_limit_for_tests(1024);
    assert!(matches!(big.convert_line_endings(LineEnding::CrLf), Err(CoreError::TooLarge(_))));
}

/// Places `needle` so it straddles each 4 MiB window boundary.
fn haystack_with_needles(needle: &[u8]) -> (Vec<u8>, Vec<u64>) {
    let mut hay = vec![b'.'; (9 << 20) + 100];
    let spots = vec![3, (4 << 20) - 3, (8 << 20) - 1, (9 << 20) + 90];
    spots.iter().for_each(|&s| hay[s as usize..s as usize + needle.len()].copy_from_slice(needle));
    (hay, spots)
}

fn collect_all(searcher: &Searcher, snap: &Snapshot) -> Vec<u64> {
    let mut found = Vec::new();
    searcher.find_all(snap, 0..snap.len(), &Progress::default(), usize::MAX, &mut |m| found.push(m.start)).unwrap();
    found
}

#[test]
fn search_finds_matches_across_window_boundaries_both_ways() {
    let (hay, spots) = haystack_with_needles(b"needle");
    let mut doc = Document::from_bytes(hay);
    // Split the text into pieces so windows also cross piece borders.
    doc.insert(5 << 20, b"").unwrap();
    doc.replace((6 << 20)..(6 << 20) + 1, b"#").unwrap();
    let snap = doc.snapshot();
    let patterns = [
        SearchPattern::Text { pattern: "needle".into(), regex: false, case_sensitive: true, whole_word: false },
        SearchPattern::Text { pattern: "NEEDLE".into(), regex: false, case_sensitive: false, whole_word: true },
        SearchPattern::Text { pattern: "ne+dle".into(), regex: true, case_sensitive: true, whole_word: false },
        SearchPattern::Bytes(b"needle".to_vec()),
    ];
    patterns.iter().for_each(|p| {
        let searcher = Searcher::new(p).unwrap();
        assert_eq!(collect_all(&searcher, &snap), spots, "{p:?}");
        let mut forward = Vec::new();
        let mut from = 0;
        while let Some(m) = searcher.find_next(&snap, from, &Progress::default()).unwrap() {
            assert_eq!(m.end - m.start, 6);
            forward.push(m.start);
            from = m.end;
        }
        assert_eq!(forward, spots, "{p:?}");
        let mut backward = Vec::new();
        let mut before = snap.len();
        while let Some(m) = searcher.find_prev(&snap, before, &Progress::default()).unwrap() {
            backward.push(m.start);
            before = m.start;
        }
        backward.reverse();
        assert_eq!(backward, spots, "{p:?}");
    });
}

#[test]
fn search_respects_whole_word_limits_and_cancel() {
    let snap = Document::from_bytes(b"needles needle Needle".to_vec()).snapshot();
    let word = Searcher::new(&SearchPattern::Text {
        pattern: "needle".into(),
        regex: false,
        case_sensitive: false,
        whole_word: true,
    })
    .unwrap();
    assert_eq!(collect_all(&word, &snap), [8, 15]);
    let mut seen = 0;
    let n = word.find_all(&snap, 0..snap.len(), &Progress::default(), 1, &mut |_| seen += 1).unwrap();
    assert_eq!((n, seen), (1, 1));
    let cancelled = Progress::new();
    cancelled.cancel();
    assert!(matches!(word.find_next(&snap, 0, &cancelled), Err(CoreError::Cancelled)));
    assert!(matches!(Searcher::new(&SearchPattern::Bytes(Vec::new())), Err(CoreError::InvalidPattern(_))));
    let bad = SearchPattern::Text { pattern: "(".into(), regex: true, case_sensitive: true, whole_word: false };
    assert!(matches!(Searcher::new(&bad), Err(CoreError::InvalidPattern(_))));
}

#[test]
fn regex_replacement_expands_groups() {
    let snap = Document::from_bytes(b"mail jens@example now\r\nend".to_vec()).snapshot();
    let p = SearchPattern::Text {
        pattern: r"(\w+)@(?P<host>\w+)$".into(),
        regex: true,
        case_sensitive: true,
        whole_word: false,
    };
    assert!(Searcher::new(&p).unwrap().find_next(&snap, 0, &Progress::default()).unwrap().is_none());
    let p = SearchPattern::Text {
        pattern: r"(\w+)@(?P<host>\w+) now$".into(),
        regex: true,
        case_sensitive: true,
        whole_word: false,
    };
    let searcher = Searcher::new(&p).unwrap();
    let m = searcher.find_next(&snap, 0, &Progress::default()).unwrap().unwrap();
    assert_eq!(m, 5..21);
    assert_eq!(searcher.expand_replacement(&snap, m, "$1 at ${host} ($$)"), b"jens at example ($)");
}

#[test]
fn append_from_disk_follows_a_growing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.txt");
    fs::write(&path, b"line1\n").unwrap();
    let mut doc = Document::open(&path, &OpenOptions::default(), &Progress::default()).unwrap();
    assert_eq!(doc.append_from_disk().unwrap(), 0);
    fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"line2\n").unwrap();
    assert_eq!(doc.check_disk(), DiskState::Grew(12));
    let rev = doc.revision();
    assert_eq!(doc.append_from_disk().unwrap(), 6);
    assert_eq!(content(&doc), b"line1\nline2\n");
    assert_eq!(doc.changed_since(rev), Some(6));
    assert_eq!(doc.line_count(), Some(3));
    assert!(!doc.is_dirty());
    assert_eq!(doc.check_disk(), DiskState::Unchanged);
    doc.insert(0, b"x").unwrap();
    fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"line3\n").unwrap();
    assert_eq!(doc.append_from_disk().unwrap(), 0, "dirty documents are not touched");
    fs::remove_file(&path).unwrap();
    assert_eq!(doc.check_disk(), DiskState::Deleted);
}

#[test]
fn reload_discards_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.txt");
    fs::write(&path, b"disk").unwrap();
    let mut doc = Document::open(&path, &OpenOptions::default(), &Progress::default()).unwrap();
    doc.insert(0, b"edit ").unwrap();
    fs::write(&path, b"changed on disk").unwrap();
    doc.reload(&Progress::default()).unwrap();
    assert_eq!(content(&doc), b"changed on disk");
    assert!(!doc.is_dirty());
    assert!(!doc.can_undo());
}
