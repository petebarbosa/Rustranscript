//! Pipeline de transcrição de ponta a ponta com o `FakeEngine` (sem processo, sem modelos reais, áudio sintético
//! gerado na hora): versão completa, queda e retomada, pausa/cancelamento, idempotência do commit,
//! rediarize/resegment, filtro de vazamento, deslocamento mic×sys, glossário, fila e mover a chamada.
use std::cell::Cell;
use std::sync::OnceLock;

use core_lib::transcription::assemble::Assembled;
use core_lib::transcription::commit;
use core_lib::transcription::engine::{Engine, FakeEngine, Flow, Terminal};
use core_lib::transcription::models;
use core_lib::transcription::params::JobOptions;
use core_lib::transcription::protocol::{FromWorker, ToWorker};
use core_lib::transcription::queue::{self, BlockedBy, Gate, JobKind, JobState, PauseReason};
use core_lib::transcription::runner::{self, CancelReason, RunEnd, Stage};
use core_lib::transcription::staging::{self, Track};
use core_lib::{App, Error, Library, Result};

// ------------------------------------------------------------------ ambiente

struct Env {
    _dir: tempfile::TempDir,
    app: App,
    lib_id: i64,
}

fn silent_flac(secs: u32) -> &'static [u8] {
    static CACHE: OnceLock<std::collections::HashMap<u32, &'static [u8]>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        [20u32, 60, 100]
            .into_iter()
            .map(|s| {
                let dir = tempfile::tempdir().unwrap();
                let wav = dir.path().join("a.wav");
                let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
                let mut w = hound::WavWriter::create(&wav, spec).unwrap();
                for i in 0..16_000 * s {
                    w.write_sample(((i % 50) as i16) - 25).unwrap();
                }
                w.finalize().unwrap();
                let flac = dir.path().join("a.flac");
                core_lib::audio::wav_to_flac(&wav, &flac, &mut |_, _| {}).unwrap();
                (s, &*Box::leak(std::fs::read(flac).unwrap().into_boxed_slice()))
            })
            .collect()
    });
    map[&secs]
}

fn install_models(app: &App) {
    let src = tempfile::tempdir().unwrap();
    let wh = src.path().join("whisper");
    std::fs::create_dir_all(&wh).unwrap();
    for f in models::WHISPER.files {
        std::fs::write(wh.join(f.dest), b"x").unwrap();
    }
    std::fs::write(src.path().join("seg.onnx"), b"x").unwrap();
    std::fs::write(src.path().join("emb.onnx"), b"x").unwrap();
    models::import_local(&app.data_dir, "whisper", &wh).unwrap();
    models::import_local(&app.data_dir, "segmentation", &src.path().join("seg.onnx")).unwrap();
    models::import_local(&app.data_dir, "embedding", &src.path().join("emb.onnx")).unwrap();
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).unwrap();
    install_models(&app);
    let lib_id = app.inbox_id().unwrap();
    Env { _dir: dir, app, lib_id }
}

impl Env {
    fn lib(&self) -> Library {
        self.app.open_library(self.lib_id).unwrap()
    }

    /// Chamada nova (`pending`) com áudio sintético de `secs` segundos em cada trilha pedida.
    fn call(&self, key: &str, mic: Option<u32>, sys: Option<u32>) -> i64 {
        let lib = self.lib();
        let root = lib.root().join(key);
        std::fs::create_dir_all(&root).unwrap();
        let mut paths = [None, None];
        for (i, (name, secs)) in [("mic.flac", mic), ("sys.flac", sys)].into_iter().enumerate() {
            if let Some(s) = secs {
                std::fs::write(root.join(name), silent_flac(s)).unwrap();
                paths[i] = Some(format!("{key}/{name}"));
            }
        }
        lib.conn
            .execute(
                "INSERT INTO calls (key, started_at, created_at, language, dir, mic_path, sys_path, transcription_state)
                 VALUES (?1, '2026-01-01T00:00:00', '2026-01-01T00:00:00', 'pt', ?1, ?2, ?3, 'pending')",
                rusqlite::params![key, paths[0], paths[1]],
            )
            .unwrap();
        lib.conn.last_insert_rowid()
    }

    fn enqueue(&self, call_id: i64, kind: JobKind, o: JobOptions) -> queue::JobInfo {
        queue::enqueue(&self.app, self.lib_id, call_id, kind, &o).unwrap()
    }

    fn run(&self, engine: &mut dyn Engine) -> (queue::JobInfo, Result<RunEnd>) {
        runner::run_next(&self.app, engine, &|_| None, &mut |_| {}).unwrap().expect("a queued job")
    }
}

type Row = (String, f64, f64, String);

