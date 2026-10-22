use std::io;

use memchr::memchr_iter;

use crate::source::Source;
use crate::{CoreError, Progress};

const LINE_STRIDE: u64 = 1024;
/// Extra checkpoints inside very long lines keep every lookup scan short.
const BYTE_STRIDE: u64 = 1 << 20;

/// Sparse newline index of a source: `(offset, newlines before offset)` checkpoints every 1024 lines and at
/// least every MiB, so memory stays bounded for 10 GB files and each lookup scans at most one stride.
pub(crate) struct OrigIndex {
    checkpoints: Vec<(u64, u64)>,
}

pub(crate) fn build(source: &Source, base: u64, progress: &Progress) -> Result<OrigIndex, CoreError> {
    let len = source.len();
    progress.set_total(len - base);
    let mut checkpoints = vec![(base, 0u64)];
    let mut lines = 0u64;
    let mut cancelled = false;
    source.scan(base..len, &mut |off, chunk| {
        if checkpoints.last().is_some_and(|c| c.0 + BYTE_STRIDE <= off) {
            checkpoints.push((off, lines));
        }
        for p in memchr_iter(b'\n', chunk) {
            lines += 1;
            if lines % LINE_STRIDE == 0 {
                checkpoints.push((off + p as u64 + 1, lines));
            }
        }
        progress.add(chunk.len() as u64);
        cancelled = progress.is_cancelled();
        cancelled.then_some(0)
    })?;
    if cancelled {
        Err(CoreError::Cancelled)
    } else {
        progress.finish();
        Ok(OrigIndex { checkpoints })
    }
}

impl OrigIndex {
    /// Number of '\n' in `base..x`.
    pub(crate) fn newlines_before(&self, source: &Source, x: u64) -> io::Result<u64> {
        let k = self.checkpoints.partition_point(|c| c.0 <= x).saturating_sub(1);
        let (off, lines) = self.checkpoints[k];
        let mut count = 0;
        source.scan(off..x.max(off), &mut |_, chunk| {
            count += memchr_iter(b'\n', chunk).count() as u64;
            None
        })?;
        Ok(lines + count)
    }

    /// Number of '\n' in `a..b`.
    pub(crate) fn count(&self, source: &Source, a: u64, b: u64) -> io::Result<u64> {
        Ok(self.newlines_before(source, b)? - self.newlines_before(source, a)?)
    }

    /// Offset just after the `n`-th (1-based) '\n'; None if the source has fewer.
    pub(crate) fn after_newline(&self, source: &Source, n: u64) -> io::Result<Option<u64>> {
        let k = self.checkpoints.partition_point(|c| c.1 < n).saturating_sub(1);
        let (off, lines) = self.checkpoints[k];
        let need = n - lines;
        let mut seen = 0;
        source.scan(off..source.len(), &mut |at, chunk| {
            memchr_iter(b'\n', chunk)
                .find(|_| {
                    seen += 1;
                    seen == need
                })
                .map(|p| at + p as u64 + 1)
        })
    }
}
