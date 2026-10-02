//! Transcrição no shell (fase 4): comandos Tauri, thread da fila e eventos. Contrato:
//! TRANSCRIPTION_CONTRACT.md (§5 comandos, §7 fila, §9 eventos). A GUI é dona da fila; a CLI só grava em
//! `app.db` e garante que a GUI está de pé (a thread pega o que a CLI gravou por polling).
//!
//! A thread da fila tem a SUA conexão com `app.db` (como `import_start`): nunca segura o `Mutex<App>` dos
//! comandos durante uma tarefa. Ela também é dona do motor (`ProcessEngine`): o `PR_SET_PDEATHSIG` do worker
//! dispara quando a THREAD que fez o spawn termina, então o spawn sai daqui e o motor só vive enquanto há trabalho.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use core_lib::transcription::commit::{self, BleedRemoval};
use core_lib::transcription::engine::{Engine, ProcessEngine, WorkerLaunch};
use core_lib::transcription::models::{self, DownloadProgress, ModelStatus};
use core_lib::transcription::params::JobOptions;
use core_lib::transcription::queue::{self, JobInfo, JobKind, JobState, PauseReason, QueueStatus};
use core_lib::transcription::runner::{self, CancelReason, JobProgress, RunEnd, Stage};
use core_lib::transcription::runtime::{self, RuntimeProgress, RuntimeStatus};
use core_lib::transcription::{FAKE_WORKER_ENV, keys};
use core_lib::{App, Error};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

use crate::gui::{AppState, CmdError, R, with_app};
use crate::recording::RecState;

/// Nomes dos eventos emitidos pelo shell (a UI usa os mesmos em `api.ts`).
pub(crate) const EV_SETUP: &str = "transcription-setup";
pub(crate) const EV_QUEUE_CHANGED: &str = "queue-changed";
pub(crate) const EV_QUEUE_PROGRESS: &str = "queue-progress";
const EV_DATA_CHANGED: &str = "data-changed";

/// Intervalo do polling do `app.db` (tarefas da CLI, `pending` novas, pausa pedida pela CLI).
const POLL: Duration = Duration::from_secs(2);
/// De quanto em quanto a tarefa em curso reconsulta a pausa (banco + gravação).
const PAUSE_CHECK: Duration = Duration::from_secs(1);
/// `queue-progress`: no máximo 4 por segundo (a troca de etapa passa sempre).
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Quanto o gancho de saída espera a thread da fila terminar (cancelamento cooperativo + SIGKILL do worker).
const EXIT_WAIT: Duration = Duration::from_secs(150);
/// Segundos entre o `cancel` ignorado e o SIGKILL do grupo do worker.
const KILL_AFTER_S: u64 = 120;
/// Terminadas incluídas no estado da fila (a UI mostra 20).
const RECENT: usize = 20;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------ tipos dos comandos/eventos

/// Resposta de `transcription_status`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TranscriptionStatus {
    pub(crate) runtime: RuntimeStatus,
    pub(crate) models: Vec<ModelStatus>,
    /// instalação (runtime + modelos) em andamento: `phase` = `runtime` | `models`
    pub(crate) setup: SetupState,
    pub(crate) queue: QueueStatus,
    /// `TRANSCRICOES_FAKE_WORKER` ativo (a UI mostra um aviso discreto)
    pub(crate) fake_worker: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SetupState {
    pub(crate) running: bool,
    pub(crate) phase: Option<String>,
}

/// Evento `transcription-setup`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SetupEvent {
    /// `runtime` | `models` | `finished`
    pub(crate) phase: String,
    /// runtime: `download_uv` | `install_python` | `create_venv` | `sync_packages` | `verify`
    pub(crate) step: Option<String>,
    pub(crate) index: Option<u32>,
    pub(crate) of: Option<u32>,
    pub(crate) model: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) bytes_done: Option<u64>,
    pub(crate) bytes_total: Option<u64>,
    /// só em `finished` quando falhou
    pub(crate) error: Option<CmdError>,
}

impl SetupEvent {
    fn empty(phase: &str) -> SetupEvent {
        SetupEvent {
            phase: phase.into(),
            step: None,
            index: None,
            of: None,
            model: None,
            file: None,
            bytes_done: None,
            bytes_total: None,
            error: None,
        }
    }

    pub(crate) fn runtime(p: &RuntimeProgress) -> SetupEvent {
        SetupEvent { step: Some(p.step.clone()), index: Some(p.index), of: Some(p.of), ..SetupEvent::empty("runtime") }
    }

    pub(crate) fn download(p: &DownloadProgress) -> SetupEvent {
        SetupEvent {
            model: Some(p.model.clone()),
            file: Some(p.file.clone()),
            bytes_done: Some(p.bytes_done),
            bytes_total: Some(p.bytes_total),
            ..SetupEvent::empty("models")
        }
    }

    pub(crate) fn finished(error: Option<CmdError>) -> SetupEvent {
        SetupEvent { error, ..SetupEvent::empty("finished") }
    }
}

