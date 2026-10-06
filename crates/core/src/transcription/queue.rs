//! Fila de transcrição: tabela `transcription_jobs` em `app.db`. A fila é da GUI (thread no shell), mas
//! a CLI também grava aqui (`enqueue`) e a GUI pega por polling (~2 s). Uma tarefa em aberto por chamada.
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::params::JobOptions;
use super::staging;
use crate::library::{DB_FILE, Library};
use crate::{App, Error, Result, db};

/// Tentativas (execuções) antes de uma queda do worker virar `failed`.
pub const MAX_ATTEMPTS: i64 = 3;
const SEQ_KEY: &str = "transcription_job_seq";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// ASR das duas trilhas + energia + diarização + montagem + versão nova.
    Full,
    /// Só diariza de novo (usa o ASR/energia do bruto da versão base) e monta versão nova.
    Rediarize,
    /// Só remonta (filtro de vazamento, fusão de clusters...) do bruto da versão base; sem worker.
    Resegment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Linha da fila (e payload JSON para a UI/CLI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: i64,
    pub library_id: i64,
    pub call_id: i64,
    pub call_key: String,
    pub kind: JobKind,
    pub state: JobState,
    pub options: JobOptions,
    /// Bruto-base (rediarize/resegment); `None` em `full`.
    pub base_job_id: Option<i64>,
    pub attempts: i64,
    /// Etapa corrente (`runner::Stage`, em snake_case) e fração 0..1 da etapa (None = indeterminado).
    pub stage: Option<String>,
    pub progress: Option<f64>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Só preenchidos por `status` (nas `queued`): por que ela não começa e quantas tarefas estão na frente
    /// (a rodando + as enfileiradas antes). `None` em `get`/`enqueue` e nas outras tarefas.
    #[serde(default)]
    pub blocked_by: Option<BlockedBy>,
    #[serde(default)]
    pub ahead: Option<i64>,
}

/// Por que uma tarefa (ou a fila toda) não anda. Em ordem de precedência (a primeira que vale ganha; é a ordem em
/// que o laço do shell testa): pausa do usuário → pausa por gravação → instalação em curso → runtime ausente →
/// runtime desatualizado → modelos ausentes. `behind` só existe por tarefa: a fila pode andar, mas há outra na frente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedBy {
    PausedUser,
    PausedRecording,
    RuntimeInstalling,
    RuntimeMissing,
    RuntimeOutdated,
    ModelsMissing,
    Behind,
}

/// O que o shell sabe e o núcleo não: pausa, estado do runtime (`RuntimeStatus.state`), modelos e instalação.
#[derive(Debug, Clone, Copy)]
pub struct Gate<'a> {
    pub paused: Option<PauseReason>,
    /// `missing` | `outdated` | `ready` | `fake`
    pub runtime: &'a str,
    /// Os 3 modelos instalados (ou worker falso, que dispensa).
    pub models_ready: bool,
    /// Instalação do runtime em andamento (a fase de modelos não conta: o runtime já está pronto).
    pub installing_runtime: bool,
}

impl Gate<'static> {
    /// Nada impede a fila (testes e quem não liga para o ambiente).
    pub const READY: Gate<'static> = Gate { paused: None, runtime: "ready", models_ready: true, installing_runtime: false };
}

impl Gate<'_> {
    /// Motivo que vale para a fila inteira; `None` = pode rodar a próxima tarefa.
    pub fn blocker(&self) -> Option<BlockedBy> {
        match self.paused {
            Some(PauseReason::User) => return Some(BlockedBy::PausedUser),
            Some(PauseReason::Recording) => return Some(BlockedBy::PausedRecording),
            None => {}
        }
        if !matches!(self.runtime, "ready" | "fake") {
            return Some(if self.installing_runtime {
                BlockedBy::RuntimeInstalling
            } else if self.runtime == "outdated" {
                BlockedBy::RuntimeOutdated
            } else {
                BlockedBy::RuntimeMissing
            });
        }
        (!self.models_ready).then_some(BlockedBy::ModelsMissing)
    }
}

