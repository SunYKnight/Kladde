//! Kladde text engine: file sources, piece table, undo, encodings, line index, saving and search.

mod document;
mod encoding;
mod history;
mod index;
mod save;
mod search;
mod source;
#[cfg(test)]
mod tests;
mod text;

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub use document::{DiskState, Document, OpenMode, OpenOptions, SourceKind};
pub use encoding::{detect_encoding, detect_line_ending, looks_binary, LineEnding, TextEncoding};
pub use save::{PreparedSave, SaveJob, Snapshot};
pub use search::{SearchPattern, Searcher};

/// Files up to this size are read fully into RAM (no file lock).
pub const MEMORY_LIMIT: u64 = 64 << 20;

#[derive(Debug)]
pub enum CoreError {
    Io(io::Error),
    Cancelled,
    InvalidPattern(String),
    TooLarge(String),
    Busy(String),
    ReadOnly,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Io(e) => write!(f, "I/O error: {e}"),
            CoreError::Cancelled => write!(f, "cancelled"),
            CoreError::InvalidPattern(m) => write!(f, "invalid search pattern: {m}"),
            CoreError::TooLarge(m) => write!(f, "too large: {m}"),
            CoreError::Busy(m) => write!(f, "busy: {m}"),
            CoreError::ReadOnly => write!(f, "document is read-only"),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CoreError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for CoreError {
    fn from(e: io::Error) -> Self {
        CoreError::Io(e)
    }
}

/// Shared progress and cancellation state of a long-running operation.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
    finished: AtomicBool,
}

impl Progress {
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress::default())
    }

    pub fn set_total(&self, v: u64) {
        self.total.store(v, Ordering::Relaxed);
    }

    pub fn set_done(&self, v: u64) {
        self.done.store(v, Ordering::Relaxed);
    }

    pub fn add(&self, v: u64) {
        self.done.fetch_add(v, Ordering::Relaxed);
    }

    /// Fraction done in `0.0..=1.0`; 0.0 while the total is unknown.
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}
