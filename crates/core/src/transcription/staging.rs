//! Bruto da transcrição no `library.db` (`tx_*`), gravado a cada janela do Whisper. Tudo por `job_id`.
use serde::{Deserialize, Serialize};

use super::protocol::WordTime;
use rusqlite::{OptionalExtension, params};

use crate::{Error, Library, Result, db};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Track {
    Mic,
    Sys,
}

impl Track {
    pub fn as_str(self) -> &'static str {
        match self {
            Track::Mic => "mic",
            Track::Sys => "sys",
        }
    }
}

/// Etapas com marca de conclusão em `tx_stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKey {
    AsrSys,
    AsrMic,
    Energy,
    Diarize,
}

impl StageKey {
    pub fn as_str(self) -> &'static str {
        match self {
            StageKey::AsrSys => "asr_sys",
            StageKey::AsrMic => "asr_mic",
            StageKey::Energy => "energy",
            StageKey::Diarize => "diarize",
        }
    }
}

/// Trecho transcrito, tempos do arquivo da trilha.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: Vec<WordTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    /// rótulo bruto do worker (0, 1, ...)
    pub cluster: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Energy {
    pub step_ms: u32,
    pub db: Vec<f32>,
}

const TABLES: [&str; 4] = ["tx_stage", "tx_segments", "tx_turns", "tx_energy"];

