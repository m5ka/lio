//! SQLite connection, schema migrations and a handle that runs work off the UI thread.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Ordered migrations; `PRAGMA user_version` stores how many have been applied.
const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_library.sql")];

/// Shared connections. Writes (and async work via [`Db::run`] on Tokio's blocking pool)
/// go through one connection; [`Db::read`] uses a second, read-only connection so the UI
/// thread never waits behind a long sync transaction (WAL allows concurrent readers).
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    reader: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        configure(&conn)?;
        migrate(&mut conn)?;
        let reader = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        reader.busy_timeout(std::time::Duration::from_secs(5))?;
        reader.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            reader: Arc::new(Mutex::new(reader)),
        })
    }

    /// In-memory database (tests). Reads share the writer connection.
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrate(&mut conn)?;
        let conn = Arc::new(Mutex::new(conn));
        Ok(Self {
            reader: conn.clone(),
            conn,
        })
    }

    /// Runs `f` with the connection on the current thread.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut conn)
    }

    /// Runs a read-only query on the reader connection (safe on the UI thread: queries by
    /// rowid take well under a millisecond and never wait for writers).
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.reader.lock().unwrap_or_else(|p| p.into_inner());
        f(&conn)
    }

    /// Runs `f` on Tokio's blocking pool. Must be called inside a Tokio runtime.
    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.with(f))
            .await
            .map_err(|e| Error::Other(format!("database task failed: {e}")))?
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = usize::try_from(current).unwrap_or(0);
    if current > MIGRATIONS.len() {
        return Err(Error::Other(format!(
            "database schema v{current} is newer than this version of Lio supports"
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
        tracing::info!(version = i + 1, "applied database migration");
    }
    Ok(())
}

/// Current Unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x/library.db");
        let db = Db::open(&path).unwrap();
        let v: i64 = db
            .with(|c| Ok(c.pragma_query_value(None, "user_version", |r| r.get(0))?))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        drop(db);
        let db = Db::open(&path).unwrap();
        db.with(|c| Ok(c.execute("INSERT INTO sync_state (key, value) VALUES ('k', 'v')", [])?))
            .unwrap();
        let v: String = db
            .read(|c| Ok(c.query_row("SELECT value FROM sync_state", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(v, "v", "reader sees committed writes");
        assert!(
            db.read(|c| Ok(c.execute("DELETE FROM sync_state", [])?))
                .is_err(),
            "reader is read-only"
        );
    }

    #[test]
    fn fts5_and_constraints() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO search_fts (kind, ref_rowid, text) VALUES ('artist', 1, 'Björk')",
                [],
            )?;
            let n: i64 = c.query_row(
                "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'bjork'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 1);
            let bad = c.execute(
                "INSERT INTO download (kind, ref_id, quality) VALUES ('artist', 'x', 'original')",
                [],
            );
            assert!(bad.is_err(), "kind CHECK constraint must reject 'artist'");
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn run_on_blocking_pool() {
        let db = Db::open_in_memory().unwrap();
        let n: i64 = db
            .run(|c| Ok(c.query_row("SELECT 41 + 1", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 42);
    }
}
