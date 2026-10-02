//! Socket local do usuário (`$XDG_RUNTIME_DIR/transcricoes/ipc.sock`, modo 0600, nunca TCP).
//!
//! Protocolo **requisição/resposta em JSON-linhas**, uma requisição por conexão: o cliente conecta,
//! escreve uma linha, lê uma linha, fecha. O servidor atende cada conexão numa thread própria
//! (um `record_start` lento não trava um `status`).
//!
//! - Requisição: `{"cmd": "<nome>", ...campos}` (ver `Request`).
//! - Resposta: `{"ok": true, ...campos}` ou `{"ok": false, "code": "...", "detail": "..."}` (ver `Response`).
//! - **Compatibilidade**: uma linha **sem** `"cmd"` é o aviso antigo da CLI (`{"event": "changed", ...}`);
//!   vira `Request::Notify` e **não** recebe resposta (o cliente antigo já fechou).
//!
//! Sem app aberta: `record start|toggle` sobem a GUI destacada (`ensure_running`) e reenviam;
//! `status` responde `{state:"idle", app_running:false}` sem app; `stop`/`bar` → `not_running`.
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

use core_lib::paths;
use core_lib::recording::{RecordingInfo, StartRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Quanto o cliente espera a resposta (o servidor responde `record_stop` depois de parar a sessão: ≲ 1 s).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Quanto `ensure_running` espera o socket aparecer depois de subir a GUI.
pub const START_WAIT: Duration = Duration::from_secs(10);
/// Variável de ambiente que `spawn_gui` define: a GUI sobe **sem abrir a janela principal** (só tray e,
/// se for o caso, a barra). Decisão de B (contrato 10.4): quem grava por atalho/CLI não quer a janela.
pub const HIDDEN_ENV: &str = "TRANSCRICOES_HIDDEN";

// ------------------------------------------------------------------ mensagens

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Começa a gravar. Sem alvo = Não classificadas. Já gravando → `already_recording`.
    RecordStart(StartRequest),
    /// Para e finaliza em segundo plano. Resposta: o `Status` com `finalizing` contendo a chave.
    /// Sem gravação → `not_recording`.
    RecordStop,
    /// Ocioso → como `RecordStart` (usa o corpo); gravando → como `RecordStop`.
    RecordToggle(StartRequest),
    Status,
    BarShow,
    BarHide,
    /// Aviso antigo da CLI (a tela recarrega). `payload` = o JSON de sempre (`{"event": "changed", ...}`).
    Notify { payload: Value },
}

/// `{"ok": true, ...}` / `{"ok": false, "code", "detail"}`. Os campos extras ficam achatados.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(flatten)]
    pub data: Map<String, Value>,
}

impl Response {
    /// `body` deve serializar como objeto JSON (ou `null` para "sem campos").
    pub fn ok<T: Serialize>(body: T) -> Response {
        let data = match serde_json::to_value(body) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        Response { ok: true, code: None, detail: None, data }
    }

    pub fn err(code: impl Into<String>, detail: impl Into<String>) -> Response {
        Response { ok: false, code: Some(code.into()), detail: Some(detail.into()), data: Map::new() }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"ok":false,"code":"internal","detail":"serialize"}"#.into())
    }
}

/// Estado da gravação visto de fora: resposta de `status`, retorno de `record_status` e payload do evento
/// `record-state`. Campos em snake_case.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// `"idle"` | `"recording"`.
    pub state: String,
    /// `false` só quando a CLI não achou a app (resposta sintética).
    pub app_running: bool,
    /// Preenchido quando `state == "recording"`.
    pub recording: Option<RecordingInfo>,
    /// Chaves de gravações sendo convertidas/importadas em segundo plano (pode ser > 0 com `idle`).
    pub finalizing: Vec<String>,
    pub bar_visible: bool,
    /// Acelerador registrado agora (`null` = nenhum) e se o ambiente suporta atalho global (X11).
    pub shortcut: Option<String>,
    pub shortcut_supported: bool,
}

