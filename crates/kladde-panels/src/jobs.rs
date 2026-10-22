use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use kladde_core::{
    looks_binary, CoreError, Document, OpenMode, OpenOptions, Progress, SearchPattern, Searcher, Snapshot,
};
use kladde_tools::{HashAlgo, StreamHasher};

/// Bytes inspected to decide whether a file is binary.
const BINARY_SAMPLE: u64 = 8 << 10;
const READ_CHUNK: u64 = 1 << 20;
const LINE_TEXT_CHARS: usize = 300;
/// Context kept before a match in `line_text`.
const PREFIX_CHARS: usize = 80;
/// Bytes read around a match; enough for `LINE_TEXT_CHARS` even with 4-byte chars.
const BEFORE_BYTES: u64 = 4 * PREFIX_CHARS as u64;
const AFTER_BYTES: u64 = 4 * LINE_TEXT_CHARS as u64;

/// One-shot result channel of a worker thread.
struct Outcome<T> {
    rx: Receiver<T>,
    taken: bool,
}

impl<T> Outcome<T> {
    fn poll(&mut self, what: &str) -> Option<Result<T, String>> {
        let got = match self.rx.try_recv() {
            _ if self.taken => None,
            Ok(v) => Some(Ok(v)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(format!("{what} stopped unexpectedly"))),
        };
        self.taken |= got.is_some();
        got
    }
}

fn spawn_outcome<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Outcome<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // The receiver is gone when the job was dropped; nobody wants the result then.
        drop(tx.send(work()));
    });
    Outcome { rx, taken: false }
}

/// `find_all` over one snapshot on a worker thread.
pub struct DocSearchJob {
    progress: Arc<Progress>,
    outcome: Outcome<Result<Vec<Range<u64>>, String>>,
}

impl DocSearchJob {
    pub fn start(s: Snapshot, searcher: Arc<Searcher>, max: usize) -> Self {
        let progress = Progress::new();
        let p = Arc::clone(&progress);
        let outcome = spawn_outcome(move || {
            let mut found = Vec::new();
            let result = searcher.find_all(&s, 0..s.len(), &p, max, &mut |m| found.push(m));
            p.finish();
            result.map(|_| found).map_err(|e| e.to_string())
        });
        DocSearchJob { progress, outcome }
    }

    pub fn progress(&self) -> f32 {
        self.progress.fraction()
    }

    /// The result once, when the search ended.
    pub fn poll(&mut self) -> Option<Result<Vec<Range<u64>>, String>> {
        self.outcome.poll("search").map(|r| r.and_then(std::convert::identity))
    }

    pub fn cancel(&self) {
        self.progress.cancel();
    }
}

/// A match found by a files search; `line` is 0-based, `match_cols` is the byte range of the match in
/// `line_text` (clipped when the line was shortened).
#[derive(Clone, Debug, PartialEq)]
pub struct FileMatch {
    pub path: PathBuf,
    pub offset: u64,
    pub line: u64,
    pub line_text: String,
    pub match_cols: Range<usize>,
}

enum FilesMsg {
    Match(FileMatch),
    Error(String),
}

/// Searches many files on a worker thread; files are opened read-only without locks, binaries are skipped.
pub struct FilesSearchJob {
    progress: Arc<Progress>,
    current: Arc<Mutex<Arc<Progress>>>,
    rx: Receiver<FilesMsg>,
    errors: Vec<String>,
    disconnected: bool,
    max: usize,
    files: usize,
}

impl FilesSearchJob {
    pub fn start(files: Vec<PathBuf>, pattern: SearchPattern, max: usize) -> Self {
        let progress = Progress::new();
        let current = Arc::new(Mutex::new(Progress::new()));
        let (tx, rx) = mpsc::channel();
        let count = files.len();
        let (p, cur) = (Arc::clone(&progress), Arc::clone(&current));
        thread::spawn(move || {
            search_files(&files, &pattern, max, &p, &cur, &tx);
            p.finish();
        });
        FilesSearchJob { progress, current, rx, errors: Vec::new(), disconnected: false, max, files: count }
    }

    /// Files done plus the fraction of the current file, over all files.
    pub fn progress(&self) -> f32 {
        let file = self.current.lock().map_or(0.0, |c| c.fraction());
        let share = if self.files == 0 { 0.0 } else { file / self.files as f32 };
        (self.progress.fraction() + share).min(1.0)
    }

    /// Matches found since the last call; errors are collected in `errors()`.
    pub fn drain(&mut self) -> Vec<FileMatch> {
        let mut out = Vec::new();
        let mut more = true;
        while more {
            match self.rx.try_recv() {
                Ok(FilesMsg::Match(m)) => out.push(m),
                Ok(FilesMsg::Error(e)) => self.errors.push(e),
                Err(TryRecvError::Empty) => more = false,
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    more = false;
                }
            }
        }
        out
    }

    pub fn is_done(&self) -> bool {
        self.progress.is_finished() || self.disconnected
    }

    pub fn cancel(&self) {
        self.progress.cancel();
        if let Ok(c) = self.current.lock() {
            c.cancel();
        }
    }

    /// Files that could not be searched, as "path: reason".
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn max(&self) -> usize {
        self.max
    }
}