/// Por que a fila não está andando. `user`: configuração `transcription_queue_paused`; `recording`: há
/// gravação em curso (o shell decide; o núcleo só carrega o valor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    User,
    Recording,
}

/// Estado completo para a UI/CLI. `jobs`: a rodando primeiro, depois as enfileiradas (por id) e as
/// últimas `recent` terminadas (mais novas primeiro).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueStatus {
    pub paused: Option<PauseReason>,
    /// `Gate::blocker`: o que para a fila toda, esteja ela vazia ou não (`null` = anda).
    #[serde(default)]
    pub blocked_by: Option<BlockedBy>,
    pub jobs: Vec<JobInfo>,
}

const COLS: &str = "id, library_id, call_id, call_key, kind, state, options_json, base_job_id, attempts, stage, progress,
                    error_code, error_detail, created_at, started_at, finished_at";

pub(crate) fn enum_str<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<JobInfo> {
    let bad = |what: &str, v: String| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, format!("{what}: {v}").into());
    let kind: String = r.get(4)?;
    let state: String = r.get(5)?;
    let options: String = r.get(6)?;
    Ok(JobInfo {
        id: r.get(0)?,
        library_id: r.get(1)?,
        call_id: r.get(2)?,
        call_key: r.get(3)?,
        kind: serde_json::from_value(kind.clone().into()).map_err(|_| bad("kind", kind))?,
        state: serde_json::from_value(state.clone().into()).map_err(|_| bad("state", state))?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        base_job_id: r.get(7)?,
        attempts: r.get(8)?,
        stage: r.get(9)?,
        progress: r.get(10)?,
        error_code: r.get(11)?,
        error_detail: r.get(12)?,
        created_at: r.get(13)?,
        started_at: r.get(14)?,
        finished_at: r.get(15)?,
        blocked_by: None,
        ahead: None,
    })
}

/// Abre a biblioteca só se estiver disponível (a inbox sempre; as outras com `library.db` no lugar).
/// `None` = removida ou fora do ar (nunca cria pasta/banco novo; é o que o polling de 2 s precisa).
pub(crate) fn open_available(app: &App, library_id: i64) -> Result<Option<Library>> {
    let row = match app.library_row(library_id) {
        Ok(r) => r,
        Err(Error::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    if !row.is_inbox() && !row.root.join(DB_FILE).is_file() {
        return Ok(None);
    }
    Ok(Some(Library::open(row)?))
}


/// Estado da chamada SEM versão (chamada com versão fica `done`; o estado de re-execuções só aparece na fila).
/// Biblioteca fora do ar: ignora.
fn set_call_state(app: &App, job: &JobInfo, state: &str, error: Option<&str>) -> Result<()> {
    if let Some(lib) = open_available(app, job.library_id)? {
        lib.conn.execute(
            "UPDATE calls SET transcription_state = ?1, transcription_error = ?2
             WHERE id = ?3 AND NOT EXISTS (SELECT 1 FROM transcripts WHERE call_id = calls.id)",
            params![state, error, job.call_id],
        )?;
    }
    Ok(())
}

fn insert_job(app: &App, library_id: i64, call_id: i64, call_key: &str, kind: JobKind, options: &JobOptions, base: Option<i64>) -> Result<i64> {
    // `BEGIN IMMEDIATE`: GUI e CLI podem enfileirar ao mesmo tempo. O id nunca é reaproveitado (o bruto
    // `tx_*` das versões é indexado por ele): vem de max(id, contador gravado nas configurações) + 1.
    let tx = Transaction::new_unchecked(&app.db, TransactionBehavior::Immediate)?;
    let open: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM transcription_jobs WHERE library_id = ?1 AND call_id = ?2 AND state IN ('queued', 'running'))",
        params![library_id, call_id],
        |r| r.get(0),
    )?;
    if open {
        return Err(Error::Conflict(format!("call {call_key} already has an open transcription job")));
    }
    let max: i64 = tx.query_row("SELECT coalesce(max(id), 0) FROM transcription_jobs", [], |r| r.get(0))?;
    let seq: i64 = tx
        .query_row("SELECT value FROM settings WHERE key = ?1", [SEQ_KEY], |r| r.get::<_, String>(0))
        .optional()?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let id = max.max(seq) + 1;
    tx.execute(
        "INSERT INTO transcription_jobs (id, library_id, call_id, call_key, kind, state, options_json, base_job_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8)",
        params![id, library_id, call_id, call_key, enum_str(&kind), serde_json::to_string(options)?, base, db::now()],
    )?;
    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SEQ_KEY, id.to_string()],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Cria uma tarefa. Validações: biblioteca disponível e chamada existem (`not_found`); `audio_deleted` quando o
