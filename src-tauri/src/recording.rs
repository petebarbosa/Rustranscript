//! Gravação no shell Tauri: estado da gravação, comandos para a UI, eventos e atendimento
//! das requisições do socket (CLI). **A GUI é dona do gravador**: no máximo uma `ActiveRecording`.
//! Contrato (comandos, eventos, payloads): `RECORDING_CONTRACT.md`.
//!
//! - `record_stop`: `ActiveRecording::stop()` (rápido) e responde; `finalize` roda em **thread própria com
//!   `App::open(&data_dir)` próprio** (nunca segura `AppState.app` durante a conversão), emitindo
//!   `record-finalize-progress` (≤ ~100 eventos por trilha) e, no fim, `record-finalize-done` +
//!   `data-changed {event:"changed"}`.
//! - Um único **ticker** (~10 Hz) lê `levels()` e emite `record-levels` enquanto houver gravação ou
//!   monitor (ele é o único leitor dos medidores). `record_start` encerra o monitor.
//! - Todo `Err` volta como `CmdError {code, detail}` com os códigos do contrato.
//!
//! Travas (sempre adquiridas uma por vez, exceto `active` → `app` em `record_update`): `op` serializa
//! as transições (start/stop/monitor); `active`/`monitor`/`finalizing` são curtas; `AppState.app` só
//! durante chamadas rápidas ao núcleo.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use core_lib::recording::{self as core_rec, ActiveRecording, CallRef, LastUsed, Meta, Orphan, Progress, StartRequest, keys};
use core_lib::{App, Error};
use recorder::{CaptureBackend, DeviceInfo, Levels, Monitor, StreamChoice};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::gui::{AppState, CmdError, R, with_app};
use crate::i18n::{self, Lang};
use crate::ipc::{Request, Response, Status};
use crate::shortcut::ShortcutInfo;
use crate::{bar, shell, shortcut, tray};

// ------------------------------------------------------------------ nomes de eventos

/// `LevelsEvent` ~10 Hz enquanto grava ou monitora.
pub const EV_LEVELS: &str = "record-levels";
/// `Status` a cada mudança (início, fim, corte/reconexão, barra mostrada/escondida, finalizações).
pub const EV_STATE: &str = "record-state";
/// `FinalizeProgressEvent`.
pub const EV_FINALIZE_PROGRESS: &str = "record-finalize-progress";
/// `FinalizeDoneEvent`.
pub const EV_FINALIZE_DONE: &str = "record-finalize-done";
/// `Vec<Orphan>` — emitido no startup (se houver) e após `record_discard`/`record_recover`.
pub const EV_RECOVERY: &str = "record-recovery";

/// Intervalo do ticker de níveis (10 Hz).
const TICK: Duration = Duration::from_millis(100);

// ------------------------------------------------------------------ payloads