/// Payload de `data-changed` depois que a versão nasce.
pub(crate) fn transcribed_payload(job: &JobInfo, transcript_id: i64) -> Value {
    json!({
        "event": "transcribed",
        "library_id": job.library_id,
        "call_id": job.call_id,
        "job_id": job.id,
        "transcript_id": transcript_id,
    })
}

// ------------------------------------------------------------------ decisões puras

/// Por que a fila não anda. Pausa do usuário vence a da gravação (a UI explica a que o usuário controla).
pub(crate) fn pause_reason(user: bool, recording: bool) -> Option<PauseReason> {
    if user {
        Some(PauseReason::User)
    } else if recording {
        Some(PauseReason::Recording)
    } else {
        None
    }
}

/// O que pedir à tarefa em curso. Ordem: cancelar (vontade explícita do usuário) → fechar → pausar.
pub(crate) fn cancel_reason(shutdown: bool, cancel_job: Option<i64>, job_id: i64, paused: Option<PauseReason>) -> Option<CancelReason> {
    if cancel_job == Some(job_id) {
        Some(CancelReason::Cancel)
    } else if shutdown {
        Some(CancelReason::Shutdown)
    } else if paused.is_some() {
        Some(CancelReason::Pause)
    } else {
        None
    }
}

/// Cache da pausa: a tarefa consulta a cada evento do worker, o banco só é lido a cada `every`.
struct PauseProbe {
    every: Duration,
    last: Cell<Option<Instant>>,
    cached: Cell<Option<PauseReason>>,
}

impl PauseProbe {
    fn new(every: Duration) -> PauseProbe {
        PauseProbe { every, last: Cell::new(None), cached: Cell::new(None) }
    }

    fn get(&self, now: Instant, read: impl FnOnce() -> Option<PauseReason>) -> Option<PauseReason> {
        if self.last.get().is_none_or(|t| now.duration_since(t) >= self.every) {
            self.last.set(Some(now));
            self.cached.set(read());
        }
        self.cached.get()
    }
}

/// Limita `queue-progress` a ~4/s; mudar de etapa passa sempre.
struct ProgressGate {
    last: Option<(Instant, Stage)>,
}

impl ProgressGate {
    fn new() -> ProgressGate {
        ProgressGate { last: None }
    }

    fn allow(&mut self, stage: Stage, now: Instant) -> bool {
        let pass = match self.last {
            Some((t, s)) => s != stage || now.duration_since(t) >= PROGRESS_EVERY,
            None => true,
        };
        if pass {
            self.last = Some((now, stage));
        }
        pass
    }
}

/// Qual motor usar, a partir de `TRANSCRICOES_FAKE_WORKER`: vazio/ausente = real; `1`/`slow` = `worker.py --fake`
/// com o `python3` do sistema; caminho existente = esse interpretador (o `slow` é lido pelo próprio worker).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EngineChoice {
    Real,
    Fake { python: PathBuf },
    /// Só testes do laço: o motor em memória do núcleo (sem processo).
    #[cfg(test)]
    Memory,
}

pub(crate) fn engine_choice(env: Option<&str>) -> EngineChoice {
    match env.map(str::trim).filter(|v| !v.is_empty()) {
        None => EngineChoice::Real,
        Some(v) if v != "1" && v != "slow" && Path::new(v).exists() => EngineChoice::Fake { python: PathBuf::from(v) },
        Some(_) => EngineChoice::Fake { python: PathBuf::from("python3") },
    }
}

/// `TRANSCRICOES_FAKE_WORKER` ativo (valor não vazio).
pub(crate) fn fake_env() -> Option<String> {
    std::env::var(FAKE_WORKER_ENV).ok().filter(|v| !v.trim().is_empty())
}

/// Estado do runtime; com o worker falso ativo é sempre `fake` (não precisa de runtime nem de modelos).
pub(crate) fn runtime_status(data_dir: &Path) -> core_lib::Result<RuntimeStatus> {
    let mut status = runtime::status(data_dir)?;
    if fake_env().is_some() {
        status.state = "fake".into();
    }
    Ok(status)
}

fn setting_on(app: &App, key: &str, default: bool) -> bool {
    match app.setting(key).ok().flatten().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => default,
    }
}

/// Pausa do usuário (`transcription_queue_paused`; a CLI grava o mesmo valor).
fn user_paused(app: &App) -> bool {
    setting_on(app, keys::QUEUE_PAUSED, false)
}

// ------------------------------------------------------------------ estado compartilhado

/// Estado gerenciado pelo Tauri. A thread da fila e os comandos conversam por aqui.
pub(crate) struct TxState {
    shared: Arc<Shared>,
    rx: Mutex<Option<Receiver<()>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

struct Shared {
    shutdown: AtomicBool,
    /// Tarefa que o usuário mandou cancelar (a thread devolve `CancelReason::Cancel` a ela).
    cancel_current: Mutex<Option<i64>>,
    wake: Sender<()>,
    setup: Mutex<SetupState>,
    setup_cancel: AtomicBool,
}

impl Shared {
    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    fn wake(&self) {
        let _ = self.wake.send(());
    }
}

impl TxState {
    pub(crate) fn new() -> TxState {
        let (wake, rx) = mpsc::channel();
        TxState {
            shared: Arc::new(Shared {
                shutdown: AtomicBool::new(false),
                cancel_current: Mutex::new(None),
                wake,
                setup: Mutex::new(SetupState { running: false, phase: None }),
                setup_cancel: AtomicBool::new(false),
            }),
            rx: Mutex::new(Some(rx)),
            thread: Mutex::new(None),
        }
    }
}

/// O que a thread precisa do mundo de fora (gravando? emitir evento); no app vem do Tauri, nos testes é simulado.
trait Host: Send + Sync + 'static {
    fn recording(&self) -> bool;
    fn emit(&self, event: &str, payload: Value);
}