impl Status {
    /// Resposta da CLI quando não há app: `{state:"idle", app_running:false}`.
    pub fn not_running() -> Status {
        Status {
            state: "idle".into(),
            app_running: false,
            recording: None,
            finalizing: vec![],
            bar_visible: false,
            shortcut: None,
            shortcut_supported: false,
        }
    }
}

// ------------------------------------------------------------------ cliente

#[derive(Debug)]
pub enum IpcError {
    /// Sem socket / ninguém ouvindo: a app não está aberta.
    NotRunning,
    Io(std::io::Error),
    /// Resposta que não é JSON válido, ou conexão fechada sem resposta.
    Protocol(String),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::NotRunning => write!(f, "app is not running"),
            IpcError::Io(e) => write!(f, "{e}"),
            IpcError::Protocol(s) => write!(f, "{s}"),
        }
    }
}

/// Melhor esforço: sem app aberta, não faz nada. (Aviso antigo, sem resposta.)
pub fn notify(data_dir: &Path, msg: &Value) {
    let Ok(mut stream) = UnixStream::connect(paths::socket_path(data_dir)) else { return };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let _ = writeln!(stream, "{msg}");
}

/// Envia uma requisição ao socket `sock` e espera a resposta (`timeout`; a CLI usa `REQUEST_TIMEOUT`).
pub fn request_on(sock: &Path, req: &Request, timeout: Duration) -> Result<Response, IpcError> {
    let mut stream = UnixStream::connect(sock).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => IpcError::NotRunning,
        _ => IpcError::Io(e),
    })?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(IpcError::Io)?;
    stream.set_read_timeout(Some(timeout)).map_err(IpcError::Io)?;
    let line = serde_json::to_string(req).map_err(|e| IpcError::Protocol(e.to_string()))?;
    writeln!(stream, "{line}").map_err(IpcError::Io)?;
    let mut reply = String::new();
    BufReader::new(Read::take(stream, 1024 * 1024)).read_line(&mut reply).map_err(IpcError::Io)?;
    if reply.trim().is_empty() {
        return Err(IpcError::Protocol("connection closed without a response".into()));
    }
    serde_json::from_str(&reply).map_err(|e| IpcError::Protocol(e.to_string()))
}

/// Sobe a GUI destacada, sem janela principal (`HIDDEN_ENV`) (`$APPIMAGE` se existir — dentro de um AppImage `current_exe` aponta para o
/// squashfs, que some quando o processo sai —, senão `current_exe`), sem argumentos, com
/// `TRANSCRICOES_DATA_DIR` apontando para `data_dir`, stdio nulo e em grupo de processos próprio.
/// Subir duas vezes é inofensivo: o plugin de instância única absorve a segunda.
pub fn spawn_gui(data_dir: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    let exe = match exe {
        Some(e) => e,
        None => std::env::current_exe()?,
    };
    std::process::Command::new(exe)
        .env(paths::DATA_DIR_ENV, data_dir)
        .env(HIDDEN_ENV, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// Garante que a app está ouvindo em `sock`: se não estiver, sobe a GUI (`spawn_gui`) e espera o socket
/// aceitar conexões por até `wait` (`START_WAIT`), testando a cada 100 ms. `NotRunning` se estourar o prazo.
/// (O caminho do socket é parâmetro para os testes não tocarem no socket real do usuário.)
pub fn ensure_running_on(sock: &Path, data_dir: &Path, wait: Duration) -> Result<(), IpcError> {
    if UnixStream::connect(sock).is_ok() {
        return Ok(());
    }
    spawn_gui(data_dir).map_err(IpcError::Io)?;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if UnixStream::connect(sock).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(IpcError::NotRunning)
}

// ------------------------------------------------------------------ servidor

/// Abre o socket e atende cada conexão numa thread. `handler` recebe a requisição já decodificada;
/// o retorno é escrito na mesma conexão (exceto para o aviso antigo, sem `cmd`).
pub fn listen(data_dir: &Path, handler: impl Fn(Request) -> Response + Send + Sync + 'static) -> std::io::Result<()> {
    let dir = paths::runtime_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    // instância única garante que não há outra app ouvindo: o arquivo é resto de um encerramento abrupto
    serve_on(&paths::socket_path(data_dir), handler)
}

pub fn serve_on(sock: &Path, handler: impl Fn(Request) -> Response + Send + Sync + 'static) -> std::io::Result<()> {
    let _ = std::fs::remove_file(sock);
    let listener = UnixListener::bind(sock)?;
    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))?;
    let handler = std::sync::Arc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = handler.clone();
            std::thread::spawn(move || serve_one(stream, &*handler));
        }
    });
    Ok(())
}

