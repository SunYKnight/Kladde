use std::fs::{self, File};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use memchr::{memchr, memchr_iter, memrchr};

use crate::encoding::{convert_line_endings, decode_chunk, detect_encoding, detect_line_ending, looks_binary};
use crate::history::{Change, History, Typed};
use crate::index::{self, OrigIndex};
use crate::save::{PreparedSave, SaveJob, Snapshot};
use crate::source::{self, read_exact_at, read_full, Source};
use crate::text::{Buf, Piece, Text};
use crate::{CoreError, LineEnding, Progress, TextEncoding, MEMORY_LIMIT};

/// Bytes inspected for encoding, binary and line ending detection.
const SAMPLE: u64 = 64 << 10;
const IO_CHUNK: usize = 4 << 20;

#[derive(Clone, Debug)]
pub enum OpenMode {
    Text { encoding: Option<TextEncoding> },
    Binary,
}

#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub mode: OpenMode,
    pub read_only: bool,
    pub no_lock: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions { mode: OpenMode::Text { encoding: None }, read_only: false, no_lock: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Memory,
    Mapped,
    Paged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskState {
    Unchanged,
    Modified,
    Grew(u64),
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: SystemTime,
}

struct IndexJob {
    progress: Arc<Progress>,
    handle: JoinHandle<Result<OrigIndex, CoreError>>,
}

/// Result of reading a file from disk.
struct Loaded {
    source: Source,
    /// Offset of the first content byte (skips a UTF-8 BOM).
    base: u64,
    binary: bool,
    encoding: TextEncoding,
    stamp: Stamp,
}

/// A text or binary buffer: piece table over a file source with undo, line index and saving.
pub struct Document {
    path: Option<PathBuf>,
    binary: bool,
    read_only: bool,
    no_lock: bool,
    memory_limit: u64,
    text: Text,
    base: u64,
    index: Option<OrigIndex>,
    index_job: Option<IndexJob>,
    encoding: TextEncoding,
    saved_encoding: TextEncoding,
    line_ending: LineEnding,
    history: History,
    stamp: Option<Stamp>,
    error: Option<CoreError>,
}

impl Document {
    /// Untitled, UTF-8, platform line ending.
    pub fn new_empty() -> Document {
        Document::from_memory(Vec::new(), MEMORY_LIMIT)
    }

    /// Untitled document holding `bytes` (hot-exit restore); dirty unless empty.
    pub fn from_bytes(bytes: Vec<u8>) -> Document {
        let mut doc = Document::from_memory(bytes, MEMORY_LIMIT);
        doc.detect_line_ending();
        if !doc.is_empty() {
            doc.history.mark_unsaved();
        }
        doc
    }

    fn from_memory(bytes: Vec<u8>, memory_limit: u64) -> Document {
        let source = Arc::new(Source::memory(bytes));
        let index = index::build(&source, 0, &Progress::default()).expect("in-memory index build cannot fail");
        let mut doc = Document {
            path: None,
            binary: false,
            read_only: false,
            no_lock: false,
            memory_limit,
            text: Text::new(source, 0),
            base: 0,
            index: Some(index),
            index_job: None,
            encoding: TextEncoding::UTF8,
            saved_encoding: TextEncoding::UTF8,
            line_ending: LineEnding::platform(),
            history: History::new(),
            stamp: None,
            error: None,
        };
        doc.fill_counts();
        doc
    }

    /// <= MEMORY_LIMIT: read into RAM. Larger: memory-mapped; with `no_lock`: paged positioned reads with a
    /// small LRU page cache (read-only). Text mode with non-UTF-8 encoding is transcoded to UTF-8 (RAM if small,
    /// else a temp file that is then mapped); `encoding()` keeps the original so saving re-encodes.
    /// Binary mode: raw bytes, never transcoded. Line index for large sources builds on a background thread.
    pub fn open(path: &Path, opts: &OpenOptions, progress: &Progress) -> Result<Document, CoreError> {
        Document::open_with_limit(path, opts, progress, MEMORY_LIMIT)
    }

    /// `open` with an injectable RAM/mapping threshold so tests can force mapped and paged sources.
    pub(crate) fn open_with_limit(
        path: &Path,
        opts: &OpenOptions,
        progress: &Progress,
        memory_limit: u64,
    ) -> Result<Document, CoreError> {
        let loaded = load(path, &opts.mode, opts.no_lock, progress, memory_limit)?;
        let mut doc = Document::from_memory(Vec::new(), memory_limit);
        doc.path = Some(path.to_path_buf());
        doc.read_only = opts.read_only;
        doc.no_lock = opts.no_lock;
        doc.install(loaded)?;
        doc.detect_line_ending();
        progress.finish();
        Ok(doc)
    }

    /// Replaces the content with a freshly loaded file; history is untouched.
    fn install(&mut self, loaded: Loaded) -> Result<(), CoreError> {
        self.stop_index();
        let source = Arc::new(loaded.source);
        self.text = Text::new(Arc::clone(&source), loaded.base);
        self.base = loaded.base;
        self.binary = loaded.binary;
        self.encoding = loaded.encoding;
        self.saved_encoding = loaded.encoding;
        self.stamp = Some(loaded.stamp);
        self.index = None;
        if source.kind() == SourceKind::Memory {
            self.index = Some(index::build(&source, loaded.base, &Progress::default())?);
            self.fill_counts();
        } else {
            self.index_job = Some(spawn_index(source, loaded.base)?);
        }
        Ok(())
    }

    fn detect_line_ending(&mut self) {
        let mut sample = Vec::new();
        self.text.read(0..SAMPLE, &mut sample);
        self.line_ending = detect_line_ending(&sample).unwrap_or(LineEnding::platform());
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_binary(&self) -> bool {
        self.binary
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only || self.source_kind() == SourceKind::Paged
    }

    pub fn source_kind(&self) -> SourceKind {
        self.text.source.kind()
    }

    pub fn len(&self) -> u64 {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// +1 on every content change, undo/redo included.
    pub fn revision(&self) -> u64 {
        self.history.revision()
    }

    /// Lowest offset changed after `revision`; Some(0) if history is too old; None if unchanged.
    pub fn changed_since(&self, revision: u64) -> Option<u64> {
        self.history.changed_since(revision)
    }

    /// Content differs from the saved state, or the encoding was changed.
    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty() || (!self.binary && self.encoding != self.saved_encoding)
    }

    pub fn encoding(&self) -> TextEncoding {
        self.encoding
    }

    /// Takes effect on the next save.
    pub fn set_encoding(&mut self, e: TextEncoding) {
        self.encoding = e;
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Terminator for newly inserted lines.
    pub fn set_line_ending(&mut self, le: LineEnding) {
        self.line_ending = le;
    }

    /// Rewrites all line breaks as `le` in one undo group; TooLarge above the RAM limit.
    pub fn convert_line_endings(&mut self, le: LineEnding) -> Result<(), CoreError> {
        let len = self.len();
        if self.is_read_only() {
            Err(CoreError::ReadOnly)
        } else if len > self.memory_limit {
            Err(CoreError::TooLarge(format!("{len} bytes exceed the {} byte limit for conversion", self.memory_limit)))
        } else {
            let mut bytes = Vec::with_capacity(len as usize);
            self.text.read_checked(0..len, &mut bytes)?;
            let converted = convert_line_endings(&bytes, le);
            if converted != bytes {
                self.history.begin_group();
                let result = self.edit(0..len, &converted, None);
                self.history.end_group();
                result?;
            }
            self.line_ending = le;
            Ok(())
        }
    }

    /// Appends the clamped `range` to `out`.
    pub fn read(&self, range: Range<u64>, out: &mut Vec<u8>) {
        self.text.read(range, out);
    }

    pub fn read_string_lossy(&self, range: Range<u64>) -> String {
        let mut bytes = Vec::new();
        self.read(range, &mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn byte_at(&self, off: u64) -> Option<u8> {
        self.text.byte_at(off)
    }

    /// First deferred error: failed background line index or a read error in an infallible API.
    pub fn take_error(&mut self) -> Option<CoreError> {
        self.error.take().or_else(|| self.text.source.take_error().map(CoreError::Io))
    }

    pub fn insert(&mut self, off: u64, bytes: &[u8]) -> Result<(), CoreError> {
        self.edit(off..off, bytes, typed_kind(bytes))
    }

    pub fn delete(&mut self, range: Range<u64>) -> Result<(), CoreError> {
        self.edit(range, &[], None)
    }

    pub fn replace(&mut self, range: Range<u64>, bytes: &[u8]) -> Result<(), CoreError> {
        self.edit(range, bytes, None)
    }

    fn edit(&mut self, range: Range<u64>, bytes: &[u8], typed: Option<Typed>) -> Result<(), CoreError> {
        let len = self.len();
        let s = range.start.min(len);
        let e = range.end.clamp(s, len);
        if self.is_read_only() {
            Err(CoreError::ReadOnly)
        } else {
            if s < e || !bytes.is_empty() {
                let change = self.build_change(s, e, bytes);
                Arc::make_mut(&mut self.text.list).splice(change.index, change.removed.len(), &change.inserted);
                self.history.record(change, typed);
            }
            Ok(())
        }
    }

    /// Piece-list splice replacing `s..e` with `bytes`.
    fn build_change(&mut self, s: u64, e: u64, bytes: &[u8]) -> Change {
        let new_piece = (!bytes.is_empty()).then(|| self.text.push_add(bytes));
        let list = &self.text.list;
        let pieces = list.pieces();
        let (i, i_start) = list.locate(s);
        let extend = match &new_piece {
            Some(np) => s == e && i > 0 && i_start == s && pieces[i - 1].followed_by(np),
            None => false,
        };
        let removed_len = e - s;
        let inserted_len = bytes.len() as u64;
        match new_piece.filter(|_| extend) {
            Some(np) => {
                let prev = pieces[i - 1];
                let grown = Piece { len: prev.len + np.len, nl: prev.nl.zip(np.nl).map(|(a, b)| a + b), ..prev };
                Change { index: i - 1, removed: vec![prev], inserted: vec![grown], pos: s, removed_len, inserted_len }
            }
            None => {
                let (j, j_start) = if e > s { list.locate(e - 1) } else { (i, i_start) };
                let k = if e > s {
                    j + 1
                } else if s > i_start {
                    i + 1
                } else {
                    i
                };
                let mut inserted = Vec::with_capacity(3);
                if s > i_start {
                    inserted.push(self.sub_piece(&pieces[i], 0..s - i_start));
                }
                inserted.extend(new_piece);
                if k > i {
                    let last = pieces[k - 1];
                    let cut = e - if e > s { j_start } else { i_start };
                    if cut < last.len {
                        inserted.push(self.sub_piece(&last, cut..last.len));
                    }
                }
                Change { index: i, removed: pieces[i..k].to_vec(), inserted, pos: s, removed_len, inserted_len }
            }
        }
    }

    fn sub_piece(&self, piece: &Piece, local: Range<u64>) -> Piece {
        let nl = self.recorded(self.newlines_in(piece, local.clone()));
        Piece { buf: piece.buf, start: piece.start + local.start, len: local.end - local.start, nl }
    }

    /// Newlines in `local` of `piece`; None for original bytes while the index is not ready.
    fn newlines_in(&self, piece: &Piece, local: Range<u64>) -> io::Result<Option<u64>> {
        match (piece.buf, &self.index) {
            (Buf::Add(_), _) => Ok(Some(memchr_iter(b'\n', self.text.add_bytes(piece, local)).count() as u64)),
            (Buf::Orig, Some(index)) => {
                Ok(Some(index.count(&self.text.source, piece.start + local.start, piece.start + local.end)?))
            }
            (Buf::Orig, None) => Ok(None),
        }
    }

    /// Unwraps a lookup result, parking I/O errors for `take_error`.
    fn recorded<T>(&self, result: io::Result<Option<T>>) -> Option<T> {
        match result {
            Ok(v) => v,
            Err(e) => {
                self.text.source.record_error(e);
                None
            }
        }
    }

    /// Computes missing newline counts of original pieces once the index exists.
    fn fill_counts(&mut self) {
        let missing = self.index.is_some() && self.text.list.pieces().iter().any(|p| p.nl.is_none());
        if missing {
            let counts: io::Result<Vec<Option<u64>>> = self
                .text
                .list
                .pieces()
                .iter()
                .map(|p| match p.nl {
                    Some(n) => Ok(Some(n)),
                    None => self.newlines_in(p, 0..p.len),
                })
                .collect();
            match counts {
                Ok(counts) => Arc::make_mut(&mut self.text.list).set_counts(&counts),
                Err(e) => self.error = Some(e.into()),
            }
        }
    }

    /// Starts a group; nestable, everything until the outermost end undoes as one step.
    pub fn begin_undo_group(&mut self) {
        self.history.begin_group();
    }

    pub fn end_undo_group(&mut self) {
        self.history.end_group();
    }

    /// Reverts the last step; returns a caret hint.
    pub fn undo(&mut self) -> Option<u64> {
        let step = self.history.take_undo()?;
        let list = Arc::make_mut(&mut self.text.list);
        step.changes.iter().rev().for_each(|c| list.splice(c.index, c.inserted.len(), &c.removed));
        let hint = step.changes.first().map(|c| c.pos + c.removed_len);
        self.history.bump(step.low());
        self.history.push_redo(step);
        self.fill_counts();
        hint
    }

    /// Reapplies the last undone step; returns a caret hint.
    pub fn redo(&mut self) -> Option<u64> {
        let step = self.history.take_redo()?;
        let list = Arc::make_mut(&mut self.text.list);
        step.changes.iter().for_each(|c| list.splice(c.index, c.removed.len(), &c.inserted));
        let hint = step.changes.last().map(|c| c.pos + c.inserted_len);
        self.history.bump(step.low());
        self.history.push_undo(step);
        self.fill_counts();
        hint
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    fn find_byte(&self, range: Range<u64>, byte: u8) -> Option<u64> {
        self.recorded(self.text.visit(range, &mut |at, chunk| memchr(byte, chunk).map(|p| at + p as u64)))
    }

    fn rfind_byte(&self, range: Range<u64>, byte: u8) -> Option<u64> {
        self.recorded(self.text.visit_rev(range, &mut |at, chunk| memrchr(byte, chunk).map(|p| at + p as u64)))
    }

    /// Start of the line containing `off`.
    pub fn line_start_at(&self, off: u64) -> u64 {
        self.rfind_byte(0..off.min(self.len()), b'\n').map_or(0, |p| p + 1)
    }

    /// Offset of the line terminator (the CR of a CRLF) or the document length.
    pub fn line_end_at(&self, off: u64) -> u64 {
        let len = self.len();
        match self.find_byte(off..len, b'\n') {
            Some(p) if p > 0 && self.byte_at(p - 1) == Some(b'\r') => p - 1,
            Some(p) => p,
            None => len,
        }
    }

    /// Offset after the next '\n' at or after `off`, or the length.
    pub fn next_line_start(&self, off: u64) -> u64 {
        let len = self.len();
        self.find_byte(off..len, b'\n').map_or(len, |p| p + 1)
    }

    /// Integrates a finished background line index; call once per frame.
    pub fn poll_line_index(&mut self) {
        if self.index_job.as_ref().is_some_and(|j| j.handle.is_finished()) {
            let job = self.index_job.take().expect("checked above");
            self.integrate(job);
        }
    }

    fn integrate(&mut self, job: IndexJob) {
        match job.handle.join() {
            Ok(Ok(index)) => {
                self.index = Some(index);
                self.fill_counts();
            }
            Ok(Err(CoreError::Cancelled)) => {}
            Ok(Err(e)) => self.error = Some(e),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Cancels and joins the background index so it releases the source.
    fn stop_index(&mut self) {
        if let Some(job) = self.index_job.take() {
            job.progress.cancel();
            self.integrate(job);
        }
    }

    fn restart_index(&mut self) -> Result<(), CoreError> {
        if self.index.is_none() && self.index_job.is_none() {
            self.index_job = Some(spawn_index(Arc::clone(&self.text.source), self.base)?);
        }
        Ok(())
    }

    /// Fraction of the background line index built; None when ready (or not building).
    pub fn line_index_progress(&self) -> Option<f32> {
        self.index_job.as_ref().map(|j| j.progress.fraction())
    }

    pub fn line_count(&self) -> Option<u64> {
        self.text.list.line_prefix().and_then(|p| p.last()).map(|n| n + 1)
    }

    /// Offset of the start of 0-based `line`.
    pub fn line_to_offset(&self, line: u64) -> Option<u64> {
        let list = &self.text.list;
        let prefix = list.line_prefix()?;
        let total = *prefix.last()?;
        if line == 0 {
            Some(0)
        } else if line > total {
            None
        } else {
            let i = prefix[1..].partition_point(|&c| c < line);
            let piece = list.pieces()[i];
            let k = line - prefix[i] - 1;
            let start = list.piece_start(i);
            self.recorded(self.nth_newline(&piece, k)).map(|p| start + p + 1)
        }
    }

    /// Local offset of the `k`-th (0-based) '\n' inside `piece`.
    fn nth_newline(&self, piece: &Piece, k: u64) -> io::Result<Option<u64>> {
        match (piece.buf, &self.index) {
            (Buf::Add(_), _) => {
                Ok(memchr_iter(b'\n', self.text.add_bytes(piece, 0..piece.len)).nth(k as usize).map(|p| p as u64))
            }
            (Buf::Orig, Some(index)) => {
                let source = &self.text.source;
                let n = index.newlines_before(source, piece.start)? + k + 1;
                Ok(index.after_newline(source, n)?.map(|after| after - 1 - piece.start))
            }
            (Buf::Orig, None) => Ok(None),
        }
    }

    /// 0-based line containing `off`.
    pub fn offset_to_line(&self, off: u64) -> Option<u64> {
        let list = &self.text.list;
        let prefix = list.line_prefix()?;
        let off = off.min(self.len());
        let (i, start) = list.locate(off);
        if i == list.pieces().len() {
            prefix.last().copied()
        } else {
            let piece = list.pieces()[i];
            self.recorded(self.newlines_in(&piece, 0..off - start)).map(|n| prefix[i] + n)
        }
    }

    /// Cheap immutable view for background work.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone() }
    }

    /// Cheap; run the returned job on a worker thread, then pass its result to `finish_save`.
    pub fn prepare_save(&self, target: &Path) -> SaveJob {
        let encoding = (!self.binary).then_some(self.encoding);
        SaveJob::new(self.snapshot(), target.to_path_buf(), encoding, self.history.revision())
    }

    /// Renames the temp file over the target, reopens from it when the source pins a file, marks clean and sets
    /// the path. Busy if the document changed during a reopening save or snapshots still pin the mapping.
    pub fn finish_save(&mut self, prepared: PreparedSave) -> Result<(), CoreError> {
        let unchanged = prepared.revision() == self.history.revision();
        let target = prepared.target().to_path_buf();
        let saved_encoding = prepared.encoding().unwrap_or(self.encoding);
        match self.text.source.pinned().map(Path::to_path_buf) {
            Some(old_path) => self.finish_pinned(prepared, unchanged, &old_path)?,
            None => {
                prepared.commit()?;
                if unchanged {
                    self.history.mark_saved();
                } else {
                    self.history.mark_unsaved();
                }
            }
        }
        self.path = Some(target.clone());
        self.saved_encoding = saved_encoding;
        self.stamp = Some(stamp_of(&fs::metadata(&target)?)?);
        Ok(())
    }

    /// The source holds a file open: release it, rename, reload from the new file.
    fn finish_pinned(&mut self, prepared: PreparedSave, unchanged: bool, old_path: &Path) -> Result<(), CoreError> {
        if unchanged {
            self.stop_index();
            if Arc::strong_count(&self.text.source) > 1 {
                let busy = CoreError::Busy("a search or other snapshot still uses the file".into());
                let error = match self.restart_index() {
                    Ok(()) => busy,
                    Err(e) => e,
                };
                Err(prepared.discard_with(error))
            } else {
                self.replace_pinned(prepared, old_path)
            }
        } else {
            // Newer edits reference the old file that the rename destroys.
            Err(prepared.discard_with(CoreError::Busy("document changed while saving".into())))
        }
    }

    fn replace_pinned(&mut self, prepared: PreparedSave, old_path: &Path) -> Result<(), CoreError> {
        let kind = self.source_kind();
        let len = self.text.source.len();
        // Windows cannot replace a mapped file; this drops the last reference.
        self.text.source = Arc::new(Source::memory(Vec::new()));
        let target = prepared.target().to_path_buf();
        match prepared.commit() {
            Ok(()) => {
                let mode = self.reopen_mode();
                let loaded = load(&target, &mode, self.no_lock, &Progress::default(), self.memory_limit);
                match loaded.and_then(|loaded| self.install(loaded)) {
                    Ok(()) => {
                        self.history.reset();
                        self.history.bump(0);
                        Ok(())
                    }
                    Err(e) => {
                        self.become_empty();
                        Err(e)
                    }
                }
            }
            Err(e) => match reattach(old_path, kind, len, self.memory_limit) {
                Ok(source) => {
                    self.text.source = Arc::new(source);
                    self.restart_index()?;
                    Err(e)
                }
                Err(re) => {
                    self.become_empty();
                    Err(CoreError::Io(io::Error::new(re.kind(), format!("{e}; reopening the original: {re}"))))
                }
            },
        }
    }

    /// Pieces can no longer be served from the released file; keep path and flags so the user can reload.
    fn become_empty(&mut self) {
        let mut empty = Document::from_memory(Vec::new(), self.memory_limit);
        empty.path = self.path.take();
        empty.read_only = self.read_only;
        empty.no_lock = self.no_lock;
        empty.history.bump(0);
        *self = empty;
    }

    fn reopen_mode(&self) -> OpenMode {
        if self.binary {
            OpenMode::Binary
        } else {
            OpenMode::Text { encoding: Some(self.encoding) }
        }
    }

    /// Writes to `target` on the calling thread.
    pub fn save_blocking(&mut self, target: &Path) -> Result<(), CoreError> {
        let prepared = self.prepare_save(target).run(&Progress::default())?;
        self.finish_save(prepared)
    }

    /// Re-reads the file with the current encoding; history is cleared.
    pub fn reload(&mut self, progress: &Progress) -> Result<(), CoreError> {
        let path = self.path.clone().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "document has no file"))?;
        let loaded = load(&path, &self.reopen_mode(), self.no_lock, progress, self.memory_limit)?;
        self.install(loaded)?;
        self.history.reset();
        self.history.bump(0);
        progress.finish();
        Ok(())
    }

    /// Compares size and modification time with the state recorded at open/save.
    pub fn check_disk(&self) -> DiskState {
        match (&self.path, &self.stamp) {
            (Some(path), Some(stamp)) => match fs::metadata(path) {
                Ok(meta) => {
                    let size = meta.len();
                    let same_time = meta.modified().is_ok_and(|m| m == stamp.modified);
                    if size == stamp.size && same_time {
                        DiskState::Unchanged
                    } else if size > stamp.size {
                        DiskState::Grew(size)
                    } else {
                        DiskState::Modified
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => DiskState::Deleted,
                // Unreadable metadata: a reload shows the real error.
                Err(_) => DiskState::Modified,
            },
            _ => DiskState::Unchanged,
        }
    }

    /// tail -f: appends bytes the file gained since open/save; only for clean documents. Returns bytes added.
    pub fn append_from_disk(&mut self) -> Result<u64, CoreError> {
        match (self.check_disk(), self.path.clone(), self.stamp) {
            (DiskState::Grew(size), Some(path), Some(stamp)) if !self.is_dirty() => {
                if size - stamp.size > self.memory_limit {
                    let before = self.len();
                    self.reload(&Progress::default())?;
                    Ok(self.len().saturating_sub(before))
                } else {
                    self.append_tail(&path, stamp, size)
                }
            }
            _ => Ok(0),
        }
    }

    fn append_tail(&mut self, path: &Path, stamp: Stamp, size: u64) -> Result<u64, CoreError> {
        let file = source::open_file(path)?;
        let mut raw = vec![0u8; (size - stamp.size) as usize];
        read_exact_at(&file, &mut raw, stamp.size)?;
        let modified = file.metadata()?.modified()?;
        let (bytes, consumed) = if self.binary || self.encoding.is_utf8() {
            let n = raw.len();
            (raw, n)
        } else {
            // Leave a dangling half code unit of UTF-16 for the next append.
            let n = if self.encoding.is_utf16() { raw.len() & !1 } else { raw.len() };
            let mut out = Vec::new();
            decode_chunk(&mut self.encoding.encoding.new_decoder_without_bom_handling(), &raw[..n], true, &mut out);
            (out, n)
        };
        if !bytes.is_empty() {
            let end = self.len();
            let piece = self.text.push_add(&bytes);
            let list = Arc::make_mut(&mut self.text.list);
            let n = list.pieces().len();
            list.splice(n, 0, &[piece]);
            self.history.reset();
            self.history.bump(end);
        }
        self.stamp = Some(Stamp { size: stamp.size + consumed as u64, modified });
        Ok(bytes.len() as u64)
    }
}

#[cfg(test)]
impl Document {
    pub(crate) fn memory_limit_for_tests(&mut self, limit: u64) {
        self.memory_limit = limit;
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        if let Some(job) = &self.index_job {
            job.progress.cancel();
        }
    }
}

fn typed_kind(bytes: &[u8]) -> Option<Typed> {
    let single = std::str::from_utf8(bytes).ok().filter(|s| s.chars().count() == 1);
    match single {
        Some(s) if s.chars().all(char::is_whitespace) => Some(Typed::Space),
        Some(_) => Some(Typed::Word),
        None if bytes == b"\r\n" => Some(Typed::Space),
        None => None,
    }
}

fn spawn_index(source: Arc<Source>, base: u64) -> Result<IndexJob, CoreError> {
    let progress = Progress::new();
    let worker = Arc::clone(&progress);
    let handle =
        thread::Builder::new().name("kladde-line-index".into()).spawn(move || index::build(&source, base, &worker))?;
    Ok(IndexJob { progress, handle })
}

fn stamp_of(meta: &fs::Metadata) -> io::Result<Stamp> {
    Ok(Stamp { size: meta.len(), modified: meta.modified()? })
}

/// Maps or pages `path` again after a failed rename; the file must be unchanged.
fn reattach(path: &Path, kind: SourceKind, len: u64, memory_limit: u64) -> io::Result<Source> {
    let file = source::open_file(path)?;
    let now = file.metadata()?.len();
    if now != len {
        Err(io::Error::other("file changed on disk"))
    } else if kind == SourceKind::Paged {
        Ok(Source::paged(file, len, path.to_path_buf()))
    } else if len <= memory_limit {
        let mut file = file;
        Source::read_file(&mut file, len)
    } else {
        Source::map(file, Some(path.to_path_buf()))
    }
}

fn load(
    path: &Path,
    mode: &OpenMode,
    no_lock: bool,
    progress: &Progress,
    memory_limit: u64,
) -> Result<Loaded, CoreError> {
    let mut file = source::open_file(path)?;
    let meta = file.metadata()?;
    let len = meta.len();
    let stamp = stamp_of(&meta)?;
    let mut sample = vec![0u8; SAMPLE.min(len) as usize];
    read_exact_at(&file, &mut sample, 0)?;
    let (binary, encoding) = match mode {
        OpenMode::Binary => (true, TextEncoding::UTF8),
        OpenMode::Text { encoding: Some(e) } => (false, *e),
        OpenMode::Text { encoding: None } if looks_binary(&sample) => (true, TextEncoding::UTF8),
        OpenMode::Text { encoding: None } => (false, detect_encoding(&sample)),
    };
    let (source, base) = if binary || encoding.is_utf8() {
        let base = if !binary && encoding.bom && sample.starts_with(encoding.bom_bytes()) { 3 } else { 0 };
        let source = if len <= memory_limit {
            Source::read_file(&mut file, len)?
        } else if no_lock {
            Source::paged(file, len, path.to_path_buf())
        } else {
            Source::map(file, Some(path.to_path_buf()))?
        };
        (source, base)
    } else {
        (transcode(file, len, encoding, memory_limit, progress)?, 0)
    };
    Ok(Loaded { source, base, binary, encoding, stamp })
}

/// Decodes a non-UTF-8 file into UTF-8: in RAM when small, else into a scratch file that is then mapped.
fn transcode(
    mut file: File,
    len: u64,
    encoding: TextEncoding,
    memory_limit: u64,
    progress: &Progress,
) -> Result<Source, CoreError> {
    progress.set_total(len);
    let mut decoder = encoding.encoding.new_decoder_with_bom_removal();
    io::Seek::seek(&mut file, io::SeekFrom::Start(0))?;
    let mut sink = if len <= memory_limit { None } else { Some(source::temp_file()?) };
    let mut memory = Vec::new();
    let mut input = vec![0u8; IO_CHUNK];
    let mut out = Vec::new();
    let mut written = 0u64;
    let mut done = false;
    while !done {
        if progress.is_cancelled() {
            Err(CoreError::Cancelled)?;
        }
        let n = read_full(&mut file, &mut input)?;
        done = n < input.len();
        match &mut sink {
            Some(temp) => {
                out.clear();
                decode_chunk(&mut decoder, &input[..n], done, &mut out);
                io::Write::write_all(temp, &out)?;
                written += out.len() as u64;
            }
            None => decode_chunk(&mut decoder, &input[..n], done, &mut memory),
        }
        progress.add(n as u64);
    }
    match sink {
        Some(temp) if written > 0 => Ok(Source::map(temp, None)?),
        _ => Ok(Source::memory(memory)),
    }
}
