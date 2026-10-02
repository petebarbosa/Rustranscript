//! Uma tarefa de ponta a ponta. Síncrono (roda na thread da fila do shell); fala com um `Engine`.
use std::path::PathBuf;

use rusqlite::OptionalExtension;
use serde::Serialize;

use super::assemble::{self, AssembleInput, PrevBlock};
use super::commit;
use super::engine::{Engine, Flow, Terminal};
use super::models;
use super::params::Params;
use super::protocol::{FromWorker, ToWorker};
use super::queue::{self, JobInfo, JobKind, MAX_ATTEMPTS};
use super::staging::{self, Energy, Segment, StageKey, Track, Turn};
use crate::library::Library;
use crate::{App, Error, Result, rules};

/// Passo do envelope de energia pedido ao worker (ms).
pub const ENERGY_STEP_MS: u32 = 100;

/// Etapas (e valores de `JobInfo.stage`, em snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Preparing,
    LoadingModel,
    AsrSys,
    AsrMic,
    Energy,
    Diarize,
    Assemble,
    Commit,
}

/// Evento `queue-progress` (e callback do runner). `fraction` = da etapa corrente; `None` = indeterminado
/// (carga do modelo, 1ª fase da diarização).
#[derive(Debug, Clone, Serialize)]
pub struct JobProgress {
    pub job_id: i64,
    pub library_id: i64,
    pub call_id: i64,
    pub stage: Stage,
    pub fraction: Option<f64>,
    /// ASR: segundos de áudio já transcritos / total.
    pub audio_s: Option<f64>,
    pub total_s: Option<f64>,
}

/// Por que o chamador quer parar a tarefa agora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// gravação começou: volta para `queued` e segue depois
    Pause,
    /// o app vai fechar: idem `Pause` (a fila retoma na próxima abertura)
    Shutdown,
    /// o usuário cancelou: `cancelled` + bruto apagado
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunEnd {
    Done { transcript_id: i64 },
    /// parou por `CancelReason` (o bruto parcial ficou gravado)
    Stopped(CancelReason),
}

/// Executa a tarefa. Pipeline `full`: [por etapa não concluída em `tx_stage`] ASR sys (retoma de
/// `resume_point`) → ASR mic → energia sys/mic → diarização do sys → `assemble` → `commit_version`.
/// `rediarize`: `clone_raw` do bruto-base (sem turnos) → diarização → assemble → commit.
/// `resegment`: `clone_raw` (com turnos) → assemble → commit (sem engine).
/// `cancel()` é consultada a cada evento do worker e entre etapas; ao mudar para `Some`, devolve `Flow::Cancel`,
/// espera o `cancelled` (gravando os segmentos que ainda chegarem) e retorna `RunEnd::Stopped`.
/// Erros fatais viram `Err` (quem chama decide `failed`/repetir).
pub fn run_job(
    app: &App,
    job: &JobInfo,
    engine: &mut dyn Engine,
    cancel: &dyn Fn() -> Option<CancelReason>,
    on_progress: &mut dyn FnMut(&JobProgress),
) -> Result<RunEnd> {
    let lib = queue::open_available(app, job.library_id)?
        .ok_or_else(|| Error::not_found(format!("library {} is unavailable", job.library_id)))?;
    // idempotência: a versão deste bruto já nasceu (queda entre o commit e o `mark_done`)
    if let Some(transcript_id) = committed_version(&lib, job.id)? {
        return Ok(RunEnd::Done { transcript_id });
    }
    let mut run = Run { app, job, lib, cancel, on_progress, reason: None };
    run.emit(Stage::Preparing, None, None, None);
    run.pipeline(engine)
}

fn committed_version(lib: &Library, job_id: i64) -> Result<Option<i64>> {
    Ok(lib.conn.query_row("SELECT id FROM transcripts WHERE raw_job_id = ?1", [job_id], |r| r.get(0)).optional()?)
}

/// Dados da chamada lidos uma vez no início.
struct CallData {
    client_id: Option<i64>,
    language: Option<String>,
    expected_speakers: Option<i64>,
    sys: Option<PathBuf>,
    mic: Option<PathBuf>,
    offset_s: f64,
}

struct Run<'a> {
    app: &'a App,
    job: &'a JobInfo,
    lib: Library,
    cancel: &'a dyn Fn() -> Option<CancelReason>,
    on_progress: &'a mut dyn FnMut(&JobProgress),
    /// primeiro motivo de parada observado (permanece)
    reason: Option<CancelReason>,
}