fn serve_one(mut stream: UnixStream, handler: &dyn Fn(Request) -> Response) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    let Ok(clone) = stream.try_clone() else { return };
    if BufReader::new(Read::take(clone, 64 * 1024)).read_line(&mut line).is_err() {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(&line) else {
        let _ = writeln!(stream, "{}", Response::err("bad_request", "not valid JSON").to_line());
        return;
    };
    if value.get("cmd").is_none() {
        // aviso antigo (sem resposta)
        handler(Request::Notify { payload: value });
        return;
    }
    let response = match serde_json::from_value::<Request>(value) {
        Ok(req) => handler(req),
        Err(e) => Response::err("bad_request", e.to_string()),
    };
    let _ = writeln!(stream, "{}", response.to_line());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_wire_format() {
        let r: Request = serde_json::from_value(json!({"cmd": "record_start", "library_id": 2, "client_id": 5, "title": "T", "expected_speakers": 3, "mic": {"named": "x"}})).unwrap();
        let Request::RecordStart(s) = r else { panic!() };
        assert_eq!((s.meta.library_id, s.meta.client_id, s.meta.expected_speakers), (Some(2), Some(5), Some(3)));
        assert_eq!(s.mic, recorder::StreamChoice::Named("x".into()));
        assert_eq!(s.sys, recorder::StreamChoice::Default);
        let r: Request = serde_json::from_str(r#"{"cmd":"record_start"}"#).unwrap();
        assert_eq!(r, Request::RecordStart(StartRequest::default()));
        let r: Request = serde_json::from_str(r#"{"cmd":"record_toggle"}"#).unwrap();
        assert_eq!(r, Request::RecordToggle(StartRequest::default()));
        assert_eq!(serde_json::from_str::<Request>(r#"{"cmd":"record_stop"}"#).unwrap(), Request::RecordStop);
        assert_eq!(serde_json::to_value(Request::BarShow).unwrap(), json!({"cmd": "bar_show"}));
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"nope"}"#).is_err());
    }

    #[test]
    fn response_wire_format() {
        let line = Response::ok(Status::not_running()).to_line();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!((v["ok"].as_bool(), v["state"].as_str(), v["app_running"].as_bool()), (Some(true), Some("idle"), Some(false)));
        let back: Response = serde_json::from_str(&Response::err("not_running", "x").to_line()).unwrap();
        assert!(!back.ok && back.code.as_deref() == Some("not_running"));
    }

    #[test]
    fn roundtrip_over_socket_and_legacy_notify() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("ipc.sock");
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        serve_on(&sock, move |req| match req {
            Request::Notify { payload } => {
                tx.lock().unwrap().send(payload).unwrap();
                Response::ok(Value::Null)
            }
            Request::Status => Response::ok(Status::not_running()),
            _ => Response::err("not_recording", "nothing"),
        })
        .unwrap();
        let r = request_on(&sock, &Request::Status, Duration::from_secs(2)).unwrap();
        assert!(r.ok && r.data["state"] == "idle");
        let r = request_on(&sock, &Request::RecordStop, Duration::from_secs(2)).unwrap();
        assert_eq!(r.code.as_deref(), Some("not_recording"));
        // aviso antigo: sem "cmd", sem resposta
        let mut s = UnixStream::connect(&sock).unwrap();
        writeln!(s, r#"{{"event":"changed","library_id":1}}"#).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap()["event"], "changed");
        assert!(matches!(request_on(&tmp.path().join("nope.sock"), &Request::Status, Duration::from_secs(1)), Err(IpcError::NotRunning)));
    }
}