struct TauriHost(AppHandle);

impl Host for TauriHost {
    fn recording(&self) -> bool {
        self.0.state::<RecState>().is_recording()
    }

    fn emit(&self, event: &str, payload: Value) {
        let _ = self.0.emit(event, payload);
    }
}

// ------------------------------------------------------------------ a thread da fila

/// Visão da thread da fila: conexão própria + canal de eventos.
struct Ctx<'a> {
    shared: &'a Shared,
    host: &'a dyn Host,
    app: &'a App,
    /// Último estado emitido (JSON): `queue-changed` só sai quando muda.
    sig: RefCell<String>,
}

impl Ctx<'_> {
    /// Durante o fechamento a thread principal está parada no `Exit`: nada de emitir.
    fn emit(&self, event: &str, payload: Value) {
        if !self.shared.is_shutdown() {
            self.host.emit(event, payload);
        }
    }

    fn pause(&self) -> Option<PauseReason> {
        pause_reason(user_paused(self.app), self.host.recording())
    }

    fn status(&self) -> Option<Value> {
        let st = queue::status(self.app, self.pause(), RECENT).ok()?;
        serde_json::to_value(st).ok()
    }

    /// Emite `queue-changed` sempre.
    fn publish(&self) {
        if let Some(v) = self.status() {
            *self.sig.borrow_mut() = v.to_string();
            self.emit(EV_QUEUE_CHANGED, v);
        }
    }

    /// Emite `queue-changed` só se o estado mudou desde o último (tarefas da CLI, pausa, `pending` novas).
    fn publish_if_changed(&self) {
        if let Some(v) = self.status() {
            let s = v.to_string();
            if *self.sig.borrow() != s {
                *self.sig.borrow_mut() = s;
                self.emit(EV_QUEUE_CHANGED, v);
            }
        }
    }
}

/// Mostra cada mensagem de erro uma vez (o laço roda a cada 2 s; não encher o terminal).
struct LogOnce(Option<String>);

impl LogOnce {
    fn log(&mut self, msg: String) {
        if self.0.as_deref() != Some(msg.as_str()) {
            eprintln!("transcription: {msg}");
            self.0 = Some(msg);
        }
    }
}

/// Sobe a thread única da fila (dona do motor). Devolve o handle para o gancho de saída esperar.
fn spawn_loop(shared: Arc<Shared>, host: Arc<dyn Host>, rx: Receiver<()>, data_dir: PathBuf, choice: EngineChoice) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("transcription-queue".into())
        .spawn(move || run_loop(&shared, host.as_ref(), &rx, &data_dir, &choice))
        .expect("spawn transcription thread")
}

fn run_loop(shared: &Shared, host: &dyn Host, rx: &Receiver<()>, data_dir: &Path, choice: &EngineChoice) {
    let app = match App::open(data_dir) {
        Ok(a) => a,
        Err(e) => return eprintln!("transcription: {}: {}", e.code(), e.detail()),
    };
    let ctx = Ctx { shared, host, app: &app, sig: RefCell::new(String::new()) };
    let mut log = LogOnce(None);
    // `running` que sobrou de uma queda: já tem versão → `done`; senão volta à fila e retoma do bruto
    if let Err(e) = queue::recover_after_crash(&app) {
        log.log(format!("recover: {}: {}", e.code(), e.detail()));
    }
    let mut engine: Option<Box<dyn Engine>> = None;
    while !shared.is_shutdown() {
        if setting_on(&app, keys::AUTO, true)
            && let Err(e) = queue::enqueue_pending(&app)
        {
            log.log(format!("pending: {}: {}", e.code(), e.detail()));
        }
        ctx.publish_if_changed();
        let progressed = ctx.pause().is_none() && step(&ctx, &mut engine, choice, data_dir, &mut log);
        if !progressed {
            // sem trabalho (ou pausada): o worker não fica carregado na memória à toa
            if let Some(mut e) = engine.take() {
                e.shutdown();
            }
            // acorda por comando (`wake`) ou a cada `POLL`; junta os avisos acumulados
            let _ = rx.recv_timeout(POLL);
            while rx.try_recv().is_ok() {}
        }
    }
    if let Some(mut e) = engine.take() {
        e.shutdown();
    }
}

