use std::io;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use memchr::memchr_iter;

use crate::source::{Source, SCAN};

/// Add-buffer chunks stay small enough that a shared chunk never forces a big copy.
const ADD_CHUNK: usize = 1 << 20;
const ADD_CHUNK_MIN: usize = 4 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Buf {
    Orig,
    Add(u32),
}

/// A run of bytes from the original source or one add-buffer chunk.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Piece {
    pub(crate) buf: Buf,
    pub(crate) start: u64,
    pub(crate) len: u64,
    /// Newlines inside the piece; None for original pieces until the line index exists.
    pub(crate) nl: Option<u64>,
}

impl Piece {
    pub(crate) fn followed_by(&self, next: &Piece) -> bool {
        self.buf == next.buf && self.start + self.len == next.start
    }
}

#[derive(Clone, Default)]
pub(crate) struct PieceList {
    pieces: Vec<Piece>,
    len: u64,
    starts: OnceLock<Vec<u64>>,
    lines: OnceLock<Option<Vec<u64>>>,
}

impl PieceList {
    fn new(pieces: Vec<Piece>) -> PieceList {
        let len = pieces.iter().map(|p| p.len).sum();
        PieceList { pieces, len, ..PieceList::default() }
    }

    pub(crate) fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    fn starts(&self) -> &[u64] {
        self.starts.get_or_init(|| {
            let mut acc = 0;
            std::iter::once(0)
                .chain(self.pieces.iter().map(|p| {
                    acc += p.len;
                    acc
                }))
                .collect()
        })
    }

    pub(crate) fn piece_start(&self, i: usize) -> u64 {
        self.starts()[i]
    }

    /// Index and start offset of the piece containing `off`; `(n, len)` at or past the end.
    pub(crate) fn locate(&self, off: u64) -> (usize, u64) {
        let n = self.pieces.len();
        if off >= self.len {
            (n, self.len)
        } else {
            let starts = self.starts();
            let i = starts[..n].partition_point(|&s| s <= off) - 1;
            (i, starts[i])
        }
    }

    /// Newlines before each piece (n + 1 entries); None while any count is unknown.
    pub(crate) fn line_prefix(&self) -> Option<&[u64]> {
        self.lines
            .get_or_init(|| {
                self.pieces.iter().try_fold(vec![0], |mut acc, p| {
                    let next = acc[acc.len() - 1] + p.nl?;
                    acc.push(next);
                    Some(acc)
                })
            })
            .as_deref()
    }

    pub(crate) fn splice(&mut self, index: usize, remove: usize, insert: &[Piece]) {
        let removed: u64 = self.pieces[index..index + remove].iter().map(|p| p.len).sum();
        let added: u64 = insert.iter().map(|p| p.len).sum();
        self.pieces.splice(index..index + remove, insert.iter().copied());
        self.len = self.len + added - removed;
        self.invalidate();
    }

    pub(crate) fn set_counts(&mut self, counts: &[Option<u64>]) {
        self.pieces.iter_mut().zip(counts).for_each(|(p, c)| p.nl = *c);
        self.invalidate();
    }

    fn invalidate(&mut self) {
        self.starts = OnceLock::new();
        self.lines = OnceLock::new();
    }
}

/// Document content: original source + append-only add chunks + piece list, all cheaply shareable.
#[derive(Clone)]
pub(crate) struct Text {
    pub(crate) source: Arc<Source>,
    pub(crate) list: Arc<PieceList>,
    add: Arc<Vec<Arc<Vec<u8>>>>,
}

impl Text {
    pub(crate) fn new(source: Arc<Source>, base: u64) -> Text {
        let len = source.len() - base;
        let pieces = if len > 0 { vec![Piece { buf: Buf::Orig, start: base, len, nl: None }] } else { Vec::new() };
        Text { source, list: Arc::new(PieceList::new(pieces)), add: Arc::new(Vec::new()) }
    }

    pub(crate) fn len(&self) -> u64 {
        self.list.len()
    }