#[derive(Debug, Clone, Serialize)]
pub struct LevelsEvent {
    /// `"recording"` ou `"monitor"`.
    pub source: String,
    /// Só em `recording`.
    pub elapsed_s: Option<f64>,
    #[serde(flatten)]
    pub levels: Levels,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalizeProgressEvent {
    pub key: String,
    #[serde(flatten)]
    pub progress: Progress,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalizeDoneEvent {
    pub key: String,
    pub ok: bool,
    pub call: Option<CallRef>,
    /// Quando `ok == false`: `{code, detail}` (p.ex. `empty_recording`).
    pub error: Option<CmdError>,
}

/// Retorno de `record_info`: tudo que a tela de gravar/configurações precisa saber do ambiente.
#[derive(Debug, Clone, Serialize)]
pub struct RecordInfo {
    /// `"pulse"` | `"fake"` | `"unavailable"`.
    pub backend: String,
    /// `"x11"` | `"wayland"` | `"unknown"` (regra em `shortcut::session_type`).
    pub session_type: String,
    /// `XDG_CURRENT_DESKTOP` (ex.: `Hyprland`, `X-Cinnamon`).
    pub desktop: Option<String>,
    pub shortcut: ShortcutInfo,
    pub shortcut_default: String,
    pub bar_on_start: bool,
    pub last_used: LastUsed,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordDevices {
    pub backend: String,
    pub devices: Vec<DeviceInfo>,
}

// ------------------------------------------------------------------ estado

pub struct RecState {
    pub backend: Arc<dyn CaptureBackend>,
    pub active: Mutex<Option<ActiveRecording>>,
    pub monitor: Mutex<Option<Monitor>>,
    /// Chaves em finalização (conversão/importação em segundo plano).
    pub finalizing: Mutex<Vec<String>>,
    /// Serializa start/stop/monitor (as operações demoradas); nada mais a usa.
    op: Mutex<()>,
    /// A barra foi aberta pelo início automático (`record_bar_on_start`): fecha ao parar.
    bar_auto: AtomicBool,
    /// Saída confirmada: `ExitRequested` não deve mais ser impedido.
    quitting: AtomicBool,
    /// Há um diálogo "fechar durante a gravação" aberto (evita empilhar vários com cliques repetidos no X).
    close_prompt: AtomicBool,
    /// Último resultado de registro do atalho global (lido por `status`/`record_info`).
    pub shortcut: Mutex<ShortcutInfo>,
}

impl RecState {
    pub fn new() -> RecState {
        RecState {
            backend: recorder::default_backend(),
            active: Mutex::new(None),
            monitor: Mutex::new(None),
            finalizing: Mutex::new(Vec::new()),
            op: Mutex::new(()),
            bar_auto: AtomicBool::new(false),
            quitting: AtomicBool::new(false),
            close_prompt: AtomicBool::new(false),
            shortcut: Mutex::new(ShortcutInfo { accelerator: None, supported: shortcut::supported(), registered: false, error: None }),
        }
    }

    /// Gravando ou finalizando: o app não deve fechar sem confirmar.
    pub fn is_busy(&self) -> bool {
        lock(&self.active).is_some() || !lock(&self.finalizing).is_empty()
    }

    pub fn is_recording(&self) -> bool {
        lock(&self.active).is_some()
    }

    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    /// Reserva o diálogo de fechar; `false` se já há um aberto.
    pub fn begin_close_prompt(&self) -> bool {
        !self.close_prompt.swap(true, Ordering::SeqCst)
    }

    pub fn end_close_prompt(&self) {
        self.close_prompt.store(false, Ordering::SeqCst);
    }

    /// Espera as finalizações em segundo plano terminarem (usado por "Parar e sair").
    pub fn wait_finalized(&self) {
        while !lock(&self.finalizing).is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

impl Default for RecState {
    fn default() -> Self {
        Self::new()
    }
}

/// Trava tolerante a envenenamento (uma thread que entrou em pânico não deve derrubar a gravação).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn err(code: &str, detail: impl Into<String>) -> CmdError {
    CmdError { code: code.into(), detail: detail.into() }
}

/// Idioma das mensagens do shell (configuração `language`; senão o do sistema).
pub fn lang(handle: &AppHandle) -> Lang {
    let state = handle.state::<AppState>();
    let configured = lock(&state.app).setting("language").ok().flatten();
    configured.as_deref().and_then(Lang::parse).unwrap_or_else(Lang::system)
}

// ------------------------------------------------------------------ estado visto de fora

pub fn status(handle: &AppHandle) -> Status {
    let rec = handle.state::<RecState>();
    let recording = lock(&rec.active).as_ref().map(|a| a.info());
    let finalizing = lock(&rec.finalizing).clone();
    let sc = lock(&rec.shortcut).clone();
    Status {
        state: if recording.is_some() { "recording" } else { "idle" }.into(),
        app_running: true,
        recording,
        finalizing,
        bar_visible: bar::is_visible(handle),
        shortcut: if sc.registered { sc.accelerator } else { None },
        shortcut_supported: sc.supported,
    }
}

/// Emite `record-state` e atualiza o menu do tray.
pub fn emit_state(handle: &AppHandle) {
    let _ = handle.emit(EV_STATE, status(handle));
    tray::refresh(handle);
}

/// Mostra um erro de uma ação sem janela de retorno (atalho, tray): diálogo nativo.
pub fn report_error(handle: &AppHandle, e: &CmdError) {
    let l = lang(handle);
    let text = format!("{}: {}", i18n::error_prefix(l, &e.code), e.detail);
    handle.dialog().message(text).title(i18n::msg(l, "rec_error_title")).kind(MessageDialogKind::Error).show(|_| {});
}

// ------------------------------------------------------------------ pedido de início

/// Monta o `StartRequest` quando a origem não informou tudo: sem alvo (biblioteca e cliente ausentes) →
/// último alvo usado; `mic`/`sys` ausentes → últimos dispositivos. (Os campos de `meta` são sempre
/// os do chamador, exceto o alvo.)
pub fn build_request(last: &LastUsed, mut meta: Meta, mic: Option<StreamChoice>, sys: Option<StreamChoice>) -> StartRequest {
    if meta.library_id.is_none() && meta.client_id.is_none() {
        meta.library_id = last.library_id;
        meta.client_id = last.client_id;
    }
    StartRequest { meta, mic: mic.unwrap_or_else(|| last.mic.clone()), sys: sys.unwrap_or_else(|| last.sys.clone()) }
}

fn request_from_last(handle: &AppHandle, meta: Meta, mic: Option<StreamChoice>, sys: Option<StreamChoice>) -> R<StartRequest> {
    let state = handle.state::<AppState>();
    let last = with_app(&state, core_rec::last_used)?;
    Ok(build_request(&last, meta, mic, sys))
}

// ------------------------------------------------------------------ operações

/// Começa a gravar. Erros: `already_recording` e os do núcleo/recorder.
pub fn do_start(handle: &AppHandle, req: StartRequest) -> R<Status> {
    let state = handle.state::<AppState>();
    let rec = handle.state::<RecState>();
    let _op = lock(&rec.op);
    if rec.is_recording() {
        return Err(err("already_recording", "a recording is already in progress"));
    }
    // o monitor ocupa os mesmos dispositivos: some antes de gravar
    if let Some(m) = lock(&rec.monitor).take() {
        m.stop();
    }
    let (active, bar_on_start) = with_app(&state, |app| {
        // `core_rec::start` já grava o "último usado" (alvo + dispositivos)
        let active = core_rec::start(app, rec.backend.clone(), req)?;
        let bar_on_start = app.setting(keys::BAR_ON_START)?.as_deref() != Some("0");
        Ok((active, bar_on_start))
    })?;
    *lock(&rec.active) = Some(active);
    if bar_on_start && !bar::is_visible(handle) {
        rec.bar_auto.store(true, Ordering::SeqCst);
        bar::show(handle);
    }
    emit_state(handle);
    Ok(status(handle))
}

/// Para a sessão (rápido) e finaliza em segundo plano. Erro `not_recording`.
pub fn do_stop(handle: &AppHandle) -> R<Status> {
    let rec = handle.state::<RecState>();
    let _op = lock(&rec.op);
    let active = lock(&rec.active).take().ok_or_else(|| err("not_recording", "no recording in progress"))?;
    let key = active.key().to_string();
    // já conta como "finalizando" enquanto a sessão encerra: o app não pode fechar nesse intervalo
    lock(&rec.finalizing).push(key.clone());
    let stopped = match active.stop() {
        Ok(s) => s,
        Err(e) => {
            lock(&rec.finalizing).retain(|k| k != &key);
            emit_state(handle);
            return Err(e.into());
        }
    };
    if rec.bar_auto.swap(false, Ordering::SeqCst) {
        bar::hide(handle);
    }
    spawn_finalize(handle, stopped.key, false);
    emit_state(handle);
    Ok(status(handle))
}

/// Gravando → para; ocioso → começa com `req`.
pub fn do_toggle(handle: &AppHandle, req: StartRequest) -> R<Status> {
    if handle.state::<RecState>().is_recording() { do_stop(handle) } else { do_start(handle, req) }
}

/// `toggle` com o alvo e os dispositivos do último uso (atalho global, tray, UI).
pub fn toggle_with_last(handle: &AppHandle) -> R<Status> {
    if handle.state::<RecState>().is_recording() {
        return do_stop(handle);
    }
    do_start(handle, request_from_last(handle, Meta::default(), None, None)?)
}

fn do_update(handle: &AppHandle, meta: Meta) -> R<Status> {
    let state = handle.state::<AppState>();
    let rec = handle.state::<RecState>();
    {
        let mut active = lock(&rec.active);
        let a = active.as_mut().ok_or_else(|| err("not_recording", "no recording in progress"))?;
        with_app(&state, |app| a.update(app, &meta))?;
    }
    emit_state(handle);
    Ok(status(handle))
}

/// Finaliza (`recover = false`) ou recupera (`true`) em segundo plano. A chave deve estar em
/// `finalizing` (quem chama empurra); sai dela quando a thread termina.
fn spawn_finalize(handle: &AppHandle, key: String, recover: bool) {
    let handle = handle.clone();
    std::thread::spawn(move || {
        let data_dir = handle.state::<AppState>().data_dir.clone();
        let mut last = (String::new(), u64::MAX);
        let result = App::open(&data_dir).and_then(|app| {
            let mut on_progress = |p: Progress| {
                // limita a ~100 eventos por trilha
                if let Progress::Convert { track, done, of } = &p {
                    let pct = done * 100 / (*of).max(1);
                    if (track.clone(), pct) == last {
                        return;
                    }
                    last = (track.clone(), pct);
                }
                let _ = handle.emit(EV_FINALIZE_PROGRESS, FinalizeProgressEvent { key: key.clone(), progress: p });
            };
            if recover { core_rec::recover(&app, &key, &mut on_progress) } else { core_rec::finalize(&app, &key, &mut on_progress) }
        });
        let rec = handle.state::<RecState>();
        lock(&rec.finalizing).retain(|k| k != &key);
        let done = match result {
            Ok(call) => FinalizeDoneEvent { key: key.clone(), ok: true, call: Some(call), error: None },
            Err(e) => FinalizeDoneEvent { key: key.clone(), ok: false, call: None, error: Some(e.into()) },
        };
        if let Some(call) = &done.call {
            let _ = handle.emit("data-changed", serde_json::json!({"event": "changed", "library_id": call.library_id, "call_id": call.call_id}));
        }
        let _ = handle.emit(EV_FINALIZE_DONE, done);
        emit_state(&handle);
        if recover {
            emit_recovery(&handle);
        }
    });
}

/// Órfãs que ainda não estão sendo finalizadas, exceto a gravação ativa.
fn orphans_now(handle: &AppHandle) -> R<Vec<Orphan>> {
    let state = handle.state::<AppState>();
    let rec = handle.state::<RecState>();
    let active_key = lock(&rec.active).as_ref().map(|a| a.key().to_string());
    let mut list = with_app(&state, |app| core_rec::scan_orphans(app, active_key.as_deref()))?;
    let busy = lock(&rec.finalizing).clone();
    list.retain(|o| !busy.contains(&o.key));
    Ok(list)
}

/// Re-emite `record-recovery` com a lista atual (vazia = a UI fecha o diálogo).
fn emit_recovery(handle: &AppHandle) {
    match orphans_now(handle) {
        Ok(list) => {
            let _ = handle.emit(EV_RECOVERY, list);
        }
        Err(e) => eprintln!("record: orphans: {}: {}", e.code, e.detail),
    }
}

// ------------------------------------------------------------------ ganchos de ciclo de vida

/// Chamado no `setup`: inicia o ticker de níveis, registra o atalho, varre órfãs
/// (`core_lib::recording::scan_orphans`) e emite `record-recovery` se houver.
pub fn startup(handle: &AppHandle) {
    shortcut::register_from_settings(handle);
    let h = handle.clone();
    std::thread::spawn(move || ticker(h));
    let h = handle.clone();
    std::thread::spawn(move || {
        // dá tempo de a UI começar a escutar (ela também chama `record_orphans` no boot)
        std::thread::sleep(Duration::from_millis(1500));
        match orphans_now(&h) {
            Ok(list) if !list.is_empty() => {
                eprintln!("record: {} orphan recording(s) found", list.len());
                let _ = h.emit(EV_RECOVERY, list);
            }
            Ok(_) => {}
            Err(e) => eprintln!("record: orphans: {}: {}", e.code, e.detail),
        }
    });
}

/// Único leitor dos medidores: emite `record-levels` (~10 Hz) enquanto grava ou monitora e
/// `record-state` quando um fluxo cai/volta.
fn ticker(handle: AppHandle) {
    let rec = handle.state::<RecState>();
    let mut health: Option<(bool, u32, bool, u32)> = None;
    loop {
        std::thread::sleep(TICK);
        let recording = lock(&rec.active).as_ref().map(|a| {
            let info = a.info();
            let sig = (
                info.session.mic.as_ref().is_none_or(|s| s.alive),
                info.session.mic.as_ref().map_or(0, |s| s.cuts),
                info.session.sys.as_ref().is_none_or(|s| s.alive),
                info.session.sys.as_ref().map_or(0, |s| s.cuts),
            );
            (LevelsEvent { source: "recording".into(), elapsed_s: Some(info.session.elapsed_s), levels: a.levels() }, sig)
        });
        if let Some((ev, sig)) = recording {
            let _ = handle.emit(EV_LEVELS, ev);
            if health.is_some_and(|h| h != sig) {
                emit_state(&handle);
            }
            health = Some(sig);
            continue;
        }
        health = None;
        let monitor = lock(&rec.monitor).as_ref().map(|m| LevelsEvent { source: "monitor".into(), elapsed_s: None, levels: m.levels() });
        if let Some(ev) = monitor {
            let _ = handle.emit(EV_LEVELS, ev);
        }
    }
}

/// Pressionar o atalho global: `toggle` com o alvo padrão (último usado/inbox). Numa thread: iniciar
/// abre dispositivos (e pode criar a barra).
pub fn on_shortcut(handle: &AppHandle) {
    let h = handle.clone();
    std::thread::spawn(move || {
        if let Err(e) = toggle_with_last(&h) {
            report_error(&h, &e);
        }
    });
}

/// Ação "Gravar/Parar" do menu do tray (mesmo caminho do atalho).
pub fn on_tray_toggle(handle: &AppHandle) {
    on_shortcut(handle);
}

/// Item "Mostrar/Esconder barra" do tray.
pub fn on_tray_bar(handle: &AppHandle) {
    let h = handle.clone();
    std::thread::spawn(move || set_bar(&h, !bar::is_visible(&h)));
}

/// Mostrar/esconder a barra à mão: ela fica até `hide` (deixa de ser "automática").
fn set_bar(handle: &AppHandle, visible: bool) {
    handle.state::<RecState>().bar_auto.store(false, Ordering::SeqCst);
    if visible { bar::show(handle) } else { bar::hide(handle) }
}

/// Atende uma requisição do socket (CLI). `Notify` → emite `data-changed` com o payload (como na fase 2).
pub fn handle_request(handle: &AppHandle, req: Request) -> Response {
    let reply = |r: R<Status>| match r {
        Ok(s) => Response::ok(s),
        Err(e) => Response::err(e.code, e.detail),
    };
    match req {
        Request::Notify { payload } => {
            let _ = handle.emit("data-changed", payload);
            Response::ok(serde_json::Value::Null)
        }
        Request::RecordStart(req) => reply(do_start(handle, req)),
        Request::RecordStop => reply(do_stop(handle)),
        Request::RecordToggle(req) => reply(do_toggle(handle, req)),
        Request::Status => Response::ok(status(handle)),
        Request::BarShow => {
            set_bar(handle, true);
            Response::ok(serde_json::Value::Null)
        }
        Request::BarHide => {
            set_bar(handle, false);
            Response::ok(serde_json::Value::Null)
        }
    }
}

// ------------------------------------------------------------------ saída do app

/// Pede para sair (item "Sair" do tray): com gravação/finalização em andamento, confirma antes.
pub fn request_quit(handle: &AppHandle) {
    let rec = handle.state::<RecState>();
    if !rec.is_busy() {
        exit_now(handle);
        return;
    }
    let l = lang(handle);
    let text = if rec.is_recording() { i18n::msg(l, "quit_recording") } else { i18n::msg(l, "quit_finalizing") };
    let h = handle.clone();
    handle
        .dialog()
        .message(text)
        .title(i18n::msg(l, "quit_title"))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(i18n::msg(l, "quit_confirm").into(), i18n::msg(l, "quit_cancel").into()))
        .show(move |confirmed| {
            if confirmed {
                exit_now(&h);
            }
        });
}

/// Encerra o gravador de forma limpa (sidecar `complete`: a gravação vira órfã recuperável se a
/// finalização não chegar a rodar) e sai.
pub fn exit_now(handle: &AppHandle) {
    handle.state::<RecState>().quitting.store(true, Ordering::SeqCst);
    shutdown(handle);
    handle.exit(0);
}

/// Derruba o monitor e para uma gravação em andamento (sem finalizar).
pub fn shutdown(handle: &AppHandle) {
    let rec = handle.state::<RecState>();
    stop_monitor(&rec);
    let active = lock(&rec.active).take();
    if let Some(a) = active
        && let Err(e) = a.stop()
    {
        eprintln!("record: stop on exit: {e}");
    }
}

pub fn stop_monitor(rec: &RecState) {
    if let Some(m) = lock(&rec.monitor).take() {
        m.stop();
    }
}

// ------------------------------------------------------------------ comandos

#[tauri::command(async)]
pub fn record_info(handle: AppHandle, state: State<AppState>, rec: State<RecState>) -> R<RecordInfo> {
    let backend = backend_id(&rec);
    let (last_used, bar_on_start) = with_app(&state, |app| Ok((core_rec::last_used(app)?, app.setting(keys::BAR_ON_START)?.as_deref() != Some("0"))))?;
    Ok(RecordInfo {
        backend,
        session_type: shortcut::session_type().into(),
        desktop: std::env::var("XDG_CURRENT_DESKTOP").ok().filter(|v| !v.is_empty()),
        shortcut: shortcut::current(&handle),
        shortcut_default: shortcut::DEFAULT_ACCELERATOR.into(),
        bar_on_start,
        last_used,
    })
}

/// Id do backend para a UI: o Pulse devolve `"pulse"` mesmo sem servidor, então um `list_devices`
/// que falha com `backend_unavailable` vira `"unavailable"` (a UI desabilita a gravação).
fn backend_id(rec: &RecState) -> String {
    match rec.backend.list_devices() {
        Err(recorder::Error::BackendUnavailable(_)) => "unavailable".into(),
        _ => rec.backend.id().into(),
    }
}

/// Lista dispositivos (mic e monitores). Sem servidor de áudio: `{backend: "unavailable", devices: []}`
/// (não é erro; a UI mostra o aviso). Outras falhas viram erro.
#[tauri::command(async)]
pub fn record_devices(rec: State<RecState>) -> R<RecordDevices> {
    match rec.backend.list_devices() {
        Ok(devices) => Ok(RecordDevices { backend: rec.backend.id().into(), devices }),
        Err(recorder::Error::BackendUnavailable(_)) => Ok(RecordDevices { backend: "unavailable".into(), devices: vec![] }),
        Err(e) => Err(e.into()),
    }
}

#[tauri::command(async)]
pub fn record_status(handle: AppHandle) -> R<Status> {
    Ok(status(&handle))
}

/// Começa a gravar. `mic`/`sys` ausentes = os últimos usados (padrão `"default"`); sem alvo = o último
/// alvo (senão Não classificadas). Salva "último usado" se deu certo e mostra a barra se
/// `record_bar_on_start`. Erros: `already_recording`, `device_not_found`, `device_open_failed`,
/// `backend_unavailable`, `invalid`, `not_found`.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub fn record_start(
    handle: AppHandle,
    library_id: Option<i64>,
    client_id: Option<i64>,
    title: Option<String>,
    expected_speakers: Option<i64>,
    language: Option<String>,
    mic: Option<StreamChoice>,
    sys: Option<StreamChoice>,
) -> R<Status> {
    let meta = Meta { library_id, client_id, title, expected_speakers, language };
    do_start(&handle, request_from_last(&handle, meta, mic, sys)?)
}

/// Edita a gravação em andamento: **substitui** alvo, título, falantes e idioma (a UI manda todos).
#[tauri::command(async)]
pub fn record_update(
    handle: AppHandle,
    library_id: Option<i64>,
    client_id: Option<i64>,
    title: Option<String>,
    expected_speakers: Option<i64>,
    language: Option<String>,
) -> R<Status> {
    do_update(&handle, Meta { library_id, client_id, title, expected_speakers, language })
}

/// Para e finaliza em segundo plano (eventos `record-finalize-*`). Erro `not_recording`.
#[tauri::command(async)]
pub fn record_stop(handle: AppHandle) -> R<Status> {
    do_stop(&handle)
}

/// Ocioso → começa com o último alvo e dispositivos; gravando → para.
#[tauri::command(async)]
pub fn record_toggle(handle: AppHandle) -> R<Status> {
    toggle_with_last(&handle)
}

/// Abre os dispositivos só para medir níveis (eventos `record-levels` com `source: "monitor"`).
/// Substitui um monitor anterior. Recusado durante a gravação (`already_recording`). A UI deve chamar
/// `record_monitor_stop` ao sair da tela; o shell também para ao esconder/fechar a janela principal.
#[tauri::command(async)]
pub fn record_monitor_start(rec: State<RecState>, mic: Option<StreamChoice>, sys: Option<StreamChoice>) -> R<()> {
    let _op = lock(&rec.op);
    if rec.is_recording() {
        return Err(err("already_recording", "a recording is already in progress"));
    }
    // libera os dispositivos do monitor anterior antes de abrir os novos
    stop_monitor(&rec);
    let monitor = Monitor::start(rec.backend.clone(), &mic.unwrap_or_default(), &sys.unwrap_or_default()).map_err(Error::from)?;
    *lock(&rec.monitor) = Some(monitor);
    Ok(())
}

#[tauri::command(async)]
pub fn record_monitor_stop(rec: State<RecState>) -> R<()> {
    stop_monitor(&rec);
    Ok(())
}

/// Gravações órfãs (queda ou finalização incompleta), mais antigas primeiro.
#[tauri::command(async)]
pub fn record_orphans(handle: AppHandle) -> R<Vec<Orphan>> {
    orphans_now(&handle)
}

/// Recupera uma órfã em segundo plano (mesmos eventos `record-finalize-*`; a chave entra em `finalizing`).
#[tauri::command(async)]
pub fn record_recover(handle: AppHandle, rec: State<RecState>, key: String) -> R<()> {
    check_orphan_key(&rec, &key)?;
    lock(&rec.finalizing).push(key.clone());
    emit_state(&handle);
    spawn_finalize(&handle, key, true);
    Ok(())
}

/// Apaga uma órfã (WAVs incluídos). Irreversível: a UI confirma antes.
#[tauri::command(async)]
pub fn record_discard(handle: AppHandle, state: State<AppState>, rec: State<RecState>, key: String) -> R<()> {
    check_orphan_key(&rec, &key)?;
    with_app(&state, |app| core_rec::discard(app, &key))?;
    emit_recovery(&handle);
    Ok(())
}

/// Recusa a gravação ativa e a que já está sendo finalizada/recuperada (`invalid`).
fn check_orphan_key(rec: &RecState, key: &str) -> R<()> {
    if lock(&rec.active).as_ref().is_some_and(|a| a.key() == key) {
        return Err(err("invalid", "that recording is in progress"));
    }
    if lock(&rec.finalizing).iter().any(|k| k == key) {
        return Err(err("invalid", "that recording is already being processed"));
    }
    Ok(())
}

/// Troca o atalho global (`null` = desligar). Devolve o estado do registro.
#[tauri::command(async)]
pub fn record_set_shortcut(handle: AppHandle, accelerator: Option<String>) -> R<ShortcutInfo> {
    let info = shortcut::set(&handle, accelerator)?;
    emit_state(&handle);
    Ok(info)
}

#[tauri::command(async)]
pub fn bar_show(handle: AppHandle) -> R<()> {
    set_bar(&handle, true);
    Ok(())
}

#[tauri::command(async)]
pub fn bar_hide(handle: AppHandle) -> R<()> {
    set_bar(&handle, false);
    Ok(())
}

/// Traz a janela principal para a frente (botão "abrir janela" da barra).
#[tauri::command(async)]
pub fn show_main_window(handle: AppHandle) -> R<()> {
    shell::show_main(&handle);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last() -> LastUsed {
        LastUsed { library_id: Some(3), client_id: Some(9), mic: StreamChoice::Named("m".into()), sys: StreamChoice::Off }
    }

    #[test]
    fn build_request_fills_target_and_devices_from_last_use() {
        let r = build_request(&last(), Meta { title: Some("T".into()), ..Default::default() }, None, None);
        assert_eq!((r.meta.library_id, r.meta.client_id, r.meta.title.as_deref()), (Some(3), Some(9), Some("T")));
        assert_eq!((r.mic, r.sys), (StreamChoice::Named("m".into()), StreamChoice::Off));
    }

    #[test]
    fn build_request_keeps_explicit_target_and_devices() {
        let meta = Meta { library_id: Some(1), ..Default::default() };
        let r = build_request(&last(), meta, Some(StreamChoice::Default), None);
        assert_eq!((r.meta.library_id, r.meta.client_id), (Some(1), None), "alvo explícito não herda o cliente do último uso");
        assert_eq!((r.mic, r.sys), (StreamChoice::Default, StreamChoice::Off));
    }

    #[test]
    fn levels_event_wire_format() {
        let ev = LevelsEvent { source: "monitor".into(), elapsed_s: None, levels: Levels::default() };
        let v = serde_json::to_value(ev).unwrap();
        assert_eq!(v["source"], "monitor");
        assert!(v["elapsed_s"].is_null() && v["mic"].is_null() && v["sys"].is_null());
        let p = FinalizeProgressEvent { key: "k".into(), progress: Progress::Convert { track: "mic".into(), done: 1, of: 4 } };
        let v = serde_json::to_value(p).unwrap();
        assert_eq!((v["key"].as_str(), v["stage"].as_str(), v["done"].as_u64(), v["of"].as_u64()), (Some("k"), Some("convert"), Some(1), Some(4)));
    }
}
