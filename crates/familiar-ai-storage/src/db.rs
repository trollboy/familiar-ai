use std::path::Path;

use rusqlite::Connection;

use familiar_ai_core::FamiliarError;

use crate::migrate;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> familiar_ai_core::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(|e| FamiliarError::Database(e.to_string()))?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> familiar_ai_core::Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| FamiliarError::Database(e.to_string()))?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> familiar_ai_core::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;",
        )
        .map_err(|e| FamiliarError::Database(e.to_string()))?;
        // FAM-BUG-102: the same 30 s bound a busy_timeout gave, but every
        // two seconds of waiting is printed, so a lock episode names the
        // process that waited and for how long instead of ending in a bare
        // "database is locked" after the work is done.
        conn.busy_handler(Some(busy_wait))
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn run_migrations(&self) -> familiar_ai_core::Result<usize> {
        migrate::run_migrations(&self.conn)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// SQLite busy handler: sleep 50 ms per call, report every two seconds,
/// give up after thirty. `count` is the number of prior invocations for this
/// lock attempt, so the elapsed estimate needs no shared state.
fn busy_wait(count: i32) -> bool {
    const STEP_MS: u64 = 50;
    let waited_ms = count.max(0) as u64 * STEP_MS;
    if waited_ms >= 30_000 {
        eprintln!("sqlite: gave up after waiting 30s for the write lock");
        return false;
    }
    if count > 0 && waited_ms % 2_000 == 0 {
        eprintln!(
            "sqlite: waited {}s for the write lock (pid {})",
            waited_ms / 1000,
            std::process::id()
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(STEP_MS));
    true
}
