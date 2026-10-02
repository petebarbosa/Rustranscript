//! Classificar uma chamada: mudar de cliente na mesma biblioteca ou levá-la (com áudio,
//! versões, edições e histórico) para outra biblioteca, p.ex. da inbox para uma empresa.
use std::collections::HashMap;
use std::path::Path;

use rusqlite::params;

use crate::app::App;
use crate::library::Library;
use crate::{Error, Result, fsx};

/// Devolve `(biblioteca, id)` da chamada no destino.
pub fn assign(app: &App, from_lib: i64, call_id: i64, to_lib: i64, client_id: Option<i64>) -> Result<(i64, i64)> {
    if from_lib == to_lib {
        app.open_library(from_lib)?.set_client(call_id, client_id)?;
        return Ok((from_lib, call_id));
    }
    let src = app.open_library(from_lib)?;
    let mut dst = app.open_library(to_lib)?;
    if dst.row.is_inbox() && client_id.is_some() {
        return Err(Error::invalid("unclassified calls have no clients"));
    }
    if let Some(cid) = client_id {
        dst.find_client(&cid.to_string())?;
    }
    src.call_exists(call_id)?;
    let (key, slug, dir): (String, String, Option<String>) =
        src.conn.query_row("SELECT key, slug, dir FROM calls WHERE id = ?1", [call_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    if dst.call_id_by_key(&key)?.is_some() {
        return Err(Error::Conflict(format!("call {key} already exists in the destination")));
    }
    let new_dir = dst.call_dir_for(&key, &slug, client_id)?;
    let old_abs = dir.as_deref().map(|d| src.root().join(d));
    let moved = match &old_abs {
        Some(p) if p.exists() => {
            fsx::move_path(p, &dst.root().join(&new_dir))?;
            true
        }
        _ => false,
    };
    let result = copy_rows(&src, &mut dst, call_id, client_id, &new_dir);
    match result {
        Ok(new_id) => {
            src.conn.execute("DELETE FROM calls WHERE id = ?1", [call_id])?;
            if let Some(parent) = old_abs.as_ref().and_then(|p| p.parent()) {
                fsx::prune_empty_dirs(parent, src.root());
            }
            Ok((to_lib, new_id))
        }
        Err(e) => {
            if moved && let Some(p) = &old_abs {
                let _ = fsx::move_path(&dst.root().join(&new_dir), p);
            }
            Err(e)
        }
    }
}

fn copy_rows(src: &Library, dst: &mut Library, call_id: i64, client_id: Option<i64>, new_dir: &Path) -> Result<i64> {
    let s = &src.conn;
    let tx = dst.conn.transaction()?;
    let old_dir: Option<String> = s.query_row("SELECT dir FROM calls WHERE id = ?1", [call_id], |r| r.get(0))?;
    let rebase = |p: Option<String>| -> Option<String> {
        let p = p?;
        match old_dir.as_deref().and_then(|d| std::path::Path::new(&p).strip_prefix(d).ok()) {
            Some(rest) => Some(new_dir.join(rest).to_string_lossy().into_owned()),
            None => Some(p),
        }
    };

    let call = s.query_row(
        "SELECT key, title, slug, started_at, duration_s, language, expected_speakers, mic_path, sys_path,
                audio_deleted_at, created_at, transcription_state, transcription_error FROM calls WHERE id = ?1",
        [call_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, Option<String>>(12)?,
            ))
        },
    )?;
    tx.execute(
        "INSERT INTO calls (key, client_id, title, slug, started_at, duration_s, language, expected_speakers,
                            dir, mic_path, sys_path, audio_deleted_at, created_at,
                            transcription_state, transcription_error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            call.0,
            client_id,
            call.1,
            call.2,
            call.3,
            call.4,
            call.5,
            call.6,
            new_dir.to_string_lossy(),
            rebase(call.7),
            rebase(call.8),
            call.9,
            call.10,
            call.11,
            call.12
        ],
    )?;
    let new_call = tx.last_insert_rowid();

    let mut speakers = HashMap::new();
    {
        let mut st = s.prepare("SELECT id, track, label, name FROM speakers WHERE call_id = ?1")?;
        let rows = st.query_map([call_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
        })?;
        for row in rows {
            let (id, track, label, name) = row?;
            tx.execute(
                "INSERT INTO speakers (call_id, track, label, name) VALUES (?1, ?2, ?3, ?4)",
                params![new_call, track, label, name],
            )?;
            speakers.insert(id, tx.last_insert_rowid());
        }
    }

    let mut blocks = HashMap::new();
    {
        let mut st = s.prepare(
            "SELECT id, version, model, engine, params_json, source_file, created_at, is_active
             FROM transcripts WHERE call_id = ?1",
        )?;
        let rows = st.query_map([call_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, bool>(7)?,
            ))
        })?;
        for row in rows {
            let t = row?;
            tx.execute(
                "INSERT INTO transcripts (call_id, version, model, engine, params_json, source_file, created_at, is_active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![new_call, t.1, t.2, t.3, t.4, t.5, t.6, t.7],
            )?;
            let new_t = tx.last_insert_rowid();
            let mut bs = s.prepare(
                "SELECT id, seq, t_start, t_end, speaker_id, original_text, text, edited_at FROM blocks WHERE transcript_id = ?1",
            )?;
            let brows = bs.query_map([t.0], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            })?;
            for b in brows {
                let b = b?;
                tx.execute(
                    "INSERT INTO blocks (transcript_id, seq, t_start, t_end, speaker_id, original_text, text, edited_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![new_t, b.1, b.2, b.3, speakers[&b.4], b.5, b.6, b.7],
                )?;
                blocks.insert(b.0, tx.last_insert_rowid());
            }
        }
    }

    for (sql_sel, sql_ins) in [
        ("SELECT t, title FROM chapters WHERE call_id = ?1", "INSERT INTO chapters (call_id, t, title) VALUES (?1, ?2, ?3)"),
        (
            "SELECT kind, path, size FROM import_sources WHERE call_id = ?1",
            "INSERT INTO import_sources (call_id, kind, path, size) VALUES (?1, ?2, ?3, ?4)",
        ),
    ] {
        let mut st = s.prepare(sql_sel)?;
        let n = st.column_count();
        let rows = st.query_map([call_id], |r| (0..n).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect::<rusqlite::Result<Vec<_>>>())?;
        for row in rows {
            let mut vals = vec![rusqlite::types::Value::Integer(new_call)];
            vals.extend(row?);
            tx.execute(sql_ins, rusqlite::params_from_iter(vals))?;
        }
    }

    {
        let mut st = s.prepare(
            "SELECT entity, entity_id, old_value, new_value, origin, at, undone_at, batch_id, batch_kind
             FROM edit_history WHERE call_id = ?1 ORDER BY id",
        )?;
        let rows = st.query_map([call_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        })?;
        // ids de lote são por biblioteca: no destino cada lote ganha um id novo
        let mut batches: HashMap<i64, i64> = HashMap::new();
        let map_speaker = |v: Option<String>| v.and_then(|v| v.parse::<i64>().ok()).and_then(|id| speakers.get(&id)).map(|id| id.to_string());
        for row in rows {
            let (entity, eid, old, new, origin, at, undone, batch, batch_kind) = row?;
            let (eid, old, new) = match entity.as_str() {
                "block_text" => (blocks.get(&eid).copied(), old, new),
                "block_speaker" => (blocks.get(&eid).copied(), map_speaker(old), map_speaker(new)),
                "call_title" => (Some(new_call), old, new),
                "speaker_name" => (speakers.get(&eid).copied(), old, new),
                _ => (None, old, new),
            };
            let Some(eid) = eid else { continue };
            let new_batch = match batch {
                Some(b) => Some(match batches.get(&b) {
                    Some(n) => *n,
                    None => {
                        let n: i64 = tx.query_row("SELECT coalesce(max(batch_id), 0) + 1 FROM edit_history", [], |r| r.get(0))?;
                        batches.insert(b, n);
                        n
                    }
                }),
                None => None,
            };
            tx.execute(
                "INSERT INTO edit_history (call_id, entity, entity_id, old_value, new_value, origin, at, undone_at, batch_id, batch_kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![new_call, entity, eid, old, new, origin, at, undone, new_batch, batch_kind],
            )?;
        }
    }
    tx.commit()?;
    Ok(new_call)
}