fn blocks_of(lib: &Library, transcript_id: i64) -> Vec<Row> {
    let mut st = lib
        .conn
        .prepare("SELECT s.label, b.t_start, b.t_end, b.text FROM blocks b JOIN speakers s ON s.id = b.speaker_id WHERE b.transcript_id = ?1 ORDER BY b.seq")
        .unwrap();
    st.query_map([transcript_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(|r| r.unwrap()).collect()
}

fn versions(lib: &Library, call_id: i64) -> Vec<(i64, bool, Option<i64>)> {
    let mut st = lib.conn.prepare("SELECT id, is_active, raw_job_id FROM transcripts WHERE call_id = ?1 ORDER BY version").unwrap();
    st.query_map([call_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
}

fn done_id(r: &Result<RunEnd>) -> i64 {
    match r {
        Ok(RunEnd::Done { transcript_id }) => *transcript_id,
        other => panic!("expected Done, got {other:?}"),
    }
}

fn assert_contiguous(lib: &Library, job_id: i64, track: Track, expected_starts: &[f64]) {
    let segs = staging::segments(lib, job_id, track).unwrap();
    assert_eq!(segs.iter().map(|s| s.start).collect::<Vec<_>>(), expected_starts, "{track:?}: gaps or duplicates");
    let seqs: Vec<i64> = lib
        .conn
        .prepare("SELECT seq FROM tx_segments WHERE job_id = ?1 AND track = ?2 ORDER BY seq")
        .unwrap()
        .query_map(rusqlite::params![job_id, track.as_str()], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(seqs, (1..=expected_starts.len() as i64).collect::<Vec<_>>());
}

fn starts(n: usize) -> Vec<f64> {
    (0..n).map(|k| k as f64 * 5.0).collect()
}

// ------------------------------------------------------------------ motores de teste

/// Cai (`worker_crashed`) depois de `after` segmentos de uma requisição `transcribe`, `crashes` vezes.
struct CrashAfter {
    inner: FakeEngine,
    after: usize,
    crashes: usize,
}

impl Engine for CrashAfter {
    fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
        if self.crashes == 0 || !matches!(req, ToWorker::Transcribe { .. }) {
            return self.inner.execute(req, on_event);
        }
        if self.after == 0 {
            self.crashes -= 1;
            return Err(Error::transcription("worker_crashed", "simulated"));
        }
        let (mut n, mut crashed) = (0, false);
        let res = self.inner.execute(req, &mut |m| {
            let flow = if crashed { Flow::Cancel } else { on_event(m) };
            if matches!(m, FromWorker::Segment { .. }) {
                n += 1;
                crashed |= n >= self.after;
            }
            if crashed { Flow::Cancel } else { flow }
        });
        if crashed {
            self.crashes -= 1;
            return Err(Error::transcription("worker_crashed", "simulated"));
        }
        res
    }
    fn shutdown(&mut self) {}
}

/// Registra o tipo de cada requisição.
#[derive(Default)]
struct Spy {
    inner: FakeEngine,
    kinds: Vec<&'static str>,
}

impl Engine for Spy {
    fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
        self.kinds.push(match req {
            ToWorker::Transcribe { .. } => "transcribe",
            ToWorker::Diarize { .. } => "diarize",
            ToWorker::Energy { .. } => "energy",
            _ => "other",
        });
        self.inner.execute(req, on_event)
    }
    fn shutdown(&mut self) {}
}

/// Mic silencioso (-45 dB): é o que o vazamento puro do alto-falante parece.
struct QuietMic(FakeEngine);

impl Engine for QuietMic {
    fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
        let quiet = matches!(req, ToWorker::Energy { audio, .. } if audio.contains("mic"));
        let res = self.0.execute(req, on_event)?;
        Ok(match res {
            Terminal::Result(FromWorker::Result { id, seconds, step_ms, db: Some(db), .. }) if quiet => Terminal::Result(FromWorker::Result {
                id,
                segments: None,
                seconds,
                language: None,
                turns: None,
                speakers: None,
                step_ms,
                db: Some(vec![-45.0; db.len()]),
                merge: None,
            }),
            other => other,
        })
    }
    fn shutdown(&mut self) {}
}

// ------------------------------------------------------------------ testes

/// Issue #8: cada trilha mostra progresso ANTES de o 1º trecho (janela) terminar, nunca recua e fecha em 100 %.
#[test]
fn asr_progress_starts_early_never_regresses_and_ends_at_100() {
    let e = env();
    let call = e.call("call_p", Some(60), Some(60));
    e.enqueue(call, JobKind::Full, JobOptions::default());
    let mut progress = Vec::new();
    runner::run_next(&e.app, &mut FakeEngine::new(), &|_| None, &mut |p| progress.push(p.clone())).unwrap().unwrap();
    for stage in [Stage::AsrSys, Stage::AsrMic] {
        let f: Vec<f64> = progress.iter().filter(|p| p.stage == stage).filter_map(|p| p.fraction).collect();
        // o 1º trecho só termina em 4,5 s de 60; o 1º progresso (2,25 s) chega antes dele
        assert_eq!(f.first().copied(), Some(2.25 / 60.0), "{stage:?}");
        assert!(f.windows(2).all(|w| w[0] <= w[1]), "{stage:?} recuou: {f:?}");
        assert_eq!(f.last().copied(), Some(1.0), "{stage:?}");
    }
}

/// Exemplo da API: chamada com duas trilhas → fila → versão pronta (blocos Eu / Pessoa N, bruto preservado).
#[test]
fn full_pipeline_creates_a_complete_version() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    assert_eq!((job.state, job.attempts), (JobState::Queued, 0));
    let mut progress = Vec::new();
    let (done, res) = runner::run_next(&e.app, &mut FakeEngine::new(), &|_| None, &mut |p| progress.push(p.clone())).unwrap().unwrap();
    let tid = done_id(&res);
    assert_eq!((done.state, done.attempts), (JobState::Done, 1));
    assert!(progress.iter().any(|p| p.fraction.is_some()) && progress.iter().any(|p| p.fraction.is_none()));

    let lib = e.lib();
    assert_eq!(versions(&lib, call), vec![(tid, true, Some(job.id))]);
    let blocks = blocks_of(&lib, tid);
    // 12 trechos por trilha, intercalados (sys antes do mic no mesmo instante), nenhum fundido
    assert_eq!(blocks.len(), 24);
    assert_eq!(blocks[0], ("Pessoa 1".into(), 0.0, 4.5, "fala trecho 0".into()));
    assert_eq!(blocks[1], ("Eu".into(), 0.0, 4.5, "eu trecho 0".into()));
    // turnos de 15 s alternando 0,1,0,1: trechos 3-5 são da outra pessoa
    assert_eq!(blocks[6].0, "Pessoa 2");
    assert_eq!(blocks[6].3, "fala trecho 3");
    let speakers = lib.speakers(call).unwrap();
    assert_eq!(speakers.iter().map(|s| (s.label.as_str(), s.track.as_str())).collect::<Vec<_>>(), vec![("Eu", "mic"), ("Pessoa 1", "sys"), ("Pessoa 2", "sys")]);
    let detail = lib.call_detail(call, None).unwrap();
    assert_eq!(detail.summary.transcription_state, "done");
    assert!(detail.transcripts[0].has_raw);
    assert_eq!(detail.blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), (1..=24).collect::<Vec<_>>());
    // nada de histórico de edição nem de "editado": é ASR, não edição do usuário
    assert_eq!(lib.history(Some(call), 10).unwrap().len(), 0);
    assert!(detail.blocks.iter().all(|b| !b.edited));
    assert_eq!(queue::get(&e.app, job.id).unwrap().state, JobState::Done);
    assert!(queue::next_queued(&e.app).unwrap().is_none());
    assert!(commit::bleed_removals(&lib, tid).unwrap().is_empty());
}

#[test]
fn bleed_filter_audit_and_switch() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    e.enqueue(call, JobKind::Full, JobOptions::default());
    let (_, res) = e.run(&mut QuietMic(FakeEngine::new()));
    let tid = done_id(&res);
    let lib = e.lib();
    let blocks = blocks_of(&lib, tid);
    assert!(blocks.iter().all(|b| b.0 != "Eu"), "mic 20 dB below sys with a short text: gone");
    let removed = commit::bleed_removals(&lib, tid).unwrap();
    assert_eq!(removed.len(), 12);
    assert!(removed.iter().all(|r| r.reason == "energy_short" && r.margin_db.unwrap() < -15.0));

    // "Sem o filtro de eco" = remontar sem refazer o ASR: os 12 trechos do Eu voltam
    let mut spy = Spy::default();
    e.enqueue(call, JobKind::Resegment, JobOptions { bleed_filter: Some(false), ..Default::default() });
    let (_, res) = e.run(&mut spy);
    assert!(spy.kinds.is_empty(), "resegment never touches the worker");
    let blocks = blocks_of(&lib, done_id(&res));
    assert_eq!(blocks.iter().filter(|b| b.0 == "Eu").count(), 12);
    assert!(commit::bleed_removals(&lib, done_id(&res)).unwrap().is_empty());
}

