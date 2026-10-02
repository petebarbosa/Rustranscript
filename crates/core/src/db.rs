//! Abertura de bancos SQLite e migrações por `PRAGMA user_version`.
use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::{Error, Result};

pub fn open(path: &Path, migrations: &[&str]) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&conn, migrations)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // GUI e CLI podem escrever ao mesmo tempo: WAL + espera em vez de erro imediato.
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

pub fn migrate(conn: &Connection, migrations: &[&str]) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let supported = migrations.len() as i64;
    if current > supported {
        return Err(Error::SchemaTooNew { found: current, supported });
    }
    for (i, sql) in migrations.iter().enumerate().skip(current as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// Deixa o banco consistente para cópia (backup = copiar a pasta).
pub fn checkpoint(conn: &Connection) {
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}
