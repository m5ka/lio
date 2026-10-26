//! Play queue.
//!
//! `entries` keep their original order; `order` is the play order (identity unless
//! shuffled) and `cursor` points into `order`. Shuffling builds a permutation with the
//! current entry first; unshuffling restores the original order.

use rusqlite::{Connection, params};

use crate::error::Result;
use crate::model::TrackRow;
use crate::queries;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "all" => Self::All,
            "one" => Self::One,
            _ => Self::Off,
        }
    }

    /// off → all → one → off.
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// Where a queue entry came from (album, playlist, …).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Origin {
    pub kind: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueEntry {
    pub track: TrackRow,
    pub origin: Origin,
}

impl QueueEntry {
    pub fn new(track: TrackRow, origin: Origin) -> Self {
        Self { track, origin }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Queue {
    entries: Vec<QueueEntry>,
    order: Vec<usize>,
    cursor: Option<usize>,
    shuffle: bool,
    repeat: Repeat,
    /// Playback position of the current entry, for restore.
    pub position_ms: u64,
}

/// Random source; injectable for deterministic tests.
pub type Rng<'a> = &'a mut dyn FnMut(usize) -> usize;

fn default_rng(n: usize) -> usize {
    rand::random_range(0..n)
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Position of the current entry in play order.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Entries in play order.
    pub fn items(&self) -> Vec<&QueueEntry> {
        self.order.iter().map(|&i| &self.entries[i]).collect()
    }

    pub fn get(&self, pos: usize) -> Option<&QueueEntry> {
        self.order.get(pos).map(|&i| &self.entries[i])
    }

    pub fn current(&self) -> Option<&QueueEntry> {
        self.cursor.and_then(|c| self.get(c))
    }

    /// Replaces the queue; `start` is the first entry to play.
    pub fn set(&mut self, entries: Vec<QueueEntry>, start: usize) {
        self.set_with(entries, start, &mut default_rng);
    }

    pub fn set_with(&mut self, entries: Vec<QueueEntry>, start: usize, rng: Rng<'_>) {
        let start = start.min(entries.len().saturating_sub(1));
        self.entries = entries;
        self.order = (0..self.entries.len()).collect();
        self.cursor = (!self.entries.is_empty()).then_some(start);
        self.position_ms = 0;
        if self.shuffle {
            self.reshuffle(rng);
        }
    }

    /// "Play next": inserts after the current entry.
    pub fn play_next(&mut self, entries: Vec<QueueEntry>) {
        let at = self.cursor.map_or(0, |c| c + 1);
        self.insert(at, entries);
    }

    /// "Add to queue": appends.
    pub fn enqueue(&mut self, entries: Vec<QueueEntry>) {
        let at = self.order.len();
        self.insert(at, entries);
    }

    fn insert(&mut self, at: usize, entries: Vec<QueueEntry>) {
        if entries.is_empty() {
            return;
        }
        let n = entries.len();
        let at = at.min(self.order.len());
        if self.shuffle {
            let first = self.entries.len();
            self.entries.extend(entries);
            self.order.splice(at..at, first..first + n);
        } else {
            // Unshuffled: play order equals entry order.
            self.entries.splice(at..at, entries);
            self.order = (0..self.entries.len()).collect();
        }
        match self.cursor {
            Some(c) if c >= at => self.cursor = Some(c + n),
            None => self.cursor = Some(at),
            _ => {}
        }
    }

    /// Moves the entry at play position `from` to `to`.
    pub fn move_item(&mut self, from: usize, to: usize) {
        let len = self.order.len();
        if from >= len || to >= len || from == to {
            return;
        }
        if self.shuffle {
            let item = self.order.remove(from);
            self.order.insert(to, item);
        } else {
            let entry = self.entries.remove(from);
            self.entries.insert(to, entry);
        }
        if let Some(c) = self.cursor {
            self.cursor = Some(if c == from {
                to
            } else if from < c && to >= c {
                c - 1
            } else if from > c && to <= c {
                c + 1
            } else {
                c
            });
        }
    }

    /// Removes the entry at play position `pos`. If it was current, the cursor stays
    /// on the entry that moved into its place (or the previous one at the end).
    pub fn remove(&mut self, pos: usize) {
        if pos >= self.order.len() {
            return;
        }
        let idx = self.order.remove(pos);
        self.entries.remove(idx);
        for i in &mut self.order {
            if *i > idx {
                *i -= 1;
            }
        }
        if self.order.is_empty() {
            self.cursor = None;
            return;
        }
        if let Some(c) = self.cursor {
            if pos < c {
                self.cursor = Some(c - 1);
            } else if pos == c {
                self.cursor = Some(c.min(self.order.len() - 1));
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.cursor = None;
        self.position_ms = 0;
    }

    /// Jumps to play position `pos`.
    pub fn select(&mut self, pos: usize) -> Option<&QueueEntry> {
        if pos < self.order.len() {
            self.cursor = Some(pos);
            self.position_ms = 0;
        }
        self.current()
    }

    /// The position that follows the current one. `auto` is true when the track ended
    /// by itself (repeat-one replays it then; a manual "next" moves on).
    pub fn peek_next(&self, auto: bool) -> Option<usize> {
        let c = self.cursor?;
        if auto && self.repeat == Repeat::One {
            return Some(c);
        }
        if c + 1 < self.order.len() {
            Some(c + 1)
        } else if self.repeat != Repeat::Off && !self.order.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    /// Advances to the next entry; returns it, or `None` at the end.
    pub fn advance(&mut self, auto: bool) -> Option<&QueueEntry> {
        let next = self.peek_next(auto)?;
        self.cursor = Some(next);
        self.position_ms = 0;
        self.current()
    }

    pub fn peek_previous(&self) -> Option<usize> {
        let c = self.cursor?;
        if c > 0 {
            Some(c - 1)
        } else if self.repeat == Repeat::All && !self.order.is_empty() {
            Some(self.order.len() - 1)
        } else {
            None
        }
    }

    pub fn previous(&mut self) -> Option<&QueueEntry> {
        let prev = self.peek_previous()?;
        self.cursor = Some(prev);
        self.position_ms = 0;
        self.current()
    }

    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.set_shuffle_with(shuffle, &mut default_rng);
    }

    pub fn set_shuffle_with(&mut self, shuffle: bool, rng: Rng<'_>) {
        if shuffle == self.shuffle {
            return;
        }
        self.shuffle = shuffle;
        if shuffle {
            self.reshuffle(rng);
        } else {
            let current = self.cursor.map(|c| self.order[c]);
            self.order = (0..self.entries.len()).collect();
            self.cursor = current;
        }
    }

    /// A permutation with the current entry first.
    fn reshuffle(&mut self, rng: Rng<'_>) {
        let current = self.cursor.map(|c| self.order[c]);
        let mut rest: Vec<usize> = (0..self.entries.len())
            .filter(|i| Some(*i) != current)
            .collect();
        for i in (1..rest.len()).rev() {
            let j = rng(i + 1);
            rest.swap(i, j);
        }
        self.order = current.into_iter().chain(rest).collect();
        self.cursor = current.map(|_| 0);
    }

    // Persistence ---------------------------------------------------------------------

    /// Writes the whole queue (call debounced: at most once per second, and on quit).
    pub fn save(&self, conn: &mut Connection) -> Result<()> {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM queue_item", [])?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO queue_item (position, track_rowid, origin_kind, origin_id) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (i, e) in self.entries.iter().enumerate() {
                insert.execute(params![i as i64, e.track.rowid, e.origin.kind, e.origin.id])?;
            }
        }
        let order: Vec<u8> = self
            .order
            .iter()
            .flat_map(|i| (*i as u32).to_le_bytes())
            .collect();
        let current_entry = self.cursor.map(|c| self.order[c] as i64);
        let mut set = tx.prepare(
            "INSERT INTO queue_state (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )?;
        set.execute(params!["current_position", current_entry])?;
        set.execute(params!["position_ms", self.position_ms as i64])?;
        set.execute(params!["shuffle", self.shuffle])?;
        set.execute(params!["shuffle_order", order])?;
        set.execute(params!["repeat", self.repeat.as_str()])?;
        drop(set);
        tx.commit()?;
        Ok(())
    }

    /// Restores the saved queue (tracks deleted meanwhile are dropped).
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut stmt = conn.prepare(
            "SELECT position, track_rowid, origin_kind, origin_id FROM queue_item ORDER BY position",
        )?;
        let saved: Vec<(i64, i64, Option<String>, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        let ids: Vec<i64> = saved.iter().map(|s| s.1).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        let rows: std::collections::HashMap<i64, TrackRow> = queries::track_rows(conn, &unique)?
            .into_iter()
            .map(|r| (r.rowid, r))
            .collect();

        // Map saved positions to surviving entries.
        let mut remap = vec![None; saved.len()];
        let mut entries = Vec::with_capacity(saved.len());
        for (i, (_, rowid, kind, id)) in saved.into_iter().enumerate() {
            if let Some(track) = rows.get(&rowid) {
                remap[i] = Some(entries.len());
                entries.push(QueueEntry::new(track.clone(), Origin { kind, id }));
            }
        }

        let state = |key: &str| -> rusqlite::Result<Option<rusqlite::types::Value>> {
            conn.query_row("SELECT value FROM queue_state WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .map(Some)
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
        };
        use rusqlite::types::Value;
        let int = |v: Option<Value>| match v {
            Some(Value::Integer(i)) => Some(i),
            _ => None,
        };
        let shuffle = int(state("shuffle")?).unwrap_or(0) != 0;
        let repeat = match state("repeat")? {
            Some(Value::Text(t)) => Repeat::parse(&t),
            _ => Repeat::Off,
        };
        let position_ms = int(state("position_ms")?).unwrap_or(0).max(0) as u64;
        let current_saved = int(state("current_position")?).and_then(|i| usize::try_from(i).ok());

        let mut order: Vec<usize> = match (shuffle, state("shuffle_order")?) {
            (true, Some(Value::Blob(b))) => b
                .as_chunks::<4>()
                .0
                .iter()
                .filter_map(|c| {
                    remap
                        .get(u32::from_le_bytes(*c) as usize)
                        .copied()
                        .flatten()
                })
                .collect(),
            _ => (0..entries.len()).collect(),
        };
        if order.len() != entries.len() {
            order = (0..entries.len()).collect();
        }
        let current_entry = current_saved.and_then(|i| remap.get(i).copied().flatten());
        let cursor = current_entry.and_then(|e| order.iter().position(|&o| o == e));
        let cursor = cursor.or((!entries.is_empty()).then_some(0));
        Ok(Self {
            position_ms: if cursor.is_some() && current_entry.is_some() {
                position_ms
            } else {
                0
            },
            entries,
            order,
            cursor,
            shuffle,
            repeat,
        })
    }
}
