//! Abertura de bancos SQLite e migrações por `PRAGMA user_version`.
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};

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
    // Trocar para WAL num banco novo pede lock exclusivo e o SQLite pode devolver BUSY sem passar pelo
    // `busy_timeout`: repetir até o prazo (GUI e CLI abrindo o mesmo banco novo ao mesmo tempo).
    retry_busy(|| conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0)).map(|_| ()))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Repete `f` enquanto o SQLite responder `DatabaseBusy` (até ~10 s, passos de 10–50 ms).
fn retry_busy<T>(mut f: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut wait = 10;
    loop {
        match f() {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(wait));
                wait = (wait * 2).min(50);
            }
            other => return other,
        }
    }
}

pub fn migrate(conn: &Connection, migrations: &[&str]) -> Result<()> {
    let supported = migrations.len() as i64;
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current > supported {
        return Err(Error::SchemaTooNew { found: current, supported });
    }
    for (i, sql) in migrations.iter().enumerate().skip(current as usize) {
        // `BEGIN IMMEDIATE` + reler a versão DENTRO da transação: GUI e CLI abrindo o banco novo ao mesmo
        // tempo não aplicam a mesma migração duas vezes ("table ... already exists"); quem chega depois
        // espera (busy_timeout) e encontra a migração já aplicada.
        let tx = retry_busy(|| Transaction::new_unchecked(conn, TransactionBehavior::Immediate))?;
        let now: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if now > i as i64 {
            continue; // outro processo aplicou (o rollback acontece no drop)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Vários processos (aqui, threads com conexões próprias) abrindo o mesmo banco novo ao mesmo tempo:
    /// nenhum pode falhar e o esquema final tem todas as migrações, aplicadas uma única vez.
    #[test]
    fn concurrent_open_applies_each_migration_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.db");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    open(&path, crate::schema::LIBRARY_MIGRATIONS).map(|_| ())
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("open must not fail under concurrency");
        }
        let conn = open(&path, crate::schema::LIBRARY_MIGRATIONS).unwrap();
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, crate::schema::LIBRARY_MIGRATIONS.len() as i64);
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.db");
        open(&path, crate::schema::APP_MIGRATIONS).unwrap();
        let err = open(&path, &crate::schema::APP_MIGRATIONS[..1]).map(|_| ()).unwrap_err();
        assert!(matches!(err, Error::SchemaTooNew { .. }));
    }
}