fn search_files(
    files: &[PathBuf],
    pattern: &SearchPattern,
    max: usize,
    progress: &Progress,
    current: &Mutex<Arc<Progress>>,
    tx: &Sender<FilesMsg>,
) {
    progress.set_total(files.len() as u64);
    match Searcher::new(pattern) {
        Err(e) => drop(tx.send(FilesMsg::Error(e.to_string()))),
        Ok(searcher) => {
            let mut remaining = max;
            let mut iter = files.iter();
            let mut next = iter.next();
            while let Some(path) = next.filter(|_| remaining > 0 && !progress.is_cancelled()) {
                let file_progress = Progress::new();
                if let Ok(mut c) = current.lock() {
                    *c = Arc::clone(&file_progress);
                }
                match search_file(path, &searcher, remaining, &file_progress) {
                    Ok(found) => {
                        remaining -= found.len();
                        found.into_iter().for_each(|m| drop(tx.send(FilesMsg::Match(m))));
                    }
                    Err(CoreError::Cancelled) => {}
                    Err(e) => drop(tx.send(FilesMsg::Error(format!("{}: {e}", path.display())))),
                }
                progress.add(1);
                next = iter.next();
            }
        }
    }
}

fn search_file(path: &Path, searcher: &Searcher, max: usize, progress: &Progress) -> Result<Vec<FileMatch>, CoreError> {
    let mut sample = Vec::new();
    File::open(path)?.take(BINARY_SAMPLE).read_to_end(&mut sample)?;
    if looks_binary(&sample) {
        Ok(Vec::new())
    } else {
        let opts = OpenOptions { mode: OpenMode::Text { encoding: None }, read_only: true, no_lock: true };
        let mut doc = Document::open(path, &opts, progress)?;
        let snapshot = doc.snapshot();
        let mut ranges = Vec::new();
        if !doc.is_binary() {
            searcher.find_all(&snapshot, 0..snapshot.len(), progress, max, &mut |m| ranges.push(m))?;
        }
        match doc.take_error() {
            Some(e) => Err(e),
            None => Ok(file_matches(path, &snapshot, &ranges)),
        }
    }
}

/// Builds results with 0-based line numbers (newlines counted up to each match) and a short line excerpt.
/// `ranges` must be sorted by start.
pub fn file_matches(path: &Path, s: &Snapshot, ranges: &[Range<u64>]) -> Vec<FileMatch> {
    let mut pos = 0u64;
    let mut line = 0u64;
    let mut buf = Vec::new();
    ranges
        .iter()
        .map(|m| {
            while pos < m.start {
                let end = m.start.min(pos + READ_CHUNK);
                buf.clear();
                s.read(pos..end, &mut buf);
                line += buf.iter().filter(|&&b| b == b'\n').count() as u64;
                pos = end;
            }
            let (line_text, match_cols) = excerpt(s, m);
            FileMatch { path: path.to_path_buf(), offset: m.start, line, line_text, match_cols }
        })
        .collect()
}

fn excerpt(s: &Snapshot, m: &Range<u64>) -> (String, Range<usize>) {
    let lo = m.start.saturating_sub(BEFORE_BYTES);
    let shown_end = m.end.min(m.start + AFTER_BYTES);
    let mut buf = Vec::new();
    s.read(lo..shown_end + AFTER_BYTES, &mut buf);
    let ms = ((m.start - lo) as usize).min(buf.len());
    let me = ((shown_end - lo) as usize).clamp(ms, buf.len());
    let line_start = buf[..ms].iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
    // A window cut inside a UTF-8 sequence starts at the next char.
    let start = line_start + buf[line_start..ms].iter().take_while(|&&b| (0x80..0xC0).contains(&b)).count();
    let end = buf[ms..].iter().position(|&b| b == b'\n' || b == b'\r').map_or(buf.len(), |p| ms + p);
    let me = me.min(end);
    let prefix = String::from_utf8_lossy(&buf[start..ms]);
    let prefix = prefix.trim_start();
    let prefix_chars = prefix.chars().count();
    let prefix: String = prefix.chars().skip(prefix_chars.saturating_sub(PREFIX_CHARS)).collect();
    let mut text = prefix.clone();
    text.push_str(&String::from_utf8_lossy(&buf[ms..me]));
    let match_end = text.len();
    text.push_str(&String::from_utf8_lossy(&buf[me..end]));
    if let Some((cut, _)) = text.char_indices().nth(LINE_TEXT_CHARS) {
        text.truncate(cut);
    }
    let cols = prefix.len().min(text.len())..match_end.min(text.len());
    (text, cols)
}

/// Hashes a snapshot range with several algorithms in one pass on a worker thread.
pub struct HashJob {
    progress: Arc<Progress>,
    outcome: Outcome<Result<Vec<(HashAlgo, String)>, String>>,
}

impl HashJob {
    pub fn start(s: Snapshot, range: Range<u64>, algos: Vec<HashAlgo>) -> Self {
        let progress = Progress::new();
        let p = Arc::clone(&progress);
        let outcome = spawn_outcome(move || {
            let result = hash_range(&s, range, &algos, &p);
            p.finish();
            result
        });
        HashJob { progress, outcome }
    }