/// Grava UM segmento (transação própria; `seq` = próximo da trilha, a partir de 1). Idempotente: repetir o
/// mesmo segmento (mesmos tempos e texto) logo depois do último não duplica.
pub fn push_segment(lib: &mut Library, job_id: i64, track: Track, seg: &Segment) -> Result<()> {
    let tx = lib.conn.transaction()?;
    let last: Option<(i64, f64, f64, String)> = tx
        .query_row(
            "SELECT seq, t_start, t_end, text FROM tx_segments WHERE job_id = ?1 AND track = ?2 ORDER BY seq DESC LIMIT 1",
            params![job_id, track.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if let Some((_, s, e, t)) = &last
        && (*s, *e, t.as_str()) == (seg.start, seg.end, seg.text.as_str())
    {
        return Ok(());
    }
    let words = if seg.words.is_empty() { None } else { Some(serde_json::to_string(&seg.words)?) };
    tx.execute(
        "INSERT INTO tx_segments (job_id, track, seq, t_start, t_end, text, words_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![job_id, track.as_str(), last.map_or(1, |l| l.0 + 1), seg.start, seg.end, seg.text, words],
    )?;
    tx.commit()?;
    Ok(())
}

/// Onde retomar o ASR da trilha: o maior `t_end` já gravado (0.0 se nada).
pub fn resume_point(lib: &Library, job_id: i64, track: Track) -> Result<f64> {
    Ok(lib.conn.query_row(
        "SELECT coalesce(max(t_end), 0.0) FROM tx_segments WHERE job_id = ?1 AND track = ?2",
        params![job_id, track.as_str()],
        |r| r.get(0),
    )?)
}

pub fn segments(lib: &Library, job_id: i64, track: Track) -> Result<Vec<Segment>> {
    let mut stmt = lib
        .conn
        .prepare("SELECT t_start, t_end, text, words_json FROM tx_segments WHERE job_id = ?1 AND track = ?2 ORDER BY seq")?;
    let rows = stmt.query_map(params![job_id, track.as_str()], |r| {
        Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (start, end, text, words) = row?;
        let words = match words {
            Some(w) => serde_json::from_str(&w)?,
            None => Vec::new(),
        };
        out.push(Segment { start, end, text, words });
    }
    Ok(out)
}

/// Troca TODOS os turnos do job (a diarização é atômica: ou chegou o `result` ou não há nada).
pub fn set_turns(lib: &mut Library, job_id: i64, turns: &[Turn]) -> Result<()> {
    let tx = lib.conn.transaction()?;
    tx.execute("DELETE FROM tx_turns WHERE job_id = ?1", [job_id])?;
    for (i, t) in turns.iter().enumerate() {
        tx.execute(
            "INSERT INTO tx_turns (job_id, seq, t_start, t_end, cluster) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job_id, i as i64 + 1, t.start, t.end, t.cluster],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn turns(lib: &Library, job_id: i64) -> Result<Vec<Turn>> {
    let mut stmt = lib.conn.prepare("SELECT t_start, t_end, cluster FROM tx_turns WHERE job_id = ?1 ORDER BY seq")?;
    let rows = stmt.query_map([job_id], |r| Ok(Turn { start: r.get(0)?, end: r.get(1)?, cluster: r.get(2)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn set_energy(lib: &mut Library, job_id: i64, track: Track, energy: &Energy) -> Result<()> {
    lib.conn.execute(
        "INSERT OR REPLACE INTO tx_energy (job_id, track, step_ms, db_json) VALUES (?1, ?2, ?3, ?4)",
        params![job_id, track.as_str(), energy.step_ms, serde_json::to_string(&energy.db)?],
    )?;
    Ok(())
}

pub fn energy(lib: &Library, job_id: i64, track: Track) -> Result<Option<Energy>> {
    let row: Option<(u32, String)> = lib
        .conn
        .query_row("SELECT step_ms, db_json FROM tx_energy WHERE job_id = ?1 AND track = ?2", params![job_id, track.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    row.map(|(step_ms, db)| Ok(Energy { step_ms, db: serde_json::from_str(&db)? })).transpose()
}

pub fn mark_stage_done(lib: &mut Library, job_id: i64, stage: StageKey, info: Option<&serde_json::Value>) -> Result<()> {
    lib.conn.execute(
        "INSERT OR REPLACE INTO tx_stage (job_id, stage, done_at, info_json) VALUES (?1, ?2, ?3, ?4)",
        params![job_id, stage.as_str(), db::now(), info.map(|v| v.to_string())],
    )?;
    Ok(())
}

pub fn stage_done(lib: &Library, job_id: i64, stage: StageKey) -> Result<bool> {
    Ok(lib
        .conn
        .query_row("SELECT 1 FROM tx_stage WHERE job_id = ?1 AND stage = ?2", params![job_id, stage.as_str()], |_| Ok(()))
        .optional()?
        .is_some())
}

/// `info_json` gravado junto da marca da etapa (ex.: idioma detectado no ASR).
pub fn stage_info(lib: &Library, job_id: i64, stage: StageKey) -> Result<Option<serde_json::Value>> {
    let raw: Option<Option<String>> = lib
        .conn
        .query_row("SELECT info_json FROM tx_stage WHERE job_id = ?1 AND stage = ?2", params![job_id, stage.as_str()], |r| r.get(0))
        .optional()?;
    raw.flatten().map(|s| Ok(serde_json::from_str(&s)?)).transpose()
}

/// Há algum bruto gravado para o job (marca de etapa ou segmentos)?
pub fn has_raw(lib: &Library, job_id: i64) -> Result<bool> {
    Ok(lib
        .conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM tx_stage WHERE job_id = ?1) OR EXISTS (SELECT 1 FROM tx_segments WHERE job_id = ?1)",
            [job_id],
            |r| r.get(0),
        )?)
}

/// Copia o bruto de `from_job` para `to_job` (rediarize/resegment partem do bruto da versão base sem
/// mexer nele). `with_turns = false` no rediarize (turnos novos virão do worker). Substitui o que `to_job`
/// já tivesse, numa transação.
pub fn clone_raw(lib: &mut Library, from_job: i64, to_job: i64, with_turns: bool) -> Result<()> {
    if from_job == to_job {
        return Err(Error::invalid("clone_raw: same job"));
    }
    let tx = lib.conn.transaction()?;
    delete_rows(&tx, to_job)?;
    tx.execute(
        "INSERT INTO tx_segments (job_id, track, seq, t_start, t_end, text, words_json)
         SELECT ?2, track, seq, t_start, t_end, text, words_json FROM tx_segments WHERE job_id = ?1",
        params![from_job, to_job],
    )?;
    tx.execute(
        "INSERT INTO tx_energy (job_id, track, step_ms, db_json) SELECT ?2, track, step_ms, db_json FROM tx_energy WHERE job_id = ?1",
        params![from_job, to_job],
    )?;
    tx.execute(
        "INSERT INTO tx_stage (job_id, stage, done_at, info_json)
         SELECT ?2, stage, done_at, info_json FROM tx_stage WHERE job_id = ?1 AND (?3 OR stage <> 'diarize')",
        params![from_job, to_job, with_turns],
    )?;
    if with_turns {
        tx.execute(
            "INSERT INTO tx_turns (job_id, seq, t_start, t_end, cluster) SELECT ?2, seq, t_start, t_end, cluster FROM tx_turns WHERE job_id = ?1",
            params![from_job, to_job],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn delete_rows(conn: &rusqlite::Connection, job_id: i64) -> Result<()> {
    for t in TABLES {
        conn.execute(&format!("DELETE FROM {t} WHERE job_id = ?1"), [job_id])?;
    }
    Ok(())
}

/// Apaga o bruto de um job que NÃO virou versão (cancelado/descartado). Recusa (`conflict`) se alguma versão
/// aponta para ele.
pub fn purge(lib: &mut Library, job_id: i64) -> Result<()> {
    let tx = lib.conn.transaction()?;
    let used: bool =
        tx.query_row("SELECT EXISTS (SELECT 1 FROM transcripts WHERE raw_job_id = ?1)", [job_id], |r| r.get(0))?;
    if used {
        return Err(Error::Conflict(format!("job {job_id} already has a transcript version")));
    }
    delete_rows(&tx, job_id)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::App::open(dir.path()).unwrap();
        let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
        (dir, lib)
    }

    fn seg(a: f64, b: f64, t: &str) -> Segment {
        Segment { start: a, end: b, text: t.into(), words: vec![WordTime(a, b, t.into())] }
    }

    #[test]
    fn segments_resume_clone_and_purge() {
        let (_d, mut lib) = lib();
        assert_eq!(resume_point(&lib, 1, Track::Sys).unwrap(), 0.0);
        push_segment(&mut lib, 1, Track::Sys, &seg(0.0, 4.5, "a")).unwrap();
        push_segment(&mut lib, 1, Track::Sys, &seg(0.0, 4.5, "a")).unwrap(); // repetido: ignorado
        push_segment(&mut lib, 1, Track::Sys, &seg(5.0, 9.5, "b")).unwrap();
        push_segment(&mut lib, 1, Track::Mic, &seg(1.0, 2.0, "m")).unwrap();
        assert_eq!(resume_point(&lib, 1, Track::Sys).unwrap(), 9.5);
        let got = segments(&lib, 1, Track::Sys).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], seg(5.0, 9.5, "b"));
        set_turns(&mut lib, 1, &[Turn { start: 0.0, end: 9.0, cluster: 3 }]).unwrap();
        set_turns(&mut lib, 1, &[Turn { start: 0.0, end: 5.0, cluster: 0 }, Turn { start: 5.0, end: 9.0, cluster: 1 }]).unwrap();
        assert_eq!(turns(&lib, 1).unwrap().len(), 2);
        set_energy(&mut lib, 1, Track::Sys, &Energy { step_ms: 100, db: vec![-20.0, -21.5] }).unwrap();
        assert_eq!(energy(&lib, 1, Track::Sys).unwrap().unwrap().db, vec![-20.0, -21.5]);
        assert!(energy(&lib, 1, Track::Mic).unwrap().is_none());
        for st in [StageKey::AsrSys, StageKey::Diarize] {
            mark_stage_done(&mut lib, 1, st, Some(&serde_json::json!({"language": "pt"}))).unwrap();
        }
        assert!(stage_done(&lib, 1, StageKey::AsrSys).unwrap() && !stage_done(&lib, 1, StageKey::Energy).unwrap());
        assert_eq!(stage_info(&lib, 1, StageKey::AsrSys).unwrap().unwrap()["language"], "pt");

        clone_raw(&mut lib, 1, 2, false).unwrap();
        assert_eq!(segments(&lib, 2, Track::Sys).unwrap().len(), 2);
        assert!(turns(&lib, 2).unwrap().is_empty() && !stage_done(&lib, 2, StageKey::Diarize).unwrap());
        assert!(stage_done(&lib, 2, StageKey::AsrSys).unwrap());
        clone_raw(&mut lib, 1, 3, true).unwrap();
        assert_eq!(turns(&lib, 3).unwrap().len(), 2);
        assert!(stage_done(&lib, 3, StageKey::Diarize).unwrap());

        // versão apontando para o bruto 3: purge recusa; nos outros apaga tudo
        lib.conn.execute("INSERT INTO calls (key, started_at, created_at) VALUES ('call_x', 't', 't')", []).unwrap();
        lib.conn.execute("INSERT INTO transcripts (call_id, version, created_at, raw_job_id) VALUES (1, 1, 't', 3)", []).unwrap();
        assert_eq!(purge(&mut lib, 3).unwrap_err().code(), "conflict");
        purge(&mut lib, 2).unwrap();
        assert!(!has_raw(&lib, 2).unwrap() && has_raw(&lib, 1).unwrap());
    }
}
