//! SQLite persistence (WAL) for MAW. State tables are authoritative; events are an append-only log.
//!
//! All access goes through [`Store::read`] / [`Store::write`]; `write` runs the closure in a
//! transaction that commits on `Ok` and rolls back on `Err`.

mod db;

pub use db::Db;

use maw_core::{CoreError, CoreResult};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_init",
        include_str!("../../../migrations/0001_init.sql"),
    ),
    (
        "0002_agent_effort",
        include_str!("../../../migrations/0002_agent_effort.sql"),
    ),
];

pub struct Store {
    conn: Mutex<Connection>,
}

pub(crate) fn se(e: rusqlite::Error) -> CoreError {
    CoreError::Storage(e.to_string())
}

impl Store {
    /// Open (creating and migrating if needed) a database file.
    pub fn open(path: impl AsRef<Path>) -> CoreResult<Store> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::Storage(e.to_string()))?;
            }
        }
        let conn = Connection::open(path).map_err(se)?;
        Self::init(conn, true)
    }

    pub fn open_in_memory() -> CoreResult<Store> {
        Self::init(Connection::open_in_memory().map_err(se)?, false)
    }

    fn init(conn: Connection, wal: bool) -> CoreResult<Store> {
        if wal {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(se)?;
            conn.pragma_update(None, "synchronous", "NORMAL")
                .map_err(se)?;
        }
        conn.pragma_update(None, "foreign_keys", "ON").map_err(se)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(se)?;
        let store = Store {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> CoreResult<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CoreError::Internal("store poisoned".into()))?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_at TEXT NOT NULL);")
            .map_err(se)?;
        for (name, sql) in MIGRATIONS {
            let applied: bool = conn
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE name = ?1",
                    [name],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if applied {
                continue;
            }
            let tx = conn.transaction().map_err(se)?;
            tx.execute_batch(sql).map_err(se)?;
            tx.execute(
                "INSERT INTO schema_migrations(name, applied_at) VALUES (?1, ?2)",
                rusqlite::params![name, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(se)?;
            tx.commit().map_err(se)?;
            tracing::info!(migration = name, "applied migration");
        }
        Ok(())
    }

    pub fn read<R>(&self, f: impl FnOnce(&Db) -> CoreResult<R>) -> CoreResult<R> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CoreError::Internal("store poisoned".into()))?;
        f(&Db(&conn))
    }

    pub fn write<R>(&self, f: impl FnOnce(&Db) -> CoreResult<R>) -> CoreResult<R> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CoreError::Internal("store poisoned".into()))?;
        let tx = conn.transaction().map_err(se)?;
        match f(&Db(&tx)) {
            Ok(v) => {
                tx.commit().map_err(se)?;
                Ok(v)
            }
            Err(e) => {
                // dropping `tx` rolls back
                Err(e)
            }
        }
    }

    /// Database file size information is not tracked; expose the schema version for diagnostics.
    pub fn applied_migrations(&self) -> CoreResult<Vec<String>> {
        self.read(|db| db.applied_migrations())
    }
}