#[test]
fn bleed_filter_disabled_skips_the_energy_stage() {
    let e = env();
    let call = e.call("call_a", Some(20), Some(20));
    e.enqueue(call, JobKind::Full, JobOptions { bleed_filter: Some(false), ..Default::default() });
    let mut spy = Spy::default();
    let (job, res) = e.run(&mut spy);
    assert_eq!(spy.kinds, vec!["transcribe", "transcribe", "diarize"]);
    assert!(blocks_of(&e.lib(), done_id(&res)).iter().any(|b| b.0 == "Eu"));
    assert!(!staging::stage_done(&e.lib(), job.id, staging::StageKey::Energy).unwrap());
}

#[test]
fn mic_offset_from_the_sidecar_shifts_the_me_track() {
    let e = env();
    let call = e.call("call_a", Some(20), Some(20));
    let stream = |file: &str, first: i64| {
        serde_json::json!({"file": file, "device": "d", "description": "D", "is_monitor": false, "first_sample_unix_ms": first,
            "first_read_unix_ms": null, "latency_ms": null, "fragment_ms": 100, "samples": 0, "cuts": [], "reconnects": 0})
    };
    let sidecar = serde_json::json!({"schema": 1, "state": "complete", "key": "call_a", "app_version": "0", "started_at": "2026-01-01T00:00:00",
        "started_unix_ms": 0, "ended_at": null, "duration_s": null, "sample_rate": 16000, "channels": 1, "format": "s16le",
        "mic": stream("mic.wav", 3_000), "sys": stream("sys.wav", 1_000), "extra": {}});
    std::fs::write(e.lib().root().join("call_a/recording.json"), serde_json::to_vec(&sidecar).unwrap()).unwrap();
    e.enqueue(call, JobKind::Full, JobOptions::default());
    let (_, res) = e.run(&mut FakeEngine::new());
    let lib = e.lib();
    let me: Vec<f64> = blocks_of(&lib, done_id(&res)).iter().filter(|b| b.0 == "Eu").map(|b| b.1).collect();
    assert_eq!(me, vec![2.0, 7.0, 12.0, 17.0]); // mic começou 2 s depois do sys
}

#[test]
fn crash_mid_track_resumes_without_gaps_or_duplicates() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    // 1ª tentativa: cai depois de 4 trechos do sys
    let mut crashy = CrashAfter { inner: FakeEngine::new(), after: 4, crashes: 1 };
    let (j, res) = e.run(&mut crashy);
    assert_eq!(res.unwrap_err().code(), "worker_crashed");
    assert_eq!((j.state, j.attempts), (JobState::Queued, 1), "crash is retried by the queue");
    let lib = e.lib();
    assert_eq!(staging::segments(&lib, job.id, Track::Sys).unwrap().len(), 4);
    assert!(!staging::stage_done(&lib, job.id, staging::StageKey::AsrSys).unwrap());
    assert_eq!(staging::resume_point(&lib, job.id, Track::Sys).unwrap(), 19.5);
    // 2ª: o motor volta; retoma de 19,5 s
    let (j, res) = e.run(&mut crashy);
    let tid = done_id(&res);
    assert_eq!((j.state, j.attempts), (JobState::Done, 2));
    assert_contiguous(&lib, job.id, Track::Sys, &starts(12));
    assert_contiguous(&lib, job.id, Track::Mic, &starts(12));
    assert_eq!(blocks_of(&lib, tid).len(), 24);
    assert_eq!(versions(&lib, call).len(), 1);
    // o resultado é igual ao de uma execução sem queda
    let clean = env();
    let call2 = clean.call("call_a", Some(60), Some(60));
    clean.enqueue(call2, JobKind::Full, JobOptions::default());
    let (_, res2) = clean.run(&mut FakeEngine::new());
    assert_eq!(blocks_of(&lib, tid), blocks_of(&clean.lib(), done_id(&res2)));
}

#[test]
fn crash_in_the_second_track_keeps_the_first_and_three_crashes_fail() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    // cai sempre no começo: 3 tentativas e vira `failed`
    let mut dead = CrashAfter { inner: FakeEngine::new(), after: 0, crashes: 99 };
    for expected in [JobState::Queued, JobState::Queued, JobState::Failed] {
        let (j, res) = e.run(&mut dead);
        assert_eq!(res.unwrap_err().code(), "worker_crashed");
        assert_eq!(j.state, expected);
    }
    let failed = queue::get(&e.app, job.id).unwrap();
    assert_eq!((failed.attempts, failed.error_code.as_deref()), (3, Some("worker_crashed")));
    let detail = e.lib().call_detail(call, None).unwrap();
    assert_eq!(detail.summary.transcription_state, "failed");
    assert!(detail.summary.transcription_error.unwrap().starts_with("worker_crashed"));
    // tentar de novo: mesma linha, tentativas zeradas; o mic cai no meio na 1ª vez e o sys já estava pronto
    let again = queue::retry(&e.app, job.id).unwrap();
    assert_eq!((again.id, again.state, again.attempts), (job.id, JobState::Queued, 0));
    assert_eq!(e.lib().call_detail(call, None).unwrap().summary.transcription_state, "pending");
    let mut crashy = CrashAfter { inner: FakeEngine::new(), after: 3, crashes: 1 };
    // o sys cai (crashes = 1 vale para a 1ª requisição transcribe = sys)
    assert_eq!(e.run(&mut crashy).1.unwrap_err().code(), "worker_crashed");
    let (_, res) = e.run(&mut crashy);
    done_id(&res);
    let lib = e.lib();
    assert_contiguous(&lib, job.id, Track::Sys, &starts(12));
    assert_contiguous(&lib, job.id, Track::Mic, &starts(12));
}

/// Queda do PROCESSO inteiro: o runner some no meio (nenhum `mark_*` é chamado); na abertura `recover_after_crash`
/// devolve a tarefa à fila e a retomada termina sem duplicar nada.
#[test]
fn killed_app_mid_job_recovers_and_finishes() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    let running = queue::mark_running(&e.app, job.id).unwrap();
    let mut crashy = CrashAfter { inner: FakeEngine::new(), after: 7, crashes: 1 };
    let res = runner::run_job(&e.app, &running, &mut crashy, &|| None, &mut |_| {});
    assert_eq!(res.unwrap_err().code(), "worker_crashed");
    // ... e o app morreu aqui: a linha continua `running`
    assert_eq!(queue::get(&e.app, job.id).unwrap().state, JobState::Running);
    assert_eq!(queue::recover_after_crash(&e.app).unwrap(), 1);
    assert_eq!(queue::get(&e.app, job.id).unwrap().state, JobState::Queued);
    let (j, res) = e.run(&mut FakeEngine::new());
    let tid = done_id(&res);
    assert_eq!(j.state, JobState::Done);
    let lib = e.lib();
    assert_contiguous(&lib, job.id, Track::Sys, &starts(12));
    assert_eq!(blocks_of(&lib, tid).len(), 24);
    assert_eq!(versions(&lib, call).len(), 1);
}

