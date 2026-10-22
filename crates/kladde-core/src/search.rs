use std::ops::Range;

use memchr::memmem::{Finder, FinderRev};
use regex::bytes::{Regex, RegexBuilder};

use crate::{CoreError, Progress, Snapshot};

/// Window size; huge documents are searched window by window.
const CHUNK: u64 = 4 << 20;
/// Windows extend this far into the next one so matches crossing a boundary are found whole.
const OVERLAP: u64 = 64 << 10;
/// Bytes before a window start that give `\b` and `^` their real context.
const CONTEXT: u64 = 16;

#[derive(Clone, Debug)]
pub enum SearchPattern {
    Text { pattern: String, regex: bool, case_sensitive: bool, whole_word: bool },
    Bytes(Vec<u8>),
}

enum Matcher {
    Literal { forward: Box<Finder<'static>>, backward: Box<FinderRev<'static>>, len: usize },
    Regex(Regex),
}

impl Matcher {
    /// First match starting at or after `at`.
    fn find_at(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        match self {
            Matcher::Literal { forward, len, .. } => forward.find(&hay[at..]).map(|p| (at + p, at + p + len)),
            Matcher::Regex(re) => re.find_at(hay, at).map(|m| (m.start(), m.end())),
        }
    }

    /// Last match starting in `from..limit`.
    fn find_last(&self, hay: &[u8], from: usize, limit: usize) -> Option<(usize, usize)> {
        match self {
            Matcher::Literal { backward, len, .. } => {
                let end = hay.len().min(limit + len - 1);
                backward.rfind(&hay[from..end]).map(|p| (from + p, from + p + len))
            }
            Matcher::Regex(_) => {
                let mut last = None;
                let mut next = self.find_at(hay, from);
                while let Some((s, e)) = next.filter(|&(s, _)| s < limit) {
                    last = Some((s, e));
                    let at = if e > s { e } else { e + 1 };
                    next = if at <= hay.len() { self.find_at(hay, at) } else { None };
                }
                last
            }
        }
    }
}

/// Compiled search; matches longer than 64 KiB may be missed where windows meet.
pub struct Searcher {
    matcher: Matcher,
}

impl Searcher {
    pub fn new(p: &SearchPattern) -> Result<Searcher, CoreError> {
        let matcher = match p {
            SearchPattern::Bytes(bytes) => literal(bytes)?,
            SearchPattern::Text { pattern, regex: false, case_sensitive: true, whole_word: false } => {
                literal(pattern.as_bytes())?
            }
            SearchPattern::Text { pattern, .. } if pattern.is_empty() => {
                Err(CoreError::InvalidPattern("empty pattern".into()))?
            }
            SearchPattern::Text { pattern, regex, case_sensitive, whole_word } => {
                let source = if *regex { pattern.clone() } else { regex::escape(pattern) };
                let source = if *whole_word { format!(r"\b(?:{source})\b") } else { source };
                let re = RegexBuilder::new(&source)
                    .case_insensitive(!case_sensitive)
                    .multi_line(true)
                    .crlf(true)
                    .build()
                    .map_err(|e| CoreError::InvalidPattern(e.to_string()))?;
                Matcher::Regex(re)
            }
        };
        Ok(Searcher { matcher })
    }

    /// First match starting at or after `from`.
    pub fn find_next(&self, s: &Snapshot, from: u64, p: &Progress) -> Result<Option<Range<u64>>, CoreError> {
        let mut found = None;
        self.scan(s, from..s.len(), p, &mut |m| {
            found = Some(m);
            false
        })?;
        p.finish();
        Ok(found)
    }