    /// Bytes of an add piece.
    pub(crate) fn add_bytes(&self, piece: &Piece, local: Range<u64>) -> &[u8] {
        match piece.buf {
            Buf::Add(c) => {
                &self.add[c as usize][(piece.start + local.start) as usize..(piece.start + local.end) as usize]
            }
            Buf::Orig => unreachable!("add_bytes is only called for add pieces"),
        }
    }

    /// Bytes `local` within `piece`, borrowed when resident, else read into `buf`.
    pub(crate) fn piece_slice<'a>(
        &'a self,
        piece: &Piece,
        local: Range<u64>,
        buf: &'a mut Vec<u8>,
    ) -> io::Result<&'a [u8]> {
        match piece.buf {
            Buf::Add(_) => Ok(self.add_bytes(piece, local)),
            Buf::Orig => self.source.slice(piece.start + local.start, local.end - local.start, buf),
        }
    }

    /// Calls `f(offset, chunk)` over `range` front to back until it returns Some.
    pub(crate) fn visit(
        &self,
        range: Range<u64>,
        f: &mut dyn FnMut(u64, &[u8]) -> Option<u64>,
    ) -> io::Result<Option<u64>> {
        let end = range.end.min(self.len());
        let mut pos = range.start.min(end);
        let mut found = None;
        let mut buf = Vec::new();
        while found.is_none() && pos < end {
            let (i, start) = self.list.locate(pos);
            let piece = self.list.pieces()[i];
            let seg_end = end.min(start + piece.len).min(pos + SCAN);
            found = f(pos, self.piece_slice(&piece, pos - start..seg_end - start, &mut buf)?);
            pos = seg_end;
        }
        Ok(found)
    }

    /// Calls `f(offset, chunk)` over `range` back to front until it returns Some.
    pub(crate) fn visit_rev(
        &self,
        range: Range<u64>,
        f: &mut dyn FnMut(u64, &[u8]) -> Option<u64>,
    ) -> io::Result<Option<u64>> {
        let mut end = range.end.min(self.len());
        let start = range.start.min(end);
        let mut found = None;
        let mut buf = Vec::new();
        while found.is_none() && end > start {
            let (i, piece_start) = self.list.locate(end - 1);
            let piece = self.list.pieces()[i];
            let seg_start = start.max(piece_start).max(end.saturating_sub(SCAN));
            found = f(seg_start, self.piece_slice(&piece, seg_start - piece_start..end - piece_start, &mut buf)?);
            end = seg_start;
        }
        Ok(found)
    }

    /// Appends the clamped `range` to `out`.
    pub(crate) fn read_checked(&self, range: Range<u64>, out: &mut Vec<u8>) -> io::Result<()> {
        self.visit(range, &mut |_, chunk| {
            out.extend_from_slice(chunk);
            None
        })?;
        Ok(())
    }

    /// Like `read_checked`, but zero-fills on I/O errors and records them on the source.
    pub(crate) fn read(&self, range: Range<u64>, out: &mut Vec<u8>) {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        let before = out.len();
        if let Err(e) = self.read_checked(start..end, out) {
            out.resize(before + (end - start) as usize, 0);
            self.source.record_error(e);
        }
    }

    pub(crate) fn byte_at(&self, off: u64) -> Option<u8> {
        let mut b = Vec::with_capacity(1);
        self.read(off..off.saturating_add(1), &mut b);
        b.first().copied()
    }

    /// Appends `bytes` to the add buffer and returns a piece covering them.
    pub(crate) fn push_add(&mut self, bytes: &[u8]) -> Piece {
        let chunks = Arc::make_mut(&mut self.add);
        let fits = chunks.last_mut().and_then(Arc::get_mut).is_some_and(|c| c.len() + bytes.len() <= ADD_CHUNK);
        if !fits {
            chunks.push(Arc::new(Vec::with_capacity(bytes.len().max(ADD_CHUNK_MIN))));
        }
        let idx = chunks.len() - 1;
        let chunk = Arc::get_mut(&mut chunks[idx]).expect("chunk is unshared: checked or just created");
        let start = chunk.len() as u64;
        chunk.extend_from_slice(bytes);
        let nl = memchr_iter(b'\n', bytes).count() as u64;
        Piece { buf: Buf::Add(idx as u32), start, len: bytes.len() as u64, nl: Some(nl) }
    }
}