/// A janela entre o commit (library.db) e o `mark_done` (app.db): a versão existe, a tarefa ainda diz `running`.
#[test]
fn crash_between_commit_and_mark_done_is_resolved_to_done() {
    let e = env();
    let call = e.call("call_a", Some(20), Some(20));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    let running = queue::mark_running(&e.app, job.id).unwrap();
    let tid = done_id(&runner::run_job(&e.app, &running, &mut FakeEngine::new(), &|| None, &mut |_| {}));
    // sem mark_done: queda
    assert_eq!(queue::recover_after_crash(&e.app).unwrap(), 1);
    let j = queue::get(&e.app, job.id).unwrap();
    assert_eq!(j.state, JobState::Done);
    assert_eq!(e.lib().call_detail(call, None).unwrap().summary.transcription_state, "done");
    assert_eq!(versions(&e.lib(), call).len(), 1);
    // repetir o run_job (tarefa reexecutada por engano) nem toca no motor: devolve a mesma versão
    let mut crashy = CrashAfter { inner: FakeEngine::new(), after: 0, crashes: 99 };
    assert_eq!(done_id(&runner::run_job(&e.app, &running, &mut crashy, &|| None, &mut |_| {})), tid);
    // e `commit_version` repetido não duplica
    let mut lib = e.lib();
    let n_blocks = blocks_of(&lib, tid).len();
    let c = commit::commit_version(&mut lib, &running, &Assembled { blocks: vec![], removals: vec![], clusters: 0 }, &serde_json::json!({})).unwrap();
    assert_eq!((c.transcript_id, c.blocks), (tid, n_blocks));
    assert_eq!(versions(&lib, call).len(), 1);
}

/// Pausa (gravação começou / fechar o app) em QUALQUER ponto: volta para a fila com o bruto, e a retomada produz
/// exatamente a versão de uma execução sem interrupção.
#[test]
fn pause_anywhere_requeues_and_resumes_to_the_same_result() {
    let baseline = {
        let e = env();
        let call = e.call("call_a", Some(60), Some(60));
        e.enqueue(call, JobKind::Full, JobOptions::default());
        let (_, res) = e.run(&mut FakeEngine::new());
        blocks_of(&e.lib(), done_id(&res))
    };
    let mut paused_at_least_once = 0;
    for k in (0..60).step_by(2) {
        let e = env();
        let call = e.call("call_a", Some(60), Some(60));
        let job = e.enqueue(call, JobKind::Full, JobOptions::default());
        let polls = Cell::new(0usize);
        let reason = if k % 4 == 0 { CancelReason::Pause } else { CancelReason::Shutdown };
        let cancel = |_: &queue::JobInfo| {
            polls.set(polls.get() + 1);
            (polls.get() == k + 1).then_some(reason)
        };
        let (j, res) = runner::run_next(&e.app, &mut FakeEngine::new(), &cancel, &mut |_| {}).unwrap().unwrap();
        match res {
            Ok(RunEnd::Stopped(r)) => {
                paused_at_least_once += 1;
                assert_eq!(r, reason);
                assert_eq!((j.state, j.attempts), (JobState::Queued, 0), "k={k}: pause returns the attempt");
                assert_eq!(e.lib().call_detail(call, None).unwrap().summary.transcription_state, "pending");
                assert!(e.lib().call_detail(call, None).unwrap().transcripts.is_empty());
                let (j2, res2) = e.run(&mut FakeEngine::new());
                assert_eq!(j2.state, JobState::Done);
                let tid = done_id(&res2);
                let lib = e.lib();
                assert_contiguous(&lib, job.id, Track::Sys, &starts(12));
                assert_contiguous(&lib, job.id, Track::Mic, &starts(12));
                assert_eq!(blocks_of(&lib, tid), baseline, "k={k}");
                assert_eq!(versions(&lib, call).len(), 1);
            }
            Ok(RunEnd::Done { transcript_id }) => assert_eq!(blocks_of(&e.lib(), transcript_id), baseline),
            Err(e) => panic!("k={k}: {e}"),
        }
    }
    assert!(paused_at_least_once > 10);
}

#[test]
fn user_cancel_discards_raw_data_and_a_retry_starts_over() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    let polls = Cell::new(0);
    let cancel = |_: &queue::JobInfo| {
        polls.set(polls.get() + 1);
        (polls.get() > 20).then_some(CancelReason::Cancel)
    };
    let (j, res) = runner::run_next(&e.app, &mut FakeEngine::new(), &cancel, &mut |_| {}).unwrap().unwrap();
    assert_eq!(res.unwrap(), RunEnd::Stopped(CancelReason::Cancel));
    assert_eq!(j.state, JobState::Cancelled);
    let lib = e.lib();
    assert!(!staging::has_raw(&lib, job.id).unwrap(), "partial raw data is gone");
    assert_eq!(lib.call_detail(call, None).unwrap().summary.transcription_state, "pending");
    // o auto-pickup voltaria a enfileirar a chamada `pending`; "tentar de novo" cria tarefa nova
    let again = queue::retry(&e.app, job.id).unwrap();
    assert_ne!(again.id, job.id);
    let (_, res) = e.run(&mut FakeEngine::new());
    done_id(&res);
    assert_contiguous(&e.lib(), again.id, Track::Sys, &starts(12));
    // não se tenta de novo uma tarefa pronta
    assert_eq!(queue::retry(&e.app, again.id).unwrap_err().code(), "conflict");
}

#[test]
fn cancelling_a_queued_job_and_the_open_job_rule() {
    let e = env();
    let call = e.call("call_a", Some(20), Some(20));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    assert_eq!(queue::enqueue(&e.app, e.lib_id, call, JobKind::Full, &JobOptions::default()).unwrap_err().code(), "conflict");
    assert_eq!(queue::enqueue_pending(&e.app).unwrap().len(), 0, "already has an open job");
    queue::mark_cancelled(&e.app, job.id).unwrap();
    assert_eq!(queue::get(&e.app, job.id).unwrap().state, JobState::Cancelled);
    // `pending` sem tarefa aberta: o polling enfileira
    let picked = queue::enqueue_pending(&e.app).unwrap();
    assert_eq!(picked.len(), 1);
    assert!(picked[0].id > job.id);
    // ids nunca são reaproveitados, mesmo depois de limpar o histórico
    queue::mark_cancelled(&e.app, picked[0].id).unwrap();
    assert_eq!(queue::prune_finished(&e.app, 0).unwrap(), 2);
    let next = e.enqueue(call, JobKind::Full, JobOptions::default());
    assert!(next.id > picked[0].id);
    let st = queue::status(&e.app, &Gate::READY, 20).unwrap();
    assert_eq!(st.jobs.len(), 1);
}