    /// Last match ending at or before `before`.
    pub fn find_prev(&self, s: &Snapshot, before: u64, p: &Progress) -> Result<Option<Range<u64>>, CoreError> {
        let before = before.min(s.len());
        p.set_total(before);
        let mut hi = before;
        let mut found = None;
        let mut buf = Vec::new();
        while found.is_none() && hi > 0 {
            if p.is_cancelled() {
                Err(CoreError::Cancelled)?;
            }
            let lo = hi.saturating_sub(CHUNK);
            let base = lo - CONTEXT.min(lo);
            buf.clear();
            s.text.read_checked(base..before.min(hi + OVERLAP), &mut buf)?;
            found = self
                .matcher
                .find_last(&buf, (lo - base) as usize, (hi - base) as usize)
                .map(|(a, b)| base + a as u64..base + b as u64);
            hi = lo;
            p.set_done(before - hi);
        }
        p.finish();
        Ok(found)
    }

    /// Reports non-overlapping matches inside `range` in order; stops after `max`. Returns the count.
    pub fn find_all(
        &self,
        s: &Snapshot,
        range: Range<u64>,
        p: &Progress,
        max: usize,
        on_match: &mut dyn FnMut(Range<u64>),
    ) -> Result<usize, CoreError> {
        let mut count = 0;
        if max > 0 {
            self.scan(s, range, p, &mut |m| {
                on_match(m);
                count += 1;
                count < max
            })?;
        }
        p.finish();
        Ok(count)
    }

    /// Replacement bytes for match `m`; regex patterns expand `$1` and `${name}`.
    pub fn expand_replacement(&self, s: &Snapshot, m: Range<u64>, replacement: &str) -> Vec<u8> {
        match &self.matcher {
            Matcher::Regex(re) => {
                let base = m.start - CONTEXT.min(m.start);
                let mut hay = Vec::new();
                s.read(base..m.end, &mut hay);
                let at = (m.start - base) as usize;
                let mut out = Vec::new();
                match re.captures_at(&hay, at).filter(|c| c.get(0).is_some_and(|g| g.start() == at)) {
                    Some(caps) => caps.expand(replacement.as_bytes(), &mut out),
                    None => out.extend_from_slice(replacement.as_bytes()),
                }
                out
            }
            Matcher::Literal { .. } => replacement.as_bytes().to_vec(),
        }
    }

    /// Calls `f` for each match in `range` until it returns false.
    fn scan(
        &self,
        s: &Snapshot,
        range: Range<u64>,
        p: &Progress,
        f: &mut dyn FnMut(Range<u64>) -> bool,
    ) -> Result<(), CoreError> {
        let end = range.end.min(s.len());
        let start = range.start.min(end);
        p.set_total(end - start);
        let mut pos = start;
        let mut from = start;
        let mut more = true;
        let mut buf = Vec::new();
        // One window even for empty ranges, so `^` can match.
        let mut first = true;
        while more && (pos < end || first) {
            if p.is_cancelled() {
                Err(CoreError::Cancelled)?;
            }
            first = false;
            let window_end = end.min(pos + CHUNK + OVERLAP);
            let last_window = window_end == end;
            let accept = if last_window { end + 1 } else { pos + CHUNK };
            let base = pos - CONTEXT.min(pos);
            buf.clear();
            s.text.read_checked(base..window_end, &mut buf)?;
            let mut at = (from.max(pos) - base) as usize;
            let mut next = if at <= buf.len() { self.matcher.find_at(&buf, at) } else { None };
            while let Some((ms, me)) = next.filter(|&(ms, _)| more && base + (ms as u64) < accept) {
                more = f(base + ms as u64..base + me as u64);
                at = if me > ms { me } else { me + 1 };
                from = base + at as u64;
                next = if at <= buf.len() { self.matcher.find_at(&buf, at) } else { None };
            }
            pos = if last_window { end } else { pos + CHUNK };
            p.set_done(pos - start);
        }
        Ok(())
    }
}

fn literal(needle: &[u8]) -> Result<Matcher, CoreError> {
    if needle.is_empty() {
        Err(CoreError::InvalidPattern("empty pattern".into()))
    } else {
        Ok(Matcher::Literal {
            forward: Box::new(Finder::new(needle).into_owned()),
            backward: Box::new(FinderRev::new(needle).into_owned()),
            len: needle.len(),
        })
    }
}