/// áudio foi apagado pelo usuário (`audio_deleted_at`) e `no_audio` quando a chamada nunca teve ou perdeu os
/// arquivos (`resegment` não precisa de áudio); `rediarize`/
/// `resegment` exigem versão ativa com bruto (`no_raw_data`); já há tarefa aberta para a chamada → `conflict`.
/// Chamada sem versão que estava `failed` volta a `pending`.
pub fn enqueue(app: &App, library_id: i64, call_id: i64, kind: JobKind, options: &JobOptions) -> Result<JobInfo> {
    let lib = open_available(app, library_id)?.ok_or_else(|| Error::not_found(format!("library {library_id} is unavailable")))?;
    let (key, mic, sys, deleted): (String, Option<String>, Option<String>, Option<String>) = lib
        .conn
        .query_row("SELECT key, mic_path, sys_path, audio_deleted_at FROM calls WHERE id = ?1", [call_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?
        .ok_or_else(|| Error::not_found(format!("call {library_id}:{call_id}")))?;
    if let Some(l) = &options.language
        && crate::recording::language_code(l).is_none()
    {
        return Err(Error::invalid(format!("unsupported language: {l}")));
    }
    if let Some(n) = options.expected_speakers
        && !(1..=20).contains(&n)
    {
        return Err(Error::invalid("expected speakers must be between 1 and 20"));
    }
    let has_audio = deleted.is_none() && [mic, sys].into_iter().flatten().any(|p| lib.audio_abs(&p).is_file());
    // sem áudio: o motivo certo é "o usuário apagou" (não volta) ou "não há arquivo" (nunca teve / sumiu)
    let no_audio = || {
        if deleted.is_some() {
            Error::transcription("audio_deleted", format!("call {key}"))
        } else {
            Error::transcription("no_audio", format!("call {key} has no audio"))
        }
    };
    let mut base = None;
    match kind {
        JobKind::Full => {
            if !has_audio {
                return Err(no_audio());
            }
        }
        JobKind::Rediarize | JobKind::Resegment => {
            if kind == JobKind::Rediarize && !has_audio {
                return Err(no_audio());
            }
            base = lib
                .conn
                .query_row("SELECT raw_job_id FROM transcripts WHERE call_id = ?1 AND is_active = 1", [call_id], |r| r.get::<_, Option<i64>>(0))
                .optional()?
                .flatten();
            if base.is_none() {
                return Err(Error::transcription("no_raw_data", format!("call {key}: the active version has no raw data")));
            }
        }
    }
    // instantâneo dos cortes de áudio: editar cortes depois não muda uma tarefa já criada
    let mut options = options.clone();
    options.cuts = lib.effective_cuts(call_id)?;
    let id = insert_job(app, library_id, call_id, &key, kind, &options, base)?;
    lib.conn.execute(
        "UPDATE calls SET transcription_state = 'pending', transcription_error = NULL
         WHERE id = ?1 AND transcription_state = 'failed' AND NOT EXISTS (SELECT 1 FROM transcripts WHERE call_id = calls.id)",
        [call_id],
    )?;
    get(app, id)
}

/// Enfileira um `full` para cada chamada `pending` sem tarefa aberta (todas as bibliotecas disponíveis).
/// Chamada sem áudio é ignorada (não falha o lote).
pub fn enqueue_pending(app: &App) -> Result<Vec<JobInfo>> {
    let mut out = Vec::new();
    for row in app.library_rows()? {
        let Some(lib) = open_available(app, row.id)? else { continue };
        let pending: Vec<i64> = {
            let mut stmt = lib.conn.prepare("SELECT id FROM calls WHERE transcription_state = 'pending' ORDER BY id")?;
            stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        drop(lib);
        for call_id in pending {
            match enqueue(app, row.id, call_id, JobKind::Full, &JobOptions::default()) {
                Ok(j) => out.push(j),
                Err(Error::Conflict(_)) | Err(Error::NotFound(_)) | Err(Error::Transcription("no_audio" | "audio_deleted", _)) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(out)
}

pub fn get(app: &App, job_id: i64) -> Result<JobInfo> {
    app.db
        .query_row(&format!("SELECT {COLS} FROM transcription_jobs WHERE id = ?1"), [job_id], from_row)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("job {job_id}")))
}

fn list(app: &App, sql_where: &str, limit: i64) -> Result<Vec<JobInfo>> {
    let mut stmt = app.db.prepare(&format!("SELECT {COLS} FROM transcription_jobs WHERE {sql_where} LIMIT ?1"))?;
    Ok(stmt.query_map([limit], from_row)?.collect::<rusqlite::Result<_>>()?)
}

/// `recent` = quantas tarefas terminadas incluir (a UI usa 20).
pub fn status(app: &App, gate: &Gate, recent: usize) -> Result<QueueStatus> {
    let mut jobs = list(app, "state = 'running' ORDER BY id", -1)?;
    let running = jobs.len() as i64;
    jobs.extend(list(app, "state = 'queued' ORDER BY id", -1)?);
    let blocker = gate.blocker();
    // cada enfileirada: o motivo da fila ou, se ela anda, `behind` quando há alguém na frente
    for (i, j) in jobs.iter_mut().skip(running as usize).enumerate() {
        let ahead = running + i as i64;
        j.ahead = Some(ahead);
        j.blocked_by = blocker.or((ahead > 0).then_some(BlockedBy::Behind));
    }
    jobs.extend(list(app, "state IN ('done', 'failed', 'cancelled') ORDER BY id DESC", recent as i64)?);
    Ok(QueueStatus { paused: gate.paused, blocked_by: blocker, jobs })
}

/// A próxima `queued` (menor id), sem alterar nada.
pub fn next_queued(app: &App) -> Result<Option<JobInfo>> {
    Ok(list(app, "state = 'queued' ORDER BY id", 1)?.into_iter().next())
}

/// A tarefa em execução (se houver; no máximo uma).
pub fn running(app: &App) -> Result<Option<JobInfo>> {
    Ok(list(app, "state = 'running' ORDER BY id", 1)?.into_iter().next())
}

/// `queued → running` (`attempts += 1`, `started_at`, `stage = preparing`). Se a chamada ainda NÃO tem versão,
/// também `calls.transcription_state = running` (chamada com versão fica `done`).
pub fn mark_running(app: &App, job_id: i64) -> Result<JobInfo> {
    let n = app.db.execute(
        "UPDATE transcription_jobs SET state = 'running', attempts = attempts + 1, started_at = ?2, finished_at = NULL,
                stage = 'preparing', progress = NULL, error_code = NULL, error_detail = NULL
         WHERE id = ?1 AND state = 'queued'",
        params![job_id, db::now()],
    )?;
    let job = get(app, job_id)?;
    if n == 0 {
        return Err(Error::Conflict(format!("job {job_id} is {}", enum_str(&job.state))));
    }
    set_call_state(app, &job, "running", None)?;
    Ok(job)
}

/// Grava etapa/fração (chamado por `runner` a cada progresso; não mexe em `state`).
pub fn set_progress(app: &App, job_id: i64, stage: &str, progress: Option<f64>) -> Result<()> {
    app.db.execute("UPDATE transcription_jobs SET stage = ?2, progress = ?3 WHERE id = ?1 AND state = 'running'", params![job_id, stage, progress])?;
    Ok(())
}

fn finish(app: &App, job_id: i64, state: &str, code: Option<&str>, detail: Option<&str>) -> Result<JobInfo> {
    let n = app.db.execute(
        "UPDATE transcription_jobs SET state = ?2, finished_at = ?3, error_code = ?4, error_detail = ?5, stage = NULL, progress = NULL
         WHERE id = ?1 AND state IN ('queued', 'running')",
        params![job_id, state, db::now(), code, detail],
    )?;
    let job = get(app, job_id)?;
    if n == 0 {
        return Err(Error::Conflict(format!("job {job_id} is already {}", enum_str(&job.state))));
    }
    Ok(job)
}

/// `running → done`; `calls.transcription_state = done` (já é `done` se a chamada tinha versão).
pub fn mark_done(app: &App, job_id: i64) -> Result<()> {
    let job = finish(app, job_id, "done", None, None)?;
    if let Some(lib) = open_available(app, job.library_id)? {
        lib.conn.execute("UPDATE calls SET transcription_state = 'done', transcription_error = NULL WHERE id = ?1", [job.call_id])?;
    }
    Ok(())
}

/// `running → failed` com código/detalhe. Só se a chamada NÃO tem versão: `calls.transcription_state = failed` +
/// `transcription_error` (chamada com versão fica `done`; o erro aparece só na fila).
/// (O bruto fica: `retry` retoma do que já foi gravado.)
pub fn mark_failed(app: &App, job_id: i64, code: &str, detail: &str) -> Result<()> {
    let detail: String = detail.chars().take(2000).collect();
    let job = finish(app, job_id, "failed", Some(code), Some(&detail))?;
    let msg: String = format!("{code}: {detail}").chars().take(500).collect();
    set_call_state(app, &job, "failed", Some(&msg))
}

/// `running → queued` (pausa por gravação, ou fechamento do app): sem `attempts` extra (a tentativa que a
/// pausa gastou é devolvida), o bruto fica.
pub fn requeue(app: &App, job_id: i64) -> Result<()> {
    requeue_with(app, job_id, true)
}

/// `give_back = false`: a tentativa conta (queda do worker, repetida pelo `run_next`).
pub(crate) fn requeue_with(app: &App, job_id: i64, give_back: bool) -> Result<()> {
    let n = app.db.execute(
        "UPDATE transcription_jobs SET state = 'queued', stage = NULL, progress = NULL,
                attempts = CASE WHEN ?2 THEN max(attempts - 1, 0) ELSE attempts END
         WHERE id = ?1 AND state = 'running'",
        params![job_id, give_back],
    )?;
    let job = get(app, job_id)?;
    if n == 0 {
        return Err(Error::Conflict(format!("job {job_id} is {}", enum_str(&job.state))));
    }
    set_call_state(app, &job, "pending", None)
}

/// Cancela por pedido do usuário: `queued|running → cancelled`; apaga o bruto parcial (`staging::purge`);
/// a chamada volta ao estado anterior (`pending` se não tem versão; `done` se tem).
pub fn mark_cancelled(app: &App, job_id: i64) -> Result<()> {
    let job = finish(app, job_id, "cancelled", None, None)?;
    if let Some(mut lib) = open_available(app, job.library_id)? {
        match staging::purge(&mut lib, job_id) {
            Ok(()) | Err(Error::Conflict(_)) => {} // conflict: a versão já nasceu, o bruto é dela
            Err(e) => return Err(e),
        }
    }
    set_call_state(app, &job, "pending", None)
}

/// `failed` → volta a `queued` NA MESMA linha (preserva o bruto parcial); `cancelled` → cria uma tarefa nova
/// (o bruto foi apagado; copia opções e `base_job_id`). Devolve a tarefa resultante.
pub fn retry(app: &App, job_id: i64) -> Result<JobInfo> {
    let job = get(app, job_id)?;
    // o áudio pode ter sido apagado depois da falha/cancelamento: só `resegment` dispensa áudio
    if job.kind != JobKind::Resegment
        && let Some(lib) = open_available(app, job.library_id)?
        && lib.conn.query_row("SELECT audio_deleted_at IS NOT NULL FROM calls WHERE id = ?1", [job.call_id], |r| r.get::<_, bool>(0)).optional()?.unwrap_or(false)
    {
        return Err(Error::transcription("audio_deleted", format!("call {}", job.call_key)));
    }
    match job.state {
        JobState::Failed => {
            let open: bool = app.db.query_row(
                "SELECT EXISTS (SELECT 1 FROM transcription_jobs WHERE library_id = ?1 AND call_id = ?2 AND state IN ('queued', 'running'))",
                params![job.library_id, job.call_id],
                |r| r.get(0),
            )?;
            if open {
                return Err(Error::Conflict(format!("call {} already has an open transcription job", job.call_key)));
            }
            app.db.execute(
                "UPDATE transcription_jobs SET state = 'queued', error_code = NULL, error_detail = NULL, finished_at = NULL,
                        attempts = 0, stage = NULL, progress = NULL WHERE id = ?1 AND state = 'failed'",
                [job_id],
            )?;
            set_call_state(app, &job, "pending", None)?;
            get(app, job_id)
        }
        JobState::Cancelled => {
            let id = insert_job(app, job.library_id, job.call_id, &job.call_key, job.kind, &job.options, job.base_job_id)?;
            set_call_state(app, &job, "pending", None)?;
            get(app, id)
        }
        _ => Err(Error::Conflict(format!("job {job_id} is {}", enum_str(&job.state)))),
    }
}

/// Remove as terminadas (`done|failed|cancelled`) mais antigas que as `keep` últimas. Devolve quantas.
pub fn prune_finished(app: &App, keep: usize) -> Result<usize> {
    Ok(app.db.execute(
        "DELETE FROM transcription_jobs WHERE state IN ('done', 'failed', 'cancelled')
           AND id NOT IN (SELECT id FROM transcription_jobs WHERE state IN ('done', 'failed', 'cancelled') ORDER BY id DESC LIMIT ?1)",
        [keep as i64],
    )?)
}

/// Na abertura da GUI: toda `running` é resto de uma queda. Se a biblioteca já tem a versão
/// (`transcripts.raw_job_id = job_id`) → `done`; senão → `queued` (retoma do bruto gravado; depois de
/// `MAX_ATTEMPTS` quedas seguidas vira `failed`). Devolve quantas.
pub fn recover_after_crash(app: &App) -> Result<usize> {
    let jobs = list(app, "state = 'running' ORDER BY id", -1)?;
    for job in &jobs {
        let committed = match open_available(app, job.library_id)? {
            Some(lib) => lib
                .conn
                .query_row("SELECT EXISTS (SELECT 1 FROM transcripts WHERE raw_job_id = ?1)", [job.id], |r| r.get::<_, bool>(0))?,
            None => false,
        };
        if committed {
            mark_done(app, job.id)?;
        } else if job.attempts >= MAX_ATTEMPTS {
            mark_failed(app, job.id, "job_failed", "interrupted repeatedly")?;
        } else {
            requeue_with(app, job.id, false)?;
        }
    }
    Ok(jobs.len())
}