/// `blocked_by` da fila e de cada tarefa enfileirada, com a precedência pausa → instalação → runtime → modelos → `behind`.
#[test]
fn queue_explains_why_jobs_do_not_run() {
    let e = env();
    let a = e.enqueue(e.call("call_a", Some(20), Some(20)), JobKind::Full, JobOptions::default());
    let b = e.enqueue(e.call("call_b", Some(20), Some(20)), JobKind::Full, JobOptions::default());
    let c = e.enqueue(e.call("call_c", Some(20), Some(20)), JobKind::Full, JobOptions::default());
    let gate = |paused, runtime, models_ready, installing_runtime| Gate { paused, runtime, models_ready, installing_runtime };
    let view = |g: &Gate| {
        let st = queue::status(&e.app, g, 20).unwrap();
        let jobs: Vec<_> = st.jobs.iter().filter(|j| j.state == JobState::Queued).map(|j| (j.id, j.blocked_by, j.ahead)).collect();
        (st.blocked_by, st.paused, jobs)
    };
    // tudo pronto: a primeira vai rodar (sem motivo), as outras esperam a vez
    assert_eq!(view(&Gate::READY), (None, None, vec![(a.id, None, Some(0)), (b.id, Some(BlockedBy::Behind), Some(1)), (c.id, Some(BlockedBy::Behind), Some(2))]));
    // a primeira rodando: a seguinte tem 1 na frente, e a conta continua
    queue::mark_running(&e.app, a.id).unwrap();
    let (q, _, jobs) = view(&Gate::READY);
    assert_eq!((q, jobs), (None, vec![(b.id, Some(BlockedBy::Behind), Some(1)), (c.id, Some(BlockedBy::Behind), Some(2))]));
    let st = queue::status(&e.app, &Gate::READY, 20).unwrap();
    assert_eq!((st.jobs[0].id, st.jobs[0].state, st.jobs[0].blocked_by, st.jobs[0].ahead), (a.id, JobState::Running, None, None));
    queue::requeue(&e.app, a.id).unwrap();
    // motivo da fila vale para todas (inclusive a da frente), no lugar de `behind`
    for (g, want) in [
        (gate(None, "missing", true, false), BlockedBy::RuntimeMissing),
        (gate(None, "outdated", true, false), BlockedBy::RuntimeOutdated),
        (gate(None, "outdated", true, true), BlockedBy::RuntimeInstalling),
        (gate(None, "missing", true, true), BlockedBy::RuntimeInstalling),
        (gate(None, "ready", false, false), BlockedBy::ModelsMissing),
        (gate(Some(PauseReason::Recording), "ready", true, false), BlockedBy::PausedRecording),
        (gate(Some(PauseReason::User), "ready", true, false), BlockedBy::PausedUser),
    ] {
        let (q, paused, jobs) = view(&g);
        assert_eq!((q, paused), (Some(want), g.paused), "{g:?}");
        assert!(jobs.iter().all(|j| j.1 == Some(want)), "{g:?}: {jobs:?}");
        assert_eq!(jobs.iter().map(|j| j.2).collect::<Vec<_>>(), [Some(0), Some(1), Some(2)]);
    }
    // precedência: pausa do usuário > gravação > instalação > runtime > modelos
    let q = |g: Gate| g.blocker();
    assert_eq!(q(gate(Some(PauseReason::User), "outdated", false, true)), Some(BlockedBy::PausedUser));
    assert_eq!(q(gate(Some(PauseReason::Recording), "missing", false, false)), Some(BlockedBy::PausedRecording));
    assert_eq!(q(gate(None, "outdated", false, true)), Some(BlockedBy::RuntimeInstalling));
    assert_eq!(q(gate(None, "missing", false, false)), Some(BlockedBy::RuntimeMissing));
    assert_eq!(q(gate(None, "outdated", false, false)), Some(BlockedBy::RuntimeOutdated));
    // worker falso dispensa runtime e modelos; a instalação dos modelos não vira `runtime_installing`
    assert_eq!(q(gate(None, "fake", true, false)), None);
    assert_eq!(q(gate(None, "ready", false, false)), Some(BlockedBy::ModelsMissing));
    // fila vazia: o motivo da fila existe mesmo assim (a tela do ambiente usa)
    queue::mark_cancelled(&e.app, a.id).unwrap();
    queue::mark_cancelled(&e.app, b.id).unwrap();
    queue::mark_cancelled(&e.app, c.id).unwrap();
    assert_eq!(queue::status(&e.app, &gate(None, "outdated", true, false), 20).unwrap().blocked_by, Some(BlockedBy::RuntimeOutdated));
    // tarefas terminadas nunca carregam motivo
    assert!(queue::status(&e.app, &gate(None, "outdated", true, false), 20).unwrap().jobs.iter().all(|j| j.blocked_by.is_none() && j.ahead.is_none()));
}

#[test]
fn enqueue_validations() {
    let e = env();
    let none = queue::enqueue(&e.app, e.lib_id, 999, JobKind::Full, &JobOptions::default()).unwrap_err();
    assert_eq!(none.code(), "not_found");
    // sem arquivos de áudio
    let lib = e.lib();
    lib.conn
        .execute("INSERT INTO calls (key, started_at, created_at, mic_path, transcription_state) VALUES ('call_x', 't', 't', 'call_x/mic.flac', 'pending')", [])
        .unwrap();
    let id = lib.conn.last_insert_rowid();
    assert_eq!(queue::enqueue(&e.app, e.lib_id, id, JobKind::Full, &JobOptions::default()).unwrap_err().code(), "no_audio");
    // versão importada (sem bruto): rediarize/resegment recusam
    let call = e.call("call_a", Some(20), Some(20));
    lib.conn.execute("INSERT INTO transcripts (call_id, version, created_at, is_active) VALUES (?1, 1, 't', 1)", [call]).unwrap();
    for kind in [JobKind::Rediarize, JobKind::Resegment] {
        assert_eq!(queue::enqueue(&e.app, e.lib_id, call, kind, &JobOptions::default()).unwrap_err().code(), "no_raw_data");
    }
    let bad = JobOptions { language: Some("xx".into()), ..Default::default() };
    assert_eq!(queue::enqueue(&e.app, e.lib_id, call, JobKind::Full, &bad).unwrap_err().code(), "invalid");
}