/// Um passo: roda a próxima tarefa, se houver e se der. `true` = algo andou (volta ao laço sem esperar).
fn step(ctx: &Ctx, engine: &mut Option<Box<dyn Engine>>, choice: &EngineChoice, data_dir: &Path, log: &mut LogOnce) -> bool {
    let app = ctx.app;
    let job = match queue::next_queued(app) {
        Ok(Some(j)) => j,
        Ok(None) => return false,
        Err(e) => {
            log.log(format!("queue: {}: {}", e.code(), e.detail()));
            return false;
        }
    };
    // sem runtime/modelos (e sem worker falso) a tarefa fica `queued`; a UI mostra o estado em `transcription_status`
    if !ready(choice, data_dir) {
        return false;
    }
    if engine.is_none() {
        match build_engine(app, choice, data_dir) {
            Ok(e) => *engine = Some(e),
            Err(e) => {
                // o worker não sobe (ex.: `runtime_outdated`): a tarefa falha com o motivo e o usuário pode tentar de novo
                let failed = queue::mark_running(app, job.id).and_then(|_| queue::mark_failed(app, job.id, e.code(), &e.detail()));
                if let Err(e2) = failed {
                    log.log(format!("fail job {}: {}: {}", job.id, e2.code(), e2.detail()));
                    return false;
                }
                ctx.publish();
                return true;
            }
        }
    }
    let engine = engine.as_mut().expect("engine").as_mut();

    let probe = PauseProbe::new(PAUSE_CHECK);
    let cancel = |j: &JobInfo| {
        let target = *lock(&ctx.shared.cancel_current);
        cancel_reason(ctx.shared.is_shutdown(), target, j.id, probe.get(Instant::now(), || ctx.pause()))
    };
    let mut gate = ProgressGate::new();
    let mut seen = None;
    let mut on_progress = |p: &JobProgress| {
        // `mark_running` acontece dentro do `run_next`: o primeiro progresso é o aviso de "começou"
        if seen != Some(p.job_id) {
            seen = Some(p.job_id);
            ctx.publish();
        }
        if gate.allow(p.stage, Instant::now())
            && let Ok(v) = serde_json::to_value(p)
        {
            ctx.emit(EV_QUEUE_PROGRESS, v);
        }
    };
    match runner::run_next(app, engine, &cancel, &mut on_progress) {
        Ok(Some((job, end))) => {
            let mut cancel_job = lock(&ctx.shared.cancel_current);
            if *cancel_job == Some(job.id) {
                *cancel_job = None;
            }
            drop(cancel_job);
            match end {
                Ok(RunEnd::Done { transcript_id }) => ctx.emit(EV_DATA_CHANGED, transcribed_payload(&job, transcript_id)),
                Ok(RunEnd::Stopped(_)) => {}
                Err(e) => log.log(format!("job {} ({}): {}: {}", job.id, job.call_key, e.code(), e.detail())),
            }
            ctx.publish();
            true
        }
        Ok(None) => false,
        Err(e) => {
            log.log(format!("run: {}: {}", e.code(), e.detail()));
            false
        }
    }
}

fn ready(choice: &EngineChoice, data_dir: &Path) -> bool {
    match choice {
        EngineChoice::Real => runtime::status(data_dir).is_ok_and(|s| s.state == "ready") && models::model_paths(data_dir).is_ok(),
        EngineChoice::Fake { .. } => true,
        #[cfg(test)]
        EngineChoice::Memory => true,
    }
}

/// Grava o `worker.py` embutido em `<dados>/runtime/worker.py` quando difere do que está lá.
fn write_worker_script(data_dir: &Path) -> core_lib::Result<runtime::RuntimePaths> {
    let rt = runtime::paths(data_dir);
    if std::fs::read_to_string(&rt.worker).ok().as_deref() != Some(runtime::WORKER_PY) {
        std::fs::create_dir_all(&rt.root)?;
        core_lib::fsx::write_atomic(&rt.worker, runtime::WORKER_PY.as_bytes())?;
    }
    Ok(rt)
}

/// Cria o motor NESTA thread (o spawn do worker depende dela).
fn build_engine(app: &App, choice: &EngineChoice, data_dir: &Path) -> core_lib::Result<Box<dyn Engine>> {
    let low_priority = setting_on(app, keys::LOW_PRIORITY, true);
    let launch = |python: PathBuf, rt: runtime::RuntimePaths, fake: bool| WorkerLaunch { python, script: rt.worker, fake, low_priority, kill_after_s: KILL_AFTER_S };
    match choice {
        EngineChoice::Real => {
            let rt = write_worker_script(data_dir)?;
            Ok(Box::new(ProcessEngine::spawn(launch(rt.python.clone(), rt, false))?))
        }
        EngineChoice::Fake { python } => {
            let rt = write_worker_script(data_dir)?;
            Ok(Box::new(ProcessEngine::spawn(launch(python.clone(), rt, true))?))
        }
        #[cfg(test)]
        EngineChoice::Memory => Ok(Box::new(core_lib::transcription::engine::FakeEngine::new())),
    }
}

// ------------------------------------------------------------------ ciclo de vida

