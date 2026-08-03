use std::path::Path;

use rusqlite::Connection;

use familiar_core::FamiliarError;

use crate::migrate;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> familiar_core::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(|e| FamiliarError::Database(e.to_string()))?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> familiar_core::Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| FamiliarError::Database(e.to_string()))?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> familiar_core::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn run_migrations(&self) -> familiar_core::Result<usize> {
        migrate::run_migrations(&self.conn)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}