#[test]
fn rediarize_and_resegment_reuse_the_raw_data_and_keep_speaker_names() {
    let e = env();
    let call = e.call("call_a", Some(100), Some(100));
    e.enqueue(call, JobKind::Full, JobOptions::default());
    let (full, res) = e.run(&mut FakeEngine::new());
    let v1 = done_id(&res);
    let mut lib = e.lib();
    // o usuário dá nomes
    let p1 = lib.find_speaker(call, "Pessoa 1").unwrap();
    lib.rename_speaker(p1.id, Some("Ana"), core_lib::Origin::Ui, false).unwrap();
    let p2 = lib.find_speaker(call, "Pessoa 2").unwrap();
    lib.rename_speaker(p2.id, Some("Bruno"), core_lib::Origin::Ui, false).unwrap();
    drop(lib);

    // separar de novo com 3 pessoas: só o worker de diarização roda (ASR e energia vêm do bruto)
    let mut spy = Spy::default();
    let job = e.enqueue(call, JobKind::Rediarize, JobOptions { expected_speakers: Some(3), ..Default::default() });
    assert_eq!(job.base_job_id, Some(full.id));
    let (_, res) = e.run(&mut spy);
    assert_eq!(spy.kinds, vec!["diarize"]);
    let v2 = done_id(&res);
    let lib = e.lib();
    assert_eq!(versions(&lib, call).iter().map(|v| v.1).collect::<Vec<_>>(), vec![false, true]);
    let blocks = blocks_of(&lib, v2);
    let labels: std::collections::BTreeSet<&str> = blocks.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(labels, ["Eu", "Pessoa 1", "Pessoa 2", "Pessoa 3"].into_iter().collect());
    // o texto do ASR é o mesmo; quem fala 0-15 s continua sendo a mesma pessoa (nome preservado)
    assert_eq!(blocks[0], ("Pessoa 1".into(), 0.0, 4.5, "fala trecho 0".into()));
    assert_eq!(lib.speakers(call).unwrap().iter().find(|s| s.label == "Pessoa 1").unwrap().name.as_deref(), Some("Ana"));
    assert_eq!(lib.speakers(call).unwrap().iter().find(|s| s.label == "Pessoa 2").unwrap().name.as_deref(), Some("Bruno"));
    assert_eq!(blocks_of(&lib, v1).len(), 40, "the old version is untouched");
    // o bruto-base não foi alterado e o novo job tem o seu
    assert_eq!(staging::segments(&lib, full.id, Track::Sys).unwrap().len(), 20);
    assert_eq!(staging::segments(&lib, job.id, Track::Sys).unwrap().len(), 20);

    // remontar: nenhuma requisição ao worker
    let mut spy = Spy::default();
    e.enqueue(call, JobKind::Resegment, JobOptions::default());
    let (j3, res) = e.run(&mut spy);
    assert!(spy.kinds.is_empty());
    let v3 = done_id(&res);
    assert_eq!(blocks_of(&lib, v3), blocks, "same raw data and parameters = same blocks");
    assert_eq!(versions(&lib, call).len(), 3);
    assert_eq!(j3.base_job_id, Some(job.id));
    // a remontagem herda o diagnóstico da junção (a etapa `diarize` é clonada com o `info_json`)
    let raw: String = lib.conn.query_row("SELECT params_json FROM transcripts WHERE id = ?1", [v3], |r| r.get(0)).unwrap();
    let d = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["diarization"].clone();
    assert_eq!((d["merge"].clone(), d["time_fuse"].clone()), (serde_json::json!({ "raw": 3, "final": 3 }), serde_json::json!("off")));
}

#[test]
fn single_track_calls() {
    let e = env();
    let only_mic = e.call("call_m", Some(20), None);
    let only_sys = e.call("call_s", None, Some(20));
    e.enqueue(only_mic, JobKind::Full, JobOptions::default());
    e.enqueue(only_sys, JobKind::Full, JobOptions::default());
    let mut spy = Spy::default();
    let (_, res) = e.run(&mut spy);
    assert_eq!(spy.kinds, vec!["transcribe"], "no energy, no diarization");
    assert!(blocks_of(&e.lib(), done_id(&res)).iter().all(|b| b.0 == "Eu"));
    let mut spy = Spy::default();
    let (_, res) = e.run(&mut spy);
    assert_eq!(spy.kinds, vec!["transcribe", "diarize"]);
    assert!(blocks_of(&e.lib(), done_id(&res)).iter().all(|b| b.0.starts_with("Pessoa ")));
}

#[test]
fn glossary_is_applied_to_text_but_not_to_original_text() {
    let e = env();
    e.app
        .add_global_rule(&core_lib::rules::RuleInput::replace("trecho", "parte"), None)
        .unwrap();
    let call = e.call("call_a", None, Some(20));
    e.enqueue(call, JobKind::Full, JobOptions::default());
    let (_, res) = e.run(&mut FakeEngine::new());
    let lib = e.lib();
    let detail = lib.call_detail(call, Some(done_id(&res))).unwrap();
    // faixa única, mesma pessoa (0-15 s): os trechos vizinhos (< 1 s) são fundidos num bloco; a partir de 15 s o
    // diarizador falso troca de pessoa (desde #25 a regra por tempo não a funde mais; quem junta é o worker)
    assert_eq!(detail.blocks[0].text, "fala parte 0 fala parte 1 fala parte 2");
    assert_eq!(detail.blocks[0].original_text, "fala trecho 0 fala trecho 1 fala trecho 2");
    assert!(!detail.blocks[0].edited);
    assert!(lib.history(Some(call), 10).unwrap().is_empty());
}

#[test]
fn missing_models_fail_the_job_and_retry_works_once_installed() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).unwrap();
    let lib_id = app.inbox_id().unwrap();
    let e = Env { _dir: dir, app, lib_id };
    let call = e.call("call_a", Some(20), Some(20));
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    let (j, res) = e.run(&mut FakeEngine::new());
    assert_eq!(res.unwrap_err().code(), "models_missing");
    assert_eq!((j.state, j.error_code.as_deref()), (JobState::Failed, Some("models_missing")));
    install_models(&e.app);
    queue::retry(&e.app, job.id).unwrap();
    let (j, res) = e.run(&mut FakeEngine::new());
    done_id(&res);
    assert_eq!(j.state, JobState::Done);
}

#[test]
fn unavailable_library_is_skipped_by_the_polling() {
    let e = env();
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("lib");
    let row = e.app.add_library("Empresa", &path).unwrap();
    let off = parent.path().join("gone");
    std::fs::rename(&path, &off).unwrap();
    // pasta e library.db sumiram (disco desmontado): nenhum banco novo é criado, nada falha
    assert!(queue::enqueue_pending(&e.app).unwrap().is_empty());
    assert_eq!(queue::recover_after_crash(&e.app).unwrap(), 0);
    assert!(!path.exists(), "polling must not recreate the folder of {}", row.name);
}