impl Run<'_> {
    fn poll(&mut self) -> Flow {
        if self.reason.is_none() {
            self.reason = (self.cancel)();
        }
        if self.reason.is_some() { Flow::Cancel } else { Flow::Continue }
    }

    fn emit(&mut self, stage: Stage, fraction: Option<f64>, audio_s: Option<f64>, total_s: Option<f64>) {
        // o progresso é cosmético: falha ao gravar não derruba a tarefa
        let _ = queue::set_progress(self.app, self.job.id, &queue::enum_str(&stage), fraction);
        (self.on_progress)(&JobProgress {
            job_id: self.job.id,
            library_id: self.job.library_id,
            call_id: self.job.call_id,
            stage,
            fraction,
            audio_s,
            total_s,
        });
    }

    fn load_call(&self) -> Result<CallData> {
        let (client_id, language, expected, mic, sys, dir): (Option<i64>, Option<String>, Option<i64>, Option<String>, Option<String>, Option<String>) =
            self.lib
                .conn
                .query_row(
                    "SELECT client_id, language, expected_speakers, mic_path, sys_path, dir FROM calls WHERE id = ?1",
                    [self.job.call_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?
                .ok_or_else(|| Error::not_found(format!("call {}", self.job.call_key)))?;
        let audio_deleted: bool =
            self.lib.conn.query_row("SELECT audio_deleted_at IS NOT NULL FROM calls WHERE id = ?1", [self.job.call_id], |r| r.get(0))?;
        let existing = |p: Option<String>| p.map(|p| self.lib.audio_abs(&p)).filter(|p| !audio_deleted && p.is_file());
        let offset_s = dir
            .and_then(|d| recorder::Sidecar::read(&self.lib.root().join(d)).ok())
            .and_then(|s| s.mic_offset_ms())
            .map_or(0.0, |ms| ms as f64 / 1000.0);
        Ok(CallData { client_id, language, expected_speakers: expected, sys: existing(sys), mic: existing(mic), offset_s })
    }

    /// Roda um pedido: segmentos vão ao banco ANTES de a próxima mensagem ser lida (e mesmo depois de
    /// pedirmos cancelamento: o worker termina a janela corrente). `None` = parou por cancelamento.
    fn call(&mut self, engine: &mut dyn Engine, req: &ToWorker, track: Option<Track>, asr_stage: Stage) -> Result<Option<FromWorker>> {
        let mut failure: Option<Error> = None;
        let job_id = self.job.id;
        let term = engine.execute(req, &mut |msg| {
            if failure.is_none() {
                match msg {
                    FromWorker::Segment { start, end, text, words, .. } => {
                        if let (Some(track), false) = (track, text.trim().is_empty()) {
                            let seg = Segment { start: *start, end: *end, text: text.clone(), words: words.clone().unwrap_or_default() };
                            if let Err(e) = staging::push_segment(&mut self.lib, job_id, track, &seg) {
                                failure = Some(e);
                            }
                        }
                    }
                    FromWorker::Progress { stage, audio_s, total_s, done, total, .. } => {
                        let frac = |a: Option<f64>, b: Option<f64>| match (a, b) {
                            (Some(a), Some(b)) if b > 0.0 => Some((a / b).clamp(0.0, 1.0)),
                            _ => None,
                        };
                        match stage.as_str() {
                            "loading_model" => self.emit(Stage::LoadingModel, None, None, None),
                            "transcribe" => self.emit(asr_stage, frac(*audio_s, *total_s), *audio_s, *total_s),
                            "diarize_segmentation" => {
                                let f = frac(done.map(|d| d as f64), total.map(|t| t as f64)).filter(|f| *f < 1.0);
                                self.emit(Stage::Diarize, f, None, None)
                            }
                            "diarize_embedding" => self.emit(Stage::Diarize, None, None, None),
                            "energy" => self.emit(Stage::Energy, frac(done.map(|d| d as f64), total.map(|t| t as f64)), None, None),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            if failure.is_some() { Flow::Cancel } else { self.poll() }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        match term? {
            Terminal::Result(r) => Ok(Some(r)),
            Terminal::Cancelled { .. } if self.reason.is_some() => Ok(None),
            Terminal::Cancelled { .. } => Err(Error::transcription("worker_protocol", "worker cancelled a request nobody cancelled")),
        }
    }

    fn pipeline(&mut self, engine: &mut dyn Engine) -> Result<RunEnd> {
        let job = self.job;
        macro_rules! stop {
            () => {
                if self.poll() == Flow::Cancel {
                    return Ok(RunEnd::Stopped(self.reason.expect("set by poll")));
                }
            };
        }
        stop!();
        let call = self.load_call()?;
        let mut params = Params::from_settings(self.app, &job.options)?;
        if job.options.language.is_none()
            && let Some(l) = call.language.as_deref().and_then(crate::recording::language_code)
        {
            params.language = l.to_string();
        }
        let both = call.sys.is_some() && call.mic.is_some();

        // bruto de partida (rediarize/resegment)
        if job.kind != JobKind::Full && !staging::has_raw(&self.lib, job.id)? {
            let base = job.base_job_id.ok_or_else(|| Error::transcription("no_raw_data", "job without base raw data"))?;
            staging::clone_raw(&mut self.lib, base, job.id, job.kind == JobKind::Resegment)?;
        }

        if job.kind != JobKind::Resegment {
            if call.sys.is_none() && call.mic.is_none() {
                return Err(Error::transcription("no_audio", format!("call {} has no audio", job.call_key)));
            }
            // worker falso (testes/app de teste): não precisa dos modelos instalados
            let paths = match models::model_paths(&self.app.data_dir) {
                Err(_) if std::env::var(super::FAKE_WORKER_ENV).is_ok_and(|v| !v.trim().is_empty()) => models::model_paths_unchecked(&self.app.data_dir),
                r => r?,
            };
            let hotwords = if params.hotwords {
                let terms = self.app.prompt_terms(&self.lib, call.client_id)?;
                (!terms.is_empty()).then(|| terms.join(", "))
            } else {
                None
            };
            let language = (params.language != "auto").then(|| params.language.clone());
            let key = |s: &str| format!("j{}-{s}", job.id);
            let path = |p: &PathBuf| p.to_string_lossy().into_owned();

            if job.kind == JobKind::Full {
                for (track, audio, stage_key, stage) in [
                    (Track::Sys, &call.sys, StageKey::AsrSys, Stage::AsrSys),
                    (Track::Mic, &call.mic, StageKey::AsrMic, Stage::AsrMic),
                ] {
                    let Some(audio) = audio else { continue };
                    if staging::stage_done(&self.lib, job.id, stage_key)? {
                        continue;
                    }
                    stop!();
                    let req = ToWorker::Transcribe {
                        id: key(track.as_str()),
                        audio: path(audio),
                        track: track.as_str().into(),
                        model_dir: path(&paths.whisper_dir),
                        language: language.clone(),
                        hotwords: hotwords.clone(),
                        beam_size: params.beam_size,
                        threads: params.threads,
                        word_timestamps: track == Track::Sys,
                        vad_min_silence_ms: params.vad_min_silence_ms,
                        start_s: staging::resume_point(&self.lib, job.id, track)?,
                    };
                    let Some(res) = self.call(engine, &req, Some(track), stage)? else { continue };
                    let FromWorker::Result { segments, seconds, language, .. } = res else {
                        return Err(Error::transcription("worker_protocol", "transcribe without result"));
                    };
                    let info = serde_json::json!({ "language": language, "segments": segments, "seconds": seconds });
                    staging::mark_stage_done(&mut self.lib, job.id, stage_key, Some(&info))?;
                }
                stop!();
            }

            if params.bleed.enabled && both && !staging::stage_done(&self.lib, job.id, StageKey::Energy)? {
                for (track, audio) in [(Track::Sys, &call.sys), (Track::Mic, &call.mic)] {
                    let Some(audio) = audio else { continue };
                    stop!();
                    let req = ToWorker::Energy { id: key(&format!("energy-{}", track.as_str())), audio: path(audio), step_ms: ENERGY_STEP_MS };
                    let Some(res) = self.call(engine, &req, None, Stage::Energy)? else { continue };
                    let FromWorker::Result { step_ms, db: Some(db), .. } = res else {
                        return Err(Error::transcription("worker_protocol", "energy without result"));
                    };
                    staging::set_energy(&mut self.lib, job.id, track, &Energy { step_ms: step_ms.unwrap_or(ENERGY_STEP_MS), db })?;
                }
                stop!();
                staging::mark_stage_done(&mut self.lib, job.id, StageKey::Energy, None)?;
            }

            if let Some(sys) = &call.sys
                && !staging::stage_done(&self.lib, job.id, StageKey::Diarize)?
            {
                stop!();
                let num_clusters = job.options.expected_speakers.or(call.expected_speakers).filter(|n| *n >= 1).map(|n| n as u32);
                let req = ToWorker::Diarize {
                    id: key("diarize"),
                    audio: path(sys),
                    seg_model: path(&paths.seg_model),
                    emb_model: path(&paths.emb_model),
                    num_clusters,
                    threshold: params.diarization_threshold,
                    threads: params.threads,
                };
                if let Some(res) = self.call(engine, &req, None, Stage::Diarize)? {
                    let FromWorker::Result { turns: Some(turns), speakers, .. } = res else {
                        return Err(Error::transcription("worker_protocol", "diarize without turns"));
                    };
                    let turns: Vec<Turn> = turns.iter().map(|t| Turn { start: t.start, end: t.end, cluster: t.speaker }).collect();
                    staging::set_turns(&mut self.lib, job.id, &turns)?;
                    staging::mark_stage_done(&mut self.lib, job.id, StageKey::Diarize, Some(&serde_json::json!({ "speakers": speakers })))?;
                }
                stop!();
            }
        }
        stop!();

        // montagem + versão
        self.emit(Stage::Assemble, None, None, None);
        let (sys, mic) = (staging::segments(&self.lib, job.id, Track::Sys)?, staging::segments(&self.lib, job.id, Track::Mic)?);
        let turns = staging::turns(&self.lib, job.id)?;
        let (sys_energy, mic_energy) = (staging::energy(&self.lib, job.id, Track::Sys)?, staging::energy(&self.lib, job.id, Track::Mic)?);
        let previous = self.previous_blocks()?;
        let assembled = assemble::assemble(&AssembleInput {
            sys: &sys,
            mic: &mic,
            turns: &turns,
            sys_energy: sys_energy.as_ref(),
            mic_energy: mic_energy.as_ref(),
            mic_offset_s: call.offset_s,
            params: &params,
            previous: &previous,
        })?;
        let detected = [StageKey::AsrSys, StageKey::AsrMic]
            .into_iter()
            .find_map(|k| staging::stage_info(&self.lib, job.id, k).ok().flatten())
            .and_then(|v| v["language"].as_str().map(str::to_string));
        let params_json = serde_json::json!({
            "kind": job.kind,
            "language": params.language,
            "detected_language": detected,
            "hotwords": params.hotwords,
            "beam_size": params.beam_size,
            "vad_min_silence_ms": params.vad_min_silence_ms,
            "mic_offset_s": call.offset_s,
            "clusters": assembled.clusters,
            "diarization": {
                "threshold": params.diarization_threshold,
                "min_cluster_pct": params.min_cluster_pct,
                "min_cluster_s": params.min_cluster_s,
                "expected_speakers": job.options.expected_speakers.or(call.expected_speakers),
            },
            "bleed": params.bleed,
        });
        stop!();
        self.emit(Stage::Commit, None, None, None);
        let glossary = rules::replace_rules(&self.app.effective_rules(&self.lib, call.client_id)?);
        let committed = commit::commit_version_with(&mut self.lib, job, &assembled, &params_json, &glossary)?;
        Ok(RunEnd::Done { transcript_id: committed.transcript_id })
    }

    /// Blocos da versão ativa (para preservar rótulos e nomes na troca de versão).
    fn previous_blocks(&self) -> Result<Vec<PrevBlock>> {
        let mut stmt = self.lib.conn.prepare(
            "SELECT s.label, b.t_start, b.t_end FROM blocks b
             JOIN transcripts t ON t.id = b.transcript_id AND t.is_active = 1 AND t.call_id = ?1
             JOIN speakers s ON s.id = b.speaker_id ORDER BY b.seq",
        )?;
        let rows = stmt.query_map([self.job.call_id], |r| Ok(PrevBlock { label: r.get(0)?, t_start: r.get(1)?, t_end: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Passo da fila: `recover`/`next_queued` → `mark_running` → `run_job` → desfecho no banco:
/// `Done` → `mark_done`; `Stopped(Pause|Shutdown)` → `requeue`; `Stopped(Cancel)` → `mark_cancelled`;
/// `Err` com `worker_crashed` e `attempts < 3` → `requeue`; demais `Err` → `mark_failed`.
/// (`job_failed` = `exception` do worker: uma repetição.) `Ok(None)` = fila vazia.
pub fn run_next(
    app: &App,
    engine: &mut dyn Engine,
    cancel: &dyn Fn(&JobInfo) -> Option<CancelReason>,
    on_progress: &mut dyn FnMut(&JobProgress),
) -> Result<Option<(JobInfo, Result<RunEnd>)>> {
    let Some(next) = queue::next_queued(app)? else { return Ok(None) };
    let job = queue::mark_running(app, next.id)?;
    let res = run_job(app, &job, engine, &|| cancel(&job), on_progress);
    match &res {
        Ok(RunEnd::Done { .. }) => queue::mark_done(app, job.id)?,
        Ok(RunEnd::Stopped(CancelReason::Pause | CancelReason::Shutdown)) => queue::requeue(app, job.id)?,
        Ok(RunEnd::Stopped(CancelReason::Cancel)) => queue::mark_cancelled(app, job.id)?,
        Err(e) => {
            let retry = match e.code() {
                "worker_crashed" => job.attempts < MAX_ATTEMPTS,
                "job_failed" => job.attempts < 2,
                _ => false,
            };
            if retry {
                queue::requeue_with(app, job.id, false)?;
            } else {
                queue::mark_failed(app, job.id, e.code(), &e.detail())?;
            }
        }
    }
    Ok(Some((queue::get(app, job.id)?, res)))
}