    pub fn progress(&self) -> f32 {
        self.progress.fraction()
    }

    /// Lowercase hex digests in the order of `algos`, once, when done.
    pub fn poll(&mut self) -> Option<Result<Vec<(HashAlgo, String)>, String>> {
        self.outcome.poll("hashing").map(|r| r.and_then(std::convert::identity))
    }

    pub fn cancel(&self) {
        self.progress.cancel();
    }
}

fn hash_range(
    s: &Snapshot,
    range: Range<u64>,
    algos: &[HashAlgo],
    p: &Progress,
) -> Result<Vec<(HashAlgo, String)>, String> {
    let end = range.end.min(s.len());
    let start = range.start.min(end);
    p.set_total(end - start);
    let mut hashers: Vec<StreamHasher> = algos.iter().map(|a| StreamHasher::new(*a)).collect();
    let mut buf = Vec::new();
    let mut pos = start;
    while pos < end && !p.is_cancelled() {
        let chunk_end = end.min(pos + READ_CHUNK);
        buf.clear();
        s.read(pos..chunk_end, &mut buf);
        hashers.iter_mut().for_each(|h| h.update(&buf));
        pos = chunk_end;
        p.set_done(pos - start);
    }
    if p.is_cancelled() {
        Err(CoreError::Cancelled.to_string())
    } else {
        Ok(algos.iter().copied().zip(hashers.into_iter().map(StreamHasher::finish)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn wait_files(job: &mut FilesSearchJob) -> Vec<FileMatch> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut all = Vec::new();
        while !job.is_done() && Instant::now() < deadline {
            all.extend(job.drain());
            thread::sleep(Duration::from_millis(5));
        }
        all.extend(job.drain());
        assert!(job.is_done(), "search did not finish");
        all
    }

    fn text(p: &str) -> SearchPattern {
        SearchPattern::Text { pattern: p.to_string(), regex: false, case_sensitive: true, whole_word: false }
    }

    #[test]
    fn files_search_lines_and_binaries() {
        let dir = tempfile::tempdir().expect("temp dir");
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.txt");
        fs::write(&a, "needle first\r\nnothing\r\n\r\n    two needle needle\nend").expect("write a");
        fs::write(&b, b"needle\0\0\0\x01\x02binary needle").expect("write b");
        fs::write(&c, format!("{}needle", "x".repeat(1000))).expect("write c");
        let missing = dir.path().join("missing.txt");
        let mut job = FilesSearchJob::start(vec![a.clone(), b, missing, c.clone()], text("needle"), 100);
        let found = wait_files(&mut job);
        let summary: Vec<(PathBuf, u64, u64)> = found.iter().map(|m| (m.path.clone(), m.line, m.offset)).collect();
        assert_eq!(summary, vec![(a.clone(), 0, 0), (a.clone(), 3, 33), (a.clone(), 3, 40), (c.clone(), 0, 1000)]);
        assert_eq!(found[1].line_text, "two needle needle");
        assert_eq!(&found[1].line_text[found[1].match_cols.clone()], "needle");
        assert_eq!(&found[2].line_text[found[2].match_cols.clone()], "needle");
        assert!(found[3].line_text.chars().count() <= LINE_TEXT_CHARS);
        assert_eq!(&found[3].line_text[found[3].match_cols.clone()], "needle");
        assert_eq!(job.errors().len(), 1);
    }

    #[test]
    fn files_search_stops_at_max() {
        let dir = tempfile::tempdir().expect("temp dir");
        let a = dir.path().join("a.txt");
        fs::write(&a, "x\nx\nx\nx\n").expect("write");
        let mut job = FilesSearchJob::start(vec![a.clone(), a], text("x"), 3);
        assert_eq!(wait_files(&mut job).len(), 3);
    }

    #[test]
    fn hash_job_known_vectors() {
        let doc = Document::from_bytes(b"xxabcxx".to_vec());
        let mut job = HashJob::start(doc.snapshot(), 2..5, HashAlgo::all().to_vec());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut result = job.poll();
        while result.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
            result = job.poll();
        }
        let digests = result.expect("hash finished").expect("hash ok");
        assert_eq!(
            digests,
            vec![
                (HashAlgo::Md5, "900150983cd24fb0d6963f7d28e17f72".to_string()),
                (HashAlgo::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d".to_string()),
                (HashAlgo::Sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string()),
                (HashAlgo::Crc32, "352441c2".to_string()),
            ]
        );
        assert!(job.poll().is_none());
    }

    #[test]
    fn doc_search_finds_all() {
        let doc = Document::from_bytes(b"ab ab ab".to_vec());
        let searcher = Arc::new(Searcher::new(&text("ab")).expect("valid"));
        let mut job = DocSearchJob::start(doc.snapshot(), searcher, 2);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut result = job.poll();
        while result.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
            result = job.poll();
        }
        assert_eq!(result.expect("finished"), Ok(vec![0..2, 3..5]));
    }
}