#[test]
fn moving_a_call_carries_the_raw_data_and_refuses_open_jobs() {
    let e = env();
    let target_dir = tempfile::tempdir().unwrap();
    let target = e.app.add_library("Empresa", target_dir.path()).unwrap();
    let call = e.call("call_a", Some(60), Some(60));
    e.enqueue(call, JobKind::Full, JobOptions::default());
    // tarefa aberta: não move
    assert_eq!(core_lib::transfer::assign(&e.app, e.lib_id, call, target.id, None).unwrap_err().code(), "conflict");
    let (full, res) = e.run(&mut QuietMic(FakeEngine::new()));
    let tid = done_id(&res);
    let lib = e.lib();
    assert_eq!(commit::bleed_removals(&lib, tid).unwrap().len(), 12);
    drop(lib);
    let (lib_id, new_call) = core_lib::transfer::assign(&e.app, e.lib_id, call, target.id, None).unwrap();
    let dst = e.app.open_library(lib_id).unwrap();
    let t = dst.call_detail(new_call, None).unwrap();
    assert!(t.transcripts[0].has_raw);
    assert_eq!(commit::bleed_removals(&dst, t.transcript_id.unwrap()).unwrap().len(), 12, "audit moved with the version");
    assert_eq!(staging::segments(&dst, full.id, Track::Sys).unwrap().len(), 12);
    assert!(staging::energy(&dst, full.id, Track::Mic).unwrap().is_some());
    assert!(staging::stage_done(&dst, full.id, staging::StageKey::Diarize).unwrap());
    // no origem não sobrou bruto, e a tarefa terminada saiu da fila
    assert!(!staging::has_raw(&e.lib(), full.id).unwrap());
    assert!(queue::get(&e.app, full.id).is_err());
    // no destino dá para remontar a partir do bruto copiado (filtro desligado: o Eu volta)
    let job = queue::enqueue(&e.app, lib_id, new_call, JobKind::Resegment, &JobOptions { bleed_filter: Some(false), ..Default::default() }).unwrap();
    assert_eq!(job.base_job_id, Some(full.id));
    let mut spy = Spy::default();
    let (_, res) = runner::run_next(&e.app, &mut spy, &|_| None, &mut |_| {}).unwrap().unwrap();
    assert!(spy.kinds.is_empty());
    let blocks = blocks_of(&dst, done_id(&res));
    assert_eq!(blocks.iter().filter(|b| b.0 == "Eu").count(), 12);
}

// ------------------------------------------------------------------ cortes de áudio (#23)

/// Os trechos (segundos na linha do tempo da chamada) em que algum bloco começa ou termina dentro de `[a, b)`.
fn inside(blocks: &[Row], a: f64, b: f64) -> Vec<&Row> {
    blocks.iter().filter(|r| r.1 < b && r.2 > a).collect()
}

#[test]
fn cuts_are_snapshotted_into_the_job_and_muted_audio_yields_no_segments() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    let mut lib = e.lib();
    lib.add_cuts(call, &[(10.0, 20.0)], core_lib::Origin::Ui, false).unwrap();
    let job = e.enqueue(call, JobKind::Full, JobOptions::default());
    assert_eq!(queue::get(&e.app, job.id).unwrap().options.cuts, vec![(10.0, 20.0)], "cópia no momento de criar o job");
    // um corte feito depois não muda o que o job já enfileirado vai usar
    lib.add_cuts(call, &[(40.0, 50.0)], core_lib::Origin::Ui, false).unwrap();
    assert_eq!(queue::get(&e.app, job.id).unwrap().options.cuts, vec![(10.0, 20.0)]);
    // o job roda com a cópia feita ao enfileirar
    let mut spy = Spy::default();
    let (_, res) = e.run(&mut spy);
    assert_eq!(spy.kinds, vec!["transcribe", "transcribe", "energy", "energy", "diarize"]);
    let blocks = blocks_of(&lib, done_id(&res));
    assert!(inside(&blocks, 10.0, 20.0).is_empty(), "nenhum trecho dentro do corte: {:?}", inside(&blocks, 10.0, 20.0));
    // fora do corte tudo continua na linha do tempo ORIGINAL (sem concatenar, sem deslocar)
    assert!(blocks.iter().any(|b| b.1 == 20.0) && blocks.iter().any(|b| b.1 == 5.0));
    assert!(!inside(&blocks, 40.0, 50.0).is_empty(), "o corte feito depois não valeu para este job");
    assert_eq!(blocks.len(), 24 - 4, "2 trechos por trilha caíram dentro de 10..20");
}

#[test]
fn the_muted_ranges_follow_the_mic_offset_per_track() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    let stream = |file: &str, first: i64| {
        serde_json::json!({"file": file, "device": "d", "description": "D", "is_monitor": false, "first_sample_unix_ms": first,
            "first_read_unix_ms": null, "latency_ms": null, "fragment_ms": 100, "samples": 0, "cuts": [], "reconnects": 0})
    };
    let sidecar = serde_json::json!({"schema": 1, "state": "complete", "key": "call_a", "app_version": "0", "started_at": "2026-01-01T00:00:00",
        "started_unix_ms": 0, "ended_at": null, "duration_s": null, "sample_rate": 16000, "channels": 1, "format": "s16le",
        "mic": stream("mic.wav", 3_000), "sys": stream("sys.wav", 1_000), "extra": {}});
    std::fs::write(e.lib().root().join("call_a/recording.json"), serde_json::to_vec(&sidecar).unwrap()).unwrap();
    let mut lib = e.lib();
    lib.add_cuts(call, &[(30.0, 40.0)], core_lib::Origin::Ui, false).unwrap();
    e.enqueue(call, JobKind::Full, JobOptions::default());
    /// Guarda o `mute` de cada requisição, com o áudio que ela leu (mic.flac / sys.flac).
    struct Rec(FakeEngine, std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<(f64, f64)>)>>>);
    impl Engine for Rec {
        fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
            match req {
                ToWorker::Transcribe { audio, mute, .. } => self.1.lock().unwrap().push((format!("asr:{audio}"), mute.clone())),
                ToWorker::Energy { audio, mute, .. } => self.1.lock().unwrap().push((format!("energy:{audio}"), mute.clone())),
                ToWorker::Diarize { mute, .. } => self.1.lock().unwrap().push(("diarize".into(), mute.clone())),
                _ => {}
            }
            self.0.execute(req, on_event)
        }
        fn shutdown(&mut self) {}
    }
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (_, res) = e.run(&mut Rec(FakeEngine::new(), log.clone()));
    let seen = log.lock().unwrap().clone();
    let find = |prefix: &str, track: &str| seen.iter().find(|s| s.0.starts_with(prefix) && (track.is_empty() || s.0.contains(track))).unwrap().1.clone();
    assert_eq!(find("asr:", "sys"), vec![(30.0, 40.0)], "sys: o eixo da chamada");
    assert_eq!(find("asr:", "mic"), vec![(28.0, 38.0)], "mic começou 2 s depois: o mesmo trecho está 2 s antes no arquivo");
    assert_eq!(find("energy:", "mic"), vec![(28.0, 38.0)]);
    assert_eq!(find("energy:", "sys"), vec![(30.0, 40.0)]);
    assert_eq!(find("diarize", ""), vec![(30.0, 40.0)]);
    let blocks = blocks_of(&lib, done_id(&res));
    assert!(inside(&blocks, 31.0, 39.0).is_empty());
}