/// Chamado em `setup`: sobe a thread da fila (`recover_after_crash` roda nela, antes da primeira tarefa).
pub(crate) fn startup(handle: &AppHandle) {
    let tx = handle.state::<TxState>();
    let Some(rx) = lock(&tx.rx).take() else { return };
    let data_dir = handle.state::<AppState>().data_dir.clone();
    let host = Arc::new(TauriHost(handle.clone()));
    let thread = spawn_loop(tx.shared.clone(), host, rx, data_dir, engine_choice(fake_env().as_deref()));
    *lock(&tx.thread) = Some(thread);
}

/// Chamado em todo `RunEvent`: em `Exit`, pede `CancelReason::Shutdown`, espera o `cancelled` do worker (o
/// processo pode demorar ~12 s para sair; sem diálogo extra) e a thread fechar o motor.
pub(crate) fn on_run_event(handle: &AppHandle, event: &RunEvent) {
    if matches!(event, RunEvent::Exit) {
        shutdown(&handle.state::<TxState>());
    }
}

fn shutdown(tx: &TxState) {
    tx.shared.shutdown.store(true, Ordering::SeqCst);
    tx.shared.setup_cancel.store(true, Ordering::SeqCst);
    tx.shared.wake();
    let Some(thread) = lock(&tx.thread).take() else { return };
    let deadline = Instant::now() + EXIT_WAIT;
    while !thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if thread.is_finished() {
        let _ = thread.join();
    }
}

// ------------------------------------------------------------------ comandos

fn queue_status_now(handle: &AppHandle, app: &App) -> core_lib::Result<QueueStatus> {
    let recording = handle.state::<RecState>().is_recording();
    queue::status(app, pause_reason(user_paused(app), recording), RECENT)
}

/// Avisa a UI (e só ela) de que a fila mudou por um comando.
fn publish(handle: &AppHandle, app: &App) {
    if let Ok(st) = queue_status_now(handle, app) {
        let _ = handle.emit(EV_QUEUE_CHANGED, st);
    }
}

#[tauri::command(async)]
pub(crate) fn transcription_status(handle: AppHandle, state: State<AppState>, tx: State<TxState>) -> R<TranscriptionStatus> {
    with_app(&state, |app| {
        Ok(TranscriptionStatus {
            runtime: runtime_status(&app.data_dir)?,
            models: models::status(&app.data_dir)?,
            setup: lock(&tx.shared.setup).clone(),
            queue: queue_status_now(&handle, app)?,
            fake_worker: fake_env().is_some(),
        })
    })
}

/// Instala o runtime e baixa os modelos que faltam, em thread própria, emitindo `transcription-setup`.
/// Já em andamento → `conflict`.
#[tauri::command(async)]
pub(crate) fn transcription_setup_start(handle: AppHandle, state: State<AppState>, tx: State<TxState>) -> R<()> {
    {
        let mut setup = lock(&tx.shared.setup);
        if setup.running {
            return Err(CmdError { code: "conflict".into(), detail: "setup already running".into() });
        }
        *setup = SetupState { running: true, phase: Some("runtime".into()) };
    }
    tx.shared.setup_cancel.store(false, Ordering::SeqCst);
    let shared = tx.shared.clone();
    let data_dir = state.data_dir.clone();
    std::thread::spawn(move || {
        let result = run_setup(&handle, &shared, &data_dir);
        *lock(&shared.setup) = SetupState { running: false, phase: None };
        let _ = handle.emit(EV_SETUP, SetupEvent::finished(result.err().map(CmdError::from)));
        // com tudo instalado, o que estava `queued` pode andar
        shared.wake();
    });
    Ok(())
}

fn run_setup(handle: &AppHandle, shared: &Shared, data_dir: &Path) -> core_lib::Result<()> {
    let cancel = &shared.setup_cancel;
    if !matches!(runtime_status(data_dir)?.state.as_str(), "ready" | "fake") {
        runtime::ensure(data_dir, &mut |p| { let _ = handle.emit(EV_SETUP, SetupEvent::runtime(p)); }, cancel)?;
    }
    lock(&shared.setup).phase = Some("models".into());
    // ~10 eventos por segundo no máximo; o fim de cada arquivo passa sempre
    let mut last: Option<(Instant, String)> = None;
    models::ensure(
        data_dir,
        &[],
        &mut |p| {
            let done = p.bytes_done >= p.bytes_total;
            let fresh = last.as_ref().is_none_or(|(t, f)| *f != p.file || t.elapsed() >= Duration::from_millis(100));
            if done || fresh {
                last = Some((Instant::now(), p.file.clone()));
                let _ = handle.emit(EV_SETUP, SetupEvent::download(p));
            }
        },
        cancel,
    )
}

