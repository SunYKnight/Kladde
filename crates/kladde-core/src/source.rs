use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use crate::SourceKind;

/// Scan granularity: bounds the copy buffer for paged sources.
pub(crate) const SCAN: u64 = 1 << 20;
const PAGE: u64 = 64 << 10;
const CACHE_PAGES: usize = 64;

enum Store {
    Memory(Vec<u8>),
    Mapped { map: memmap2::Mmap, _file: File },
    Paged(Paged),
}

/// Immutable original bytes of a document.
pub(crate) struct Source {
    store: Store,
    len: u64,
    /// User file kept open (mapping or handle); it must be released before that file is replaced.
    pinned: Option<PathBuf>,
    /// First read error hit by an API that cannot return it.
    error: Mutex<Option<io::Error>>,
}

impl Source {
    pub(crate) fn memory(bytes: Vec<u8>) -> Source {
        Source::with_store(Store::Memory(bytes), None)
    }

    pub(crate) fn read_file(file: &mut File, len: u64) -> io::Result<Source> {
        let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        // Positioned reads move the cursor on Windows.
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut bytes)?;
        Ok(Source::memory(bytes))
    }

    /// `pinned` is the user file behind `file`; None for private temp files.
    pub(crate) fn map(file: File, pinned: Option<PathBuf>) -> io::Result<Source> {
        // SAFETY: read-only map; external truncation is a documented limit.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Source::with_store(Store::Mapped { map, _file: file }, pinned))
    }

    pub(crate) fn paged(file: File, len: u64, path: PathBuf) -> Source {
        let paged = Paged { file, len, cache: Mutex::new(PageCache::default()) };
        Source::with_store(Store::Paged(paged), Some(path))
    }

    fn with_store(store: Store, pinned: Option<PathBuf>) -> Source {
        let len = match &store {
            Store::Memory(v) => v.len() as u64,
            Store::Mapped { map, .. } => map.len() as u64,
            Store::Paged(p) => p.len,
        };
        Source { store, len, pinned, error: Mutex::new(None) }
    }

    pub(crate) fn kind(&self) -> SourceKind {
        match self.store {
            Store::Memory(_) => SourceKind::Memory,
            Store::Mapped { .. } => SourceKind::Mapped,
            Store::Paged(_) => SourceKind::Paged,
        }
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn pinned(&self) -> Option<&Path> {
        self.pinned.as_deref()
    }

    /// Bytes `off..off + len`, borrowed when resident, else read into `buf`.
    pub(crate) fn slice<'a>(&'a self, off: u64, len: u64, buf: &'a mut Vec<u8>) -> io::Result<&'a [u8]> {
        let range = off as usize..(off + len) as usize;
        match &self.store {
            Store::Memory(v) => Ok(&v[range]),
            Store::Mapped { map, .. } => Ok(&map[range]),
            Store::Paged(p) => {
                buf.clear();
                buf.resize(range.len(), 0);
                p.read(off, buf)?;
                Ok(&buf[..])
            }
        }
    }

    /// Calls `f(offset, chunk)` over `range` in order until it returns Some.
    pub(crate) fn scan(
        &self,
        range: Range<u64>,
        f: &mut dyn FnMut(u64, &[u8]) -> Option<u64>,
    ) -> io::Result<Option<u64>> {
        let mut pos = range.start;
        let mut found = None;
        let mut buf = Vec::new();
        while found.is_none() && pos < range.end {
            let n = (range.end - pos).min(SCAN);
            found = f(pos, self.slice(pos, n, &mut buf)?);
            pos += n;
        }
        Ok(found)
    }

    pub(crate) fn record_error(&self, e: io::Error) {
        let mut slot = self.error.lock().unwrap_or_else(PoisonError::into_inner);
        slot.get_or_insert(e);
    }

    pub(crate) fn take_error(&self) -> Option<io::Error> {
        self.error.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

struct Paged {
    file: File,
    len: u64,
    cache: Mutex<PageCache>,
}

impl Paged {
    fn read(&self, off: u64, dst: &mut [u8]) -> io::Result<()> {
        if dst.len() as u64 >= SCAN {
            // Bulk scans bypass the cache to keep on-screen pages.
            read_exact_at(&self.file, dst, off)
        } else {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            let mut done = 0usize;
            while done < dst.len() {
                let at = off + done as u64;
                let no = at / PAGE;
                let page = cache.page(&self.file, no, self.len)?;
                let inside = (at - no * PAGE) as usize;
                let n = (page.len() - inside).min(dst.len() - done);
                dst[done..done + n].copy_from_slice(&page[inside..inside + n]);
                done += n;
            }
            Ok(())
        }
    }
}

#[derive(Default)]
struct PageCache {
    pages: Vec<CachedPage>,
    tick: u64,
}

struct CachedPage {
    no: u64,
    bytes: Vec<u8>,
    used: u64,
}

impl PageCache {
    fn page(&mut self, file: &File, no: u64, file_len: u64) -> io::Result<&[u8]> {
        self.tick += 1;
        let idx = match self.pages.iter().position(|p| p.no == no) {
            Some(i) => i,
            None => {
                let start = no * PAGE;
                let mut bytes = vec![0; PAGE.min(file_len - start) as usize];
                read_exact_at(file, &mut bytes, start)?;
                let entry = CachedPage { no, bytes, used: 0 };
                if self.pages.len() < CACHE_PAGES {
                    self.pages.push(entry);
                    self.pages.len() - 1
                } else {
                    let lru = (0..self.pages.len()).min_by_key(|&i| self.pages[i].used).expect("cache is full");
                    self.pages[lru] = entry;
                    lru
                }
            }
        };
        self.pages[idx].used = self.tick;
        Ok(&self.pages[idx].bytes)
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, off)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, off)
}

/// Positioned read of exactly `buf.len()` bytes; a shrunken file is an error.
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    let mut filled = 0;
    let mut eof = false;
    while !eof && filled < buf.len() {
        match read_at(file, &mut buf[filled..], off + filled as u64) {
            Ok(0) => eof = true,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => Err(e)?,
        }
    }
    if filled < buf.len() {
        Err(io::Error::new(io::ErrorKind::UnexpectedEof, "file shrank while open"))
    } else {
        Ok(())
    }
}

/// Reads until `buf` is full or EOF; returns the byte count.
pub(crate) fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    let mut eof = false;
    while !eof && filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => eof = true,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => Err(e)?,
        }
    }
    Ok(filled)
}

/// std opens with FILE_SHARE_READ | WRITE | DELETE on Windows, so a plain handle never locks the file.
pub(crate) fn open_file(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// Process-unique file name component.
pub(crate) fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{}-{}-{}", std::process::id(), nanos, COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Anonymous scratch file that disappears when its last handle closes.
pub(crate) fn temp_file() -> io::Result<File> {
    let path = std::env::temp_dir().join(format!("kladde-{}.tmp", unique_suffix()));
    let file = open_temp(&path)?;
    #[cfg(unix)]
    fs::remove_file(&path)?;
    Ok(file)
}

#[cfg(windows)]
fn open_temp(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    fs::OpenOptions::new().read(true).write(true).create_new(true).custom_flags(FILE_FLAG_DELETE_ON_CLOSE).open(path)
}

#[cfg(unix)]
fn open_temp(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new().read(true).write(true).create_new(true).open(path)
}