#[test]
fn the_informed_number_is_a_ceiling_and_the_merge_params_are_recorded() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    /// Guarda os campos de junção da requisição de diarização.
    struct Rec(FakeEngine, std::sync::Arc<std::sync::Mutex<Vec<(Option<u32>, f64, f64)>>>);
    impl Engine for Rec {
        fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
            if let ToWorker::Diarize { max_speakers, merge_similarity, min_speaker_s, .. } = req {
                self.1.lock().unwrap().push((*max_speakers, *merge_similarity, *min_speaker_s));
            }
            self.0.execute(req, on_event)
        }
        fn shutdown(&mut self) {}
    }
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    e.enqueue(call, JobKind::Full, JobOptions { expected_speakers: Some(3), ..Default::default() });
    let (_, res) = e.run(&mut Rec(FakeEngine::new(), log.clone()));
    // padrões do #25: teto = o número informado; junção pela voz em 0,75 / 15 s
    assert_eq!(log.lock().unwrap().clone(), vec![(Some(3), 0.75, 15.0)]);
    let lib = e.lib();
    let raw: String = lib.conn.query_row("SELECT params_json FROM transcripts WHERE id = ?1", [done_id(&res)], |r| r.get(0)).unwrap();
    let d = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["diarization"].clone();
    assert_eq!(d["max_speakers"], 3);
    assert_eq!(d["expected_speakers"], 3);
    assert_eq!((d["merge_similarity"].as_f64(), d["min_speaker_s"].as_f64()), (Some(0.75), Some(15.0)));
    assert_eq!((d["min_cluster_pct"].as_f64(), d["min_cluster_s"].as_f64()), (Some(0.0), Some(0.0)), "a regra dos 5 % saiu");
    assert_eq!(d["merge"], serde_json::json!({ "raw": 3, "final": 3 }));
}

#[test]
fn turns_without_a_voice_merge_keep_the_old_time_fusion() {
    // worker de antes do #25 (ou runtime não atualizado): o resultado não traz `merge`
    struct Old(FakeEngine);
    impl Engine for Old {
        fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
            Ok(match self.0.execute(req, on_event)? {
                Terminal::Result(FromWorker::Result { id, segments, seconds, language, turns, speakers, step_ms, db, .. }) => {
                    Terminal::Result(FromWorker::Result { id, segments, seconds, language, turns, speakers, step_ms, db, merge: None })
                }
                other => other,
            })
        }
        fn shutdown(&mut self) {}
    }
    let diar = |e: &Env, engine: &mut dyn Engine| {
        let call = e.call("call_a", None, Some(20));
        e.enqueue(call, JobKind::Full, JobOptions::default());
        let (_, res) = e.run(engine);
        let lib = e.lib();
        let raw: String = lib.conn.query_row("SELECT params_json FROM transcripts WHERE id = ?1", [done_id(&res)], |r| r.get(0)).unwrap();
        let labels: std::collections::BTreeSet<String> = blocks_of(&lib, done_id(&res)).into_iter().map(|b| b.0).collect();
        (labels, serde_json::from_str::<serde_json::Value>(&raw).unwrap()["diarization"].clone())
    };
    // o falso troca de pessoa em 15 s (5 s de fala < 10 s): sem `merge` a fusão por tempo antiga junta
    let (labels, d) = diar(&env(), &mut Old(FakeEngine::new()));
    assert_eq!(labels, ["Pessoa 1".to_string()].into_iter().collect());
    assert_eq!((d["time_fuse"].clone(), d["merge"].clone(), d["min_cluster_s"].as_f64()), (serde_json::json!("legacy"), serde_json::Value::Null, Some(10.0)));
    // com `merge` (worker novo) a pessoa de 5 s fica: quem junta é o worker
    let (labels, d) = diar(&env(), &mut FakeEngine::new());
    assert_eq!(labels, ["Pessoa 1".to_string(), "Pessoa 2".to_string()].into_iter().collect());
    assert_eq!(d["time_fuse"], "off");
}

#[test]
fn rediarize_drops_cloned_passages_inside_cuts_made_after_the_full_run() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    e.enqueue(call, JobKind::Full, JobOptions::default());
    let (_, res) = e.run(&mut FakeEngine::new());
    let lib_blocks = blocks_of(&e.lib(), done_id(&res));
    assert!(!inside(&lib_blocks, 10.0, 20.0).is_empty());
    let mut lib = e.lib();
    lib.add_cuts(call, &[(10.0, 20.0)], core_lib::Origin::Ui, false).unwrap();
    let mut spy = Spy::default();
    e.enqueue(call, JobKind::Rediarize, JobOptions::default());
    let (job, res) = e.run(&mut spy);
    assert_eq!(spy.kinds, vec!["diarize"]);
    assert_eq!(job.options.cuts, vec![(10.0, 20.0)]);
    let blocks = blocks_of(&lib, done_id(&res));
    assert!(inside(&blocks, 10.0, 20.0).is_empty(), "o bruto clonado perdeu o que cai dentro do corte");
    assert_eq!(blocks.len(), lib_blocks.len() - 4);
}

#[test]
fn drop_cut_segments_applies_the_half_rule_with_the_mic_shifted_by_the_offset() {
    let e = env();
    let call = e.call("call_a", Some(60), Some(60));
    e.enqueue(call, JobKind::Full, JobOptions::default());
    let (full, _) = e.run(&mut FakeEngine::new());
    let mut lib = e.lib();
    let count = |lib: &Library, t| staging::segments(lib, full.id, t).unwrap().len();
    assert_eq!((count(&lib, Track::Sys), count(&lib, Track::Mic)), (12, 12));
    // segmentos de 4,5 s a cada 5 s. Corte 14..20: o sys [10,14.5) tem 11 % dentro (fica), o [15,19.5) está todo (sai).
    // Sem deslocamento o mic é igual: sai 1 de cada. Com o mic 2 s depois, o arquivo [10,14.5) vira [12,16.5) na chamada
    // (55 % dentro) e [15,19.5) vira [17,21.5) (67 %): saem 2 do mic.
    let n = staging::drop_cut_segments(&mut lib, full.id, &[(14.0, 20.0)], 2.0).unwrap();
    assert_eq!((n, count(&lib, Track::Sys), count(&lib, Track::Mic)), (3, 11, 10));
    // o corte vazio não mexe em nada
    assert_eq!(staging::drop_cut_segments(&mut lib, full.id, &[], 2.0).unwrap(), 0);
}
