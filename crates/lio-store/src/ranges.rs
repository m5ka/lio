//! Set of downloaded byte ranges for a partial file, persisted in a `<file>.ranges` sidecar.

use std::path::{Path, PathBuf};

/// Sorted, non-overlapping, non-adjacent half-open ranges `[start, end)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ranges {
    ranges: Vec<(u64, u64)>,
}

impl Ranges {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_slice(&self) -> &[(u64, u64)] {
        &self.ranges
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Adds `[start, end)`, merging with overlapping or adjacent ranges.
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let (mut start, mut end) = (start, end);
        // First range whose end reaches `start` (adjacent counts).
        let first = self.ranges.partition_point(|&(_, e)| e < start);
        let mut last = first;
        while last < self.ranges.len() && self.ranges[last].0 <= end {
            start = start.min(self.ranges[last].0);
            end = end.max(self.ranges[last].1);
            last += 1;
        }
        self.ranges.splice(first..last, [(start, end)]);
    }

    /// Whether every byte of `[start, end)` is present.
    pub fn contains(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return true;
        }
        let i = self.ranges.partition_point(|&(_, e)| e <= start);
        self.ranges
            .get(i)
            .is_some_and(|&(s, e)| s <= start && end <= e)
    }

    /// Number of contiguous bytes available starting at `offset`.
    pub fn available_from(&self, offset: u64) -> u64 {
        let i = self.ranges.partition_point(|&(_, e)| e <= offset);
        match self.ranges.get(i) {
            Some(&(s, e)) if s <= offset => e - offset,
            _ => 0,
        }
    }

    /// The first gap at or after `from` within `[0, total)`, as `[start, end)`.
    pub fn first_missing(&self, from: u64, total: u64) -> Option<(u64, u64)> {
        let mut cursor = from;
        for &(s, e) in &self.ranges {
            if e <= cursor {
                continue;
            }
            if s > cursor {
                return Some((cursor, s.min(total))).filter(|(a, b)| a < b);
            }
            cursor = e;
        }
        (cursor < total).then_some((cursor, total))
    }

    /// The first gap anywhere in `[0, total)`, preferring gaps at or after `from`.
    pub fn next_missing(&self, from: u64, total: u64) -> Option<(u64, u64)> {
        self.first_missing(from, total)
            .or_else(|| self.first_missing(0, total))
    }

    pub fn covers(&self, total: u64) -> bool {
        self.contains(0, total)
    }

    pub fn total_bytes(&self) -> u64 {
        self.ranges.iter().map(|(s, e)| e - s).sum()
    }

    /// Sidecar path for a partial file: `<file>.ranges`.
    pub fn sidecar_path(file: &Path) -> PathBuf {
        let mut name = file.as_os_str().to_owned();
        name.push(".ranges");
        PathBuf::from(name)
    }

    /// Writes `start end` pairs, one per line, atomically.
    pub fn save(&self, sidecar: &Path) -> std::io::Result<()> {
        let text: String = self
            .ranges
            .iter()
            .map(|(s, e)| format!("{s} {e}\n"))
            .collect();
        let tmp = sidecar.with_extension("ranges.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, sidecar)
    }

    /// Loads a sidecar; a missing or corrupt file yields an empty set.
    pub fn load(sidecar: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(sidecar) else {
            return Self::new();
        };
        let mut ranges = Self::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace().map(str::parse::<u64>);
            match (parts.next(), parts.next()) {
                (Some(Ok(s)), Some(Ok(e))) => ranges.insert(s, e),
                _ => return Self::new(),
            }
        }
        ranges
    }
}