#[tauri::command(async)]
pub(crate) fn transcription_setup_cancel(tx: State<TxState>) -> R<()> {
    tx.shared.setup_cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// `model`: `whisper` (pasta) | `segmentation` | `embedding` (arquivo). Devolve o estado de todos.
#[tauri::command(async)]
pub(crate) fn models_import_local(state: State<AppState>, tx: State<TxState>, model: String, path: String) -> R<Vec<ModelStatus>> {
    // a cópia pode ter gigabytes: sem segurar o `Mutex<App>`
    models::import_local(&state.data_dir, &model, Path::new(&path)).map_err(CmdError::from)?;
    tx.shared.wake();
    Ok(models::status(&state.data_dir)?)
}

#[tauri::command(async)]
pub(crate) fn transcribe_enqueue(
    handle: AppHandle,
    state: State<AppState>,
    tx: State<TxState>,
    library_id: i64,
    call_id: i64,
    kind: Option<JobKind>,
    options: Option<JobOptions>,
) -> R<JobInfo> {
    let job = with_app(&state, |app| {
        let job = queue::enqueue(app, library_id, call_id, kind.unwrap_or(JobKind::Full), &options.unwrap_or_default())?;
        publish(&handle, app);
        Ok(job)
    })?;
    // a chamada `failed` volta a `pending`: a lista recarrega
    let _ = handle.emit(EV_DATA_CHANGED, json!({"event": "changed", "library_id": library_id, "call_id": call_id}));
    tx.shared.wake();
    Ok(job)
}

/// Enfileira todas as chamadas `pending` sem tarefa aberta.
#[tauri::command(async)]
pub(crate) fn transcribe_pending(handle: AppHandle, state: State<AppState>, tx: State<TxState>) -> R<Vec<JobInfo>> {
    let jobs = with_app(&state, |app| {
        let jobs = queue::enqueue_pending(app)?;
        publish(&handle, app);
        Ok(jobs)
    })?;
    tx.shared.wake();
    Ok(jobs)
}

#[tauri::command(async)]
pub(crate) fn queue_status(handle: AppHandle, state: State<AppState>) -> R<QueueStatus> {
    with_app(&state, |app| queue_status_now(&handle, app))
}

/// `queued` cancela na hora; `running` pede o cancelamento cooperativo à thread (o bruto parcial é apagado quando o
/// worker confirma, e a UI vê o `queue-changed`).
#[tauri::command(async)]
pub(crate) fn queue_cancel(handle: AppHandle, state: State<AppState>, tx: State<TxState>, job_id: i64) -> R<()> {
    with_app(&state, |app| {
        let ask_thread = || *lock(&tx.shared.cancel_current) = Some(job_id);
        match queue::get(app, job_id)?.state {
            JobState::Running => ask_thread(),
            JobState::Queued => {
                queue::mark_cancelled(app, job_id)?;
                // a thread pode ter pego a tarefa entre as duas leituras
                if queue::get(app, job_id)?.state == JobState::Running {
                    ask_thread();
                }
            }
            _ => return Err(Error::Conflict(format!("job {job_id} already finished"))),
        }
        publish(&handle, app);
        Ok(())
    })?;
    tx.shared.wake();
    Ok(())
}

#[tauri::command(async)]
pub(crate) fn queue_retry(handle: AppHandle, state: State<AppState>, tx: State<TxState>, job_id: i64) -> R<JobInfo> {
    let job = with_app(&state, |app| {
        let job = queue::retry(app, job_id)?;
        publish(&handle, app);
        Ok(job)
    })?;
    tx.shared.wake();
    Ok(job)
}

/// Pausa/retoma pelo usuário (grava `transcription_queue_paused`). Devolve a fila já atualizada.
#[tauri::command(async)]
pub(crate) fn queue_pause(handle: AppHandle, state: State<AppState>, tx: State<TxState>, paused: bool) -> R<QueueStatus> {
    let status = with_app(&state, |app| {
        app.set_setting(keys::QUEUE_PAUSED, Some(if paused { "1" } else { "0" }))?;
        publish(&handle, app);
        queue_status_now(&handle, app)
    })?;
    tx.shared.wake();
    Ok(status)
}

/// Segmentos do mic descartados como vazamento numa versão (auditoria).
#[tauri::command(async)]
pub(crate) fn bleed_removals(state: State<AppState>, library_id: i64, transcript_id: i64) -> R<Vec<BleedRemoval>> {
    with_app(&state, |app| commit::bleed_removals(&app.open_library(library_id)?, transcript_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_lib::transcription::queue::JobState;
    use serde_json::json;

    fn job() -> JobInfo {
        JobInfo {
            id: 3,
            library_id: 1,
            call_id: 7,
            call_key: "call_2026-10-01_08-21-52".into(),
            kind: JobKind::Full,
            state: JobState::Running,
            options: JobOptions::default(),
            base_job_id: None,
            attempts: 1,
            stage: Some("asr_sys".into()),
            progress: Some(0.42),
            error_code: None,
            error_detail: None,
            created_at: "2026-10-02T10:00:00".into(),
            started_at: Some("2026-10-02T10:00:03".into()),
            finished_at: None,
        }
    }

    /// `JobInfo` e `QueueStatus` batem com o exemplo do §5.
    #[test]
    fn job_and_queue_json_match_contract() {
        let want = json!({
            "id": 3, "library_id": 1, "call_id": 7, "call_key": "call_2026-10-01_08-21-52", "kind": "full", "state": "running",
            "options": {"language": null, "expected_speakers": null, "bleed_filter": null, "bleed_margin_db": null, "diarization_threshold": null},
            "base_job_id": null, "attempts": 1, "stage": "asr_sys", "progress": 0.42, "error_code": null, "error_detail": null,
            "created_at": "2026-10-02T10:00:00", "started_at": "2026-10-02T10:00:03", "finished_at": null
        });
        assert_eq!(serde_json::to_value(job()).unwrap(), want);
        let st = |paused| serde_json::to_value(QueueStatus { paused, jobs: vec![job()] }).unwrap();
        assert_eq!(st(None), json!({"paused": null, "jobs": [want.clone()]}));
        assert_eq!(st(Some(PauseReason::User))["paused"], "user");
        assert_eq!(st(Some(PauseReason::Recording))["paused"], "recording");
    }

    #[test]
    fn transcription_status_json_matches_contract() {
        let v = serde_json::to_value(TranscriptionStatus {
            runtime: RuntimeStatus { state: "ready".into(), runtime_version: 1, python: "3.12.15".into(), uv: "0.12.22".into(), installed_at: Some("2026-10-02T09:00:00".into()) },
            models: vec![ModelStatus { id: "whisper".into(), installed: true, bytes_total: 10, bytes_done: 10, local: false }],
            setup: SetupState { running: true, phase: Some("models".into()) },
            queue: QueueStatus { paused: None, jobs: vec![] },
            fake_worker: false,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({
                "runtime": {"state": "ready", "runtime_version": 1, "python": "3.12.15", "uv": "0.12.22", "installed_at": "2026-10-02T09:00:00"},
                "models": [{"id": "whisper", "installed": true, "bytes_total": 10, "bytes_done": 10, "local": false}],
                "setup": {"running": true, "phase": "models"},
                "queue": {"paused": null, "jobs": []},
                "fake_worker": false
            })
        );
    }

    /// `transcription-setup`: todos os campos sempre presentes (null quando não se aplicam), como no `api.ts`.
    #[test]
    fn setup_events_json() {
        let rt = serde_json::to_value(SetupEvent::runtime(&RuntimeProgress { step: "create_venv".into(), index: 3, of: 5 })).unwrap();
        assert_eq!(
            rt,
            json!({"phase": "runtime", "step": "create_venv", "index": 3, "of": 5, "model": null, "file": null, "bytes_done": null, "bytes_total": null, "error": null})
        );
        let dl = DownloadProgress { model: "whisper".into(), file: "model.bin".into(), bytes_done: 5, bytes_total: 9 };
        assert_eq!(
            serde_json::to_value(SetupEvent::download(&dl)).unwrap(),
            json!({"phase": "models", "step": null, "index": null, "of": null, "model": "whisper", "file": "model.bin", "bytes_done": 5, "bytes_total": 9, "error": null})
        );
        let fin = serde_json::to_value(SetupEvent::finished(Some(CmdError { code: "setup_cancelled".into(), detail: "x".into() }))).unwrap();
        assert_eq!(fin["phase"], "finished");
        assert_eq!(fin["error"], json!({"code": "setup_cancelled", "detail": "x"}));
        assert!(serde_json::to_value(SetupEvent::finished(None)).unwrap()["error"].is_null());
    }

    #[test]
    fn progress_and_transcribed_payloads_match_contract() {
        let p = JobProgress { job_id: 3, library_id: 1, call_id: 7, stage: Stage::LoadingModel, fraction: None, audio_s: Some(12.5), total_s: Some(60.0) };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({"job_id": 3, "library_id": 1, "call_id": 7, "stage": "loading_model", "fraction": null, "audio_s": 12.5, "total_s": 60.0})
        );
        assert_eq!(
            transcribed_payload(&job(), 9),
            json!({"event": "transcribed", "library_id": 1, "call_id": 7, "job_id": 3, "transcript_id": 9})
        );
        let r = BleedRemoval { id: 1, t_start: 12.4, t_end: 15.0, text: "x".into(), containment: Some(0.8), margin_db: Some(-22.1), reason: "text_and_energy".into() };
        assert_eq!(
            serde_json::to_value(r).unwrap(),
            json!({"id": 1, "t_start": 12.4, "t_end": 15.0, "text": "x", "containment": 0.8, "margin_db": -22.1, "reason": "text_and_energy"})
        );
    }

    /// Os argumentos dos comandos chegam como a UI manda (`kind` snake_case, `options` nulo ou parcial).
    #[test]
    fn command_args_deserialize() {
        let kind: Option<JobKind> = serde_json::from_value(json!("rediarize")).unwrap();
        assert_eq!(kind, Some(JobKind::Rediarize));
        assert_eq!(serde_json::from_value::<Option<JobOptions>>(json!(null)).unwrap(), None);
        let o: Option<JobOptions> = serde_json::from_value(json!({"expected_speakers": 2, "bleed_filter": false})).unwrap();
        assert_eq!(o.unwrap(), JobOptions { expected_speakers: Some(2), bleed_filter: Some(false), ..Default::default() });
    }

    #[test]
    fn pause_and_cancel_decisions() {
        assert_eq!(pause_reason(false, false), None);
        assert_eq!(pause_reason(false, true), Some(PauseReason::Recording));
        assert_eq!(pause_reason(true, true), Some(PauseReason::User));
        let rec = Some(PauseReason::Recording);
        assert_eq!(cancel_reason(false, None, 3, None), None);
        assert_eq!(cancel_reason(false, None, 3, rec), Some(CancelReason::Pause));
        assert_eq!(cancel_reason(true, None, 3, rec), Some(CancelReason::Shutdown));
        assert_eq!(cancel_reason(true, Some(3), 3, rec), Some(CancelReason::Cancel));
        assert_eq!(cancel_reason(false, Some(4), 3, None), None, "cancelar outra tarefa não afeta esta");
    }

    #[test]
    fn pause_probe_reads_at_most_once_per_interval() {
        let probe = PauseProbe::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let reads = Cell::new(0);
        let read = |v| {
            reads.set(reads.get() + 1);
            v
        };
        assert_eq!(probe.get(t0, || read(None)), None);
        // dentro do intervalo: a leitura nova nem é feita
        assert_eq!(probe.get(t0 + Duration::from_millis(900), || read(Some(PauseReason::Recording))), None);
        assert_eq!(reads.get(), 1);
        assert_eq!(probe.get(t0 + Duration::from_millis(1000), || read(Some(PauseReason::Recording))), Some(PauseReason::Recording));
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn progress_gate_limits_rate_but_passes_stage_changes() {
        let mut g = ProgressGate::new();
        let t0 = Instant::now();
        assert!(g.allow(Stage::AsrSys, t0));
        assert!(!g.allow(Stage::AsrSys, t0 + Duration::from_millis(100)));
        assert!(g.allow(Stage::AsrMic, t0 + Duration::from_millis(110)), "etapa nova passa sempre");
        assert!(!g.allow(Stage::AsrMic, t0 + Duration::from_millis(300)));
        assert!(g.allow(Stage::AsrMic, t0 + Duration::from_millis(360)));
    }

    #[test]
    fn engine_choice_from_env() {
        assert_eq!(engine_choice(None), EngineChoice::Real);
        assert_eq!(engine_choice(Some("")), EngineChoice::Real);
        assert_eq!(engine_choice(Some("  ")), EngineChoice::Real);
        let py3 = EngineChoice::Fake { python: PathBuf::from("python3") };
        assert_eq!(engine_choice(Some("1")), py3);
        assert_eq!(engine_choice(Some("slow")), py3);
        assert_eq!(engine_choice(Some("/nao/existe/python")), py3);
        let exe = std::env::current_exe().unwrap();
        assert_eq!(engine_choice(exe.to_str()), EngineChoice::Fake { python: exe });
    }

    /// Host simulado: gravação por flag, eventos guardados.
    struct TestHost {
        recording: AtomicBool,
        events: Mutex<Vec<(String, Value)>>,
    }

    impl Host for TestHost {
        fn recording(&self) -> bool {
            self.recording.load(Ordering::SeqCst)
        }

        fn emit(&self, event: &str, payload: Value) {
            lock(&self.events).push((event.to_string(), payload));
        }
    }

    fn wait_for(host: &TestHost, what: &str, f: impl Fn(&[(String, Value)]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if f(&lock(&host.events)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timeout esperando: {what}; eventos: {:?}", lock(&host.events));
    }

    fn last_pause(events: &[(String, Value)]) -> Option<Value> {
        events.iter().rev().find(|(e, _)| e == EV_QUEUE_CHANGED).map(|(_, v)| v["paused"].clone())
    }

    /// O laço de verdade (thread, polling, acordar por comando, fechar): a gravação pausa a fila e o fim da gravação
    /// a libera, cada mudança vira `queue-changed`, e o fechamento termina a thread sem esperar o polling.
    #[test]
    fn loop_follows_recording_and_shuts_down() {
        let tmp = tempfile::tempdir().unwrap();
        App::open(tmp.path()).unwrap();
        let tx = TxState::new();
        let host = Arc::new(TestHost { recording: AtomicBool::new(false), events: Mutex::new(Vec::new()) });
        let rx = lock(&tx.rx).take().unwrap();
        *lock(&tx.thread) = Some(spawn_loop(tx.shared.clone(), host.clone(), rx, tmp.path().to_path_buf(), EngineChoice::Memory));

        wait_for(&host, "estado inicial", |ev| last_pause(ev) == Some(Value::Null));
        host.recording.store(true, Ordering::SeqCst);
        tx.shared.wake();
        wait_for(&host, "pausada pela gravação", |ev| last_pause(ev) == Some(json!("recording")));
        host.recording.store(false, Ordering::SeqCst);
        tx.shared.wake();
        wait_for(&host, "retomada", |ev| last_pause(ev) == Some(Value::Null));

        // pausa do usuário gravada no banco (como faz a CLI) é vista no polling/acordar
        App::open(tmp.path()).unwrap().set_setting(keys::QUEUE_PAUSED, Some("1")).unwrap();
        tx.shared.wake();
        wait_for(&host, "pausada pelo usuário", |ev| last_pause(ev) == Some(json!("user")));

        let started = Instant::now();
        shutdown(&tx);
        assert!(started.elapsed() < Duration::from_secs(1), "fechar não espera o polling de {POLL:?}");
        assert!(lock(&tx.thread).is_none());
    }
}
