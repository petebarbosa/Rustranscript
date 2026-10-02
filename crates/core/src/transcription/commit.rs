//! Criação da versão: UMA transação no `library.db` (transcrição + falantes + blocos + remoções + estado da
//! chamada). Nada de versão pela metade: se a transação não fecha, nada existe e a tarefa repete do bruto.
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::assemble::Assembled;
use super::queue::JobInfo;
use crate::glossary::{Engine, ReplaceRule};
use crate::text::MAX_BLOCK_CHARS;
use crate::{Library, Result, db};

/// Modelo/motor gravados na versão.
pub const MODEL: &str = "large-v3-turbo";
pub const ENGINE: &str = "faster-whisper";

#[derive(Debug, Clone, Serialize)]
pub struct Committed {
    pub transcript_id: i64,
    pub version: i64,
    pub blocks: usize,
    pub removals: usize,
}

/// Ordem DENTRO da transação:
/// 1. se já existe `transcripts.raw_job_id = job.id` → devolve essa versão (idempotente, não duplica);
/// 2. `INSERT transcripts` (`version = max+1`, `engine = "faster-whisper"`, `model`, `params_json`, `raw_job_id`,
///    `is_active = 1` e desativa a anterior);
/// 3. garante `speakers` (`Eu`/mic e `Pessoa N`/sys; `UNIQUE (call_id, label)` reaproveita os existentes,
///    mantendo `name`);
/// 4. `INSERT blocks` (`original_text` = texto do ASR; `text` = depois do glossário vigente, como na importação,
///    `rules::`), `bleed_removals`;
/// 5. `calls`: `language` detectado (se era `auto`), `transcription_state = 'done'`, `transcription_error = NULL`.
/// Depois (fora da transação): `data-changed` é do shell.
pub fn commit_version(lib: &mut Library, job: &JobInfo, assembled: &Assembled, params_json: &serde_json::Value) -> Result<Committed> {
    commit_version_with(lib, job, assembled, params_json, &[])
}

/// Igual a `commit_version`, mas aplicando `rules` (regras `replace` em vigor, já resolvidas pelo chamador:
/// as globais vivem no `app.db`, que o commit não enxerga) ao `text` de cada bloco. `original_text` = ASR puro;
/// sem histórico de edição e sem `edited_at`. `params_json["detected_language"]` (+ `"language": "auto"`)
/// atualiza `calls.language`.
pub fn commit_version_with(
    lib: &mut Library,
    job: &JobInfo,
    assembled: &Assembled,
    params_json: &serde_json::Value,
    rules: &[ReplaceRule],
) -> Result<Committed> {
    let glossary = Engine::new(rules)?;
    let tx = lib.conn.transaction()?;
    // 1. idempotência: a versão deste bruto já existe
    let existing: Option<(i64, i64)> = tx
        .query_row("SELECT id, version FROM transcripts WHERE raw_job_id = ?1", [job.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    if let Some((transcript_id, version)) = existing {
        let count = |sql: &str| tx.query_row(sql, [transcript_id], |r| r.get::<_, i64>(0));
        return Ok(Committed {
            transcript_id,
            version,
            blocks: count("SELECT count(*) FROM blocks WHERE transcript_id = ?1")? as usize,
            removals: count("SELECT count(*) FROM bleed_removals WHERE transcript_id = ?1")? as usize,
        });
    }
    // 2. versão nova, ativa
    let version: i64 =
        tx.query_row("SELECT coalesce(max(version), 0) + 1 FROM transcripts WHERE call_id = ?1", [job.call_id], |r| r.get(0))?;
    tx.execute("UPDATE transcripts SET is_active = 0 WHERE call_id = ?1", [job.call_id])?;
    tx.execute(
        "INSERT INTO transcripts (call_id, version, model, engine, params_json, created_at, is_active, raw_job_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
        params![job.call_id, version, MODEL, ENGINE, params_json.to_string(), db::now(), job.id],
    )?;
    let transcript_id = tx.last_insert_rowid();
    // 3 e 4. falantes (reaproveita (call, rótulo): mantém `name`) e blocos
    let mut speaker_ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for (seq, b) in assembled.blocks.iter().enumerate() {
        let sid = match speaker_ids.get(&b.speaker) {
            Some(id) => *id,
            None => {
                let found: Option<i64> = tx
                    .query_row("SELECT id FROM speakers WHERE call_id = ?1 AND label = ?2", params![job.call_id, b.speaker], |r| r.get(0))
                    .optional()?;
                let id = match found {
                    Some(id) => id,
                    None => {
                        tx.execute(
                            "INSERT INTO speakers (call_id, track, label) VALUES (?1, ?2, ?3)",
                            params![job.call_id, b.track.as_str(), b.speaker],
                        )?;
                        tx.last_insert_rowid()
                    }
                };
                speaker_ids.insert(b.speaker.clone(), id);
                id
            }
        };
        let applied = glossary.apply(&b.text).text;
        let text = if applied.chars().count() > MAX_BLOCK_CHARS { &b.text } else { &applied };
        tx.execute(
            "INSERT INTO blocks (transcript_id, seq, t_start, t_end, speaker_id, original_text, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![transcript_id, seq as i64 + 1, b.t_start, b.t_end, sid, b.text, text],
        )?;
    }
    for r in &assembled.removals {
        tx.execute(
            "INSERT INTO bleed_removals (transcript_id, t_start, t_end, text, containment, margin_db, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![transcript_id, r.t_start, r.t_end, r.text, r.containment, r.margin_db, r.reason],
        )?;
    }
    // 5. a chamada
    if params_json["language"] == "auto"
        && let Some(code) = params_json["detected_language"].as_str().and_then(crate::recording::language_code)
        && code != "auto"
    {
        tx.execute("UPDATE calls SET language = ?1 WHERE id = ?2", params![code, job.call_id])?;
    }
    tx.execute("UPDATE calls SET transcription_state = 'done', transcription_error = NULL WHERE id = ?1", [job.call_id])?;
    tx.commit()?;
    Ok(Committed { transcript_id, version, blocks: assembled.blocks.len(), removals: assembled.removals.len() })
}

#[derive(Debug, Clone, Serialize)]
pub struct BleedRemoval {
    pub id: i64,
    pub t_start: f64,
    pub t_end: f64,
    pub text: String,
    pub containment: Option<f64>,
    pub margin_db: Option<f64>,
    pub reason: String,
}

/// Segmentos do mic descartados pelo filtro de vazamento nesta versão (auditoria; ordem por `t_start`).
pub fn bleed_removals(lib: &Library, transcript_id: i64) -> Result<Vec<BleedRemoval>> {
    let mut stmt = lib.conn.prepare(
        "SELECT id, t_start, t_end, text, containment, margin_db, reason FROM bleed_removals WHERE transcript_id = ?1 ORDER BY t_start, id",
    )?;
    let rows = stmt.query_map([transcript_id], |r| {
        Ok(BleedRemoval {
            id: r.get(0)?,
            t_start: r.get(1)?,
            t_end: r.get(2)?,
            text: r.get(3)?,
            containment: r.get(4)?,
            margin_db: r.get(5)?,
            reason: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
