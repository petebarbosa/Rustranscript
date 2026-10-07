//! Ciclo de vida da gravação (fase 3): `<dados>/recording/<key>/` → chamada na biblioteca escolhida.
//! Contrato completo e fluxos: `RECORDING_CONTRACT.md`. A captura em si mora no crate `recorder`;
//! aqui ficam as regras de negócio (alvo, validação, conversão para FLAC, criação da chamada,
//! recuperação de gravações órfãs e "último usado").
//!
//! Fluxo:
//! 1. `start` valida o alvo, cria `<dados>/recording/<key>/` e abre uma `recorder::Session`. As
//!    escolhas do usuário (`Intent`) vão no sidecar (`extra.intent`), então sobrevivem a uma queda.
//! 2. `ActiveRecording::stop` (rápido) para a sessão: `mic.wav`/`sys.wav` completos + sidecar `complete`.
//! 3. `finalize` (lento: converte FLAC; **rodar em thread própria com um `App` próprio**, nunca segurando
//!    o `Mutex<App>` da GUI) cria a chamada com `transcription_state = 'pending'`, move
//!    `mic.flac`/`sys.flac`/`recording.json` para a pasta da chamada, apaga os WAVs e a pasta de gravação.
//! 4. Se o app caiu (ou `finalize` falhou no meio), a pasta continua em `<dados>/recording/` e aparece em
//!    `scan_orphans`; `recover` faz o mesmo que `finalize`, reparando WAVs antes se o estado era `recording`.
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use recorder::sidecar::{MIC_WAV, SIDECAR_FILE, SYS_WAV};
use recorder::{CaptureBackend, Levels, Session, SessionStatus, Sidecar, StartOptions, State, StreamChoice};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::library::{self, Library};
use crate::text::{self, MAX_TITLE_CHARS};
use crate::{App, Error, Result, audio, db, fsx};

/// Subpasta de `<dados>` onde as gravações em andamento/órfãs ficam (fora de qualquer biblioteca).
pub const RECORDING_DIR: &str = "recording";
/// Nomes dos arquivos dentro da pasta da chamada depois de finalizada.
pub const MIC_FLAC: &str = "mic.flac";
pub const SYS_FLAC: &str = "sys.flac";
/// O sidecar acompanha a chamada (`<chamada>/recording.json`): a fase 4 lê o alinhamento mic×sys dele.
pub const SIDECAR_IN_CALL: &str = recorder::sidecar::SIDECAR_FILE;
pub const MAX_EXPECTED_SPEAKERS: i64 = 20;
/// Abaixo disto (por trilha, em segundos) a gravação é considerada vazia: `finalize` apaga a pasta e
/// devolve `empty_recording`.
pub const MIN_AUDIO_S: f64 = 0.5;

/// Arquivo de trava dentro de `recording/<key>/`: quem está usando a pasta (gravando, finalizando,
/// recuperando, descartando) segura um `flock` exclusivo nele. Evita duas finalizações da mesma chave
/// (duplo clique em Recuperar), recuperar/descartar a gravação ativa e oferecer como órfã uma pasta que
/// está sendo finalizada. O SO solta a trava se o processo morrer, então uma queda continua órfã.
const LOCK_FILE: &str = ".lock";

/// Trava exclusiva da pasta (solta ao sair de escopo).
struct DirLock(#[allow(dead_code)] File);

/// `Ok(None)` = outra pessoa está com a pasta.
fn lock_dir(dir: &Path) -> Result<Option<DirLock>> {
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(DirLock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// A pasta está em uso agora? (consulta com trava compartilhada: não atrapalha outra consulta, e a
/// janela em que poderia atrapalhar um `lock_dir` é de microssegundos.)
fn is_locked(dir: &Path) -> bool {
    let Ok(file) = File::open(dir.join(LOCK_FILE)) else { return false };
    matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock))
}

fn busy(key: &str) -> Error {
    Error::Conflict(format!("recording {key} is in use (still recording or being finalized)"))
}

/// Chaves em `app.db` → `settings` (valores sempre texto). Só o shell escreve; a UI usa os comandos.
pub mod keys {
    /// Último alvo usado: id da biblioteca (inbox incluída) e do cliente.
    pub const LIBRARY: &str = "record_library_id";
    pub const CLIENT: &str = "record_client_id";
    /// Últimos dispositivos: JSON de `StreamChoice` (`"default"`, `"off"`, `{"named":"…"}`).
    pub const MIC: &str = "record_mic";
    pub const SYS: &str = "record_sys";
    /// Atalho global (acelerador do Tauri, ex.: `Ctrl+Alt+R`); ausente = padrão; `"none"` = desligado.
    pub const SHORTCUT: &str = "record_shortcut";
    /// `"1"`/`"0"`: mostrar a barra ao começar a gravar (padrão `"1"`).
    pub const BAR_ON_START: &str = "record_bar_on_start";
}

// ------------------------------------------------------------------ pedido de início

/// O que o usuário escolhe antes de gravar (tudo opcional). É também o corpo de `record_update`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// Biblioteca de destino; `None` = Não classificadas (inbox).
    #[serde(default)]
    pub library_id: Option<i64>,
    /// Cliente (só em biblioteca de empresa); `None` = sem cliente.
    #[serde(default)]
    pub client_id: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    /// Quantas pessoas do outro lado (1..=`MAX_EXPECTED_SPEAKERS`).
    #[serde(default)]
    pub expected_speakers: Option<i64>,
    /// Idioma da transcrição (`pt`, `en`, `es`…); `None` = configuração `transcription_language`.
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StartRequest {
    #[serde(flatten)]
    pub meta: Meta,
    #[serde(default)]
    pub mic: StreamChoice,
    #[serde(default)]
    pub sys: StreamChoice,
}

/// `Meta` já validada e resolvida (vai em `Sidecar.extra.intent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    /// Sempre preenchido: id da inbox quando o alvo era "Não classificadas".
    pub library_id: i64,
    pub client_id: Option<i64>,
    /// Pode ser vazio (a UI mostra "Chamada de <data>").
    pub title: String,
    pub expected_speakers: Option<i64>,
    pub language: Option<String>,
}

impl Intent {
    /// Valida e resolve: biblioteca existe e está disponível (`not_found`); `client_id` pertence a ela e
    /// não é a inbox (`invalid`); título normalizado e ≤ `MAX_TITLE_CHARS`; falantes em 1..=20 (`invalid`).
    pub fn resolve(app: &App, meta: &Meta) -> Result<Intent> {
        let row = match meta.library_id {
            Some(id) => app.library_row(id)?,
            None => app.library_row(app.inbox_id()?)?,
        };
        if !row.is_inbox() && !row.root.join(library::DB_FILE).is_file() {
            return Err(Error::not_found(format!("library {} is unavailable", row.id)));
        }
        let client_id = match meta.client_id {
            None => None,
            Some(_) if row.is_inbox() => return Err(Error::invalid("unclassified calls have no clients")),
            Some(id) => {
                // só por id (o `find_client` também aceitaria nome/slug); fora da biblioteca = `invalid`
                let lib = Library::open(row.clone())?;
                let found = lib.clients()?.into_iter().find(|c| c.id == id);
                Some(found.ok_or_else(|| Error::invalid(format!("client {id} does not belong to library {}", row.id)))?.id)
            }
        };
        let title = text::normalize_ws(meta.title.as_deref().unwrap_or_default());
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(Error::invalid(format!("title longer than {MAX_TITLE_CHARS} characters")));
        }
        if let Some(n) = meta.expected_speakers
            && !(1..=MAX_EXPECTED_SPEAKERS).contains(&n)
        {
            return Err(Error::invalid(format!("expected speakers must be between 1 and {MAX_EXPECTED_SPEAKERS}")));
        }
        let language = match meta.language.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
            None => None,
            Some(l) => Some(language_code(l).ok_or_else(|| Error::invalid(format!("unsupported language: {l}")))?.to_string()),
        };
        Ok(Intent { library_id: row.id, client_id, title, expected_speakers: meta.expected_speakers, language })
    }
}

/// `pt`, `en`, `es` (aceita também `pt-BR`, `en_US`, `es-419`...) ou `auto` (detectar o idioma, fase 4).
pub fn language_code(raw: &str) -> Option<&'static str> {
    let lower = raw.trim().to_lowercase();
    match lower.split(['-', '_']).next()? {
        "auto" => Some("auto"),
        "pt" => Some("pt"),
        "en" => Some("en"),
        "es" => Some("es"),
        _ => None,
    }
}

/// A chave vem do IPC/UI e vira caminho (`recording/<key>`): só `call_...` com caracteres seguros,
/// nunca `/`, `..` etc. Evita que `discard`/`recover` apaguem ou leiam fora de `recording/`.
fn check_key(key: &str) -> Result<()> {
    let ok = key.starts_with("call_")
        && key.len() <= 128
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok { Ok(()) } else { Err(Error::invalid(format!("invalid recording key: {key:?}"))) }
}

/// O `Intent` que o `start`/`update` guardaram em `sidecar.extra.intent` (`None` se não há ou está ilegível).
fn intent_of(sidecar: &Sidecar) -> Option<Intent> {
    serde_json::from_value(sidecar.extra.get("intent")?.clone()).ok()
}

fn intent_extra(intent: &Intent) -> serde_json::Value {
    serde_json::json!({ "intent": intent })
}

// ------------------------------------------------------------------ gravação ativa

/// Estado da gravação em andamento (o que o shell devolve em `record_status` / no evento `record-state`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordingInfo {
    pub key: String,
    pub started_at: String,
    #[serde(flatten)]
    pub intent: Intent,
    #[serde(flatten)]
    pub session: SessionStatus,
}

/// Uma gravação em andamento. O processo da GUI guarda no máximo uma.
pub struct ActiveRecording {
    key: String,
    dir: PathBuf,
    intent: Intent,
    session: Session,
    /// Enquanto grava, a pasta fica travada: `recover`/`discard` dela dão `conflict` e ela não é órfã.
    _lock: DirLock,
}

/// Resultado de `ActiveRecording::stop`: os WAVs estão completos; falta `finalize(app, &key, ..)`.
#[derive(Debug, Clone)]
pub struct Stopped {
    pub key: String,
    pub dir: PathBuf,
    pub sidecar: Sidecar,
}

/// Valida (`Intent::resolve`), cria `recording/<key>/` (`key = new_key()`) e inicia a `recorder::Session`.
/// Já existe uma pasta com essa chave (dois starts no mesmo segundo) → sufixo `_2`, `_3`... na pasta
/// **e na chave** (`ActiveRecording::key()` é sempre o nome da pasta); o conflito com a biblioteca só
/// aparece em `finalize`. Erros do backend chegam de forma síncrona (`device_not_found`,
/// `device_open_failed`, `backend_unavailable`) e não deixam pasta. Quando o início dá certo grava o
/// "último usado" (alvo + dispositivos, `save_last_used`); o shell não precisa repetir.
pub fn start(app: &App, backend: Arc<dyn CaptureBackend>, req: StartRequest) -> Result<ActiveRecording> {
    let intent = Intent::resolve(app, &req.meta)?;
    let root = recording_root(&app.data_dir);
    std::fs::create_dir_all(&root)?;
    let (key, dir) = create_unique_dir(&root, &new_key())?;
    let lock = match lock_dir(&dir) {
        Ok(Some(l)) => l,
        other => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(other.err().unwrap_or_else(|| busy(&key)));
        }
    };
    let mut opts = StartOptions::new(&dir, &key);
    opts.mic = req.mic.clone();
    opts.sys = req.sys.clone();
    opts.app_version = env!("CARGO_PKG_VERSION").to_string();
    opts.extra = intent_extra(&intent);
    let session = match Session::start(backend, opts) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e.into());
        }
    };
    // melhor esforço: a gravação já começou, não vale derrubá-la por causa de uma preferência
    let _ = save_last_used(
        app,
        &LastUsed { library_id: Some(intent.library_id), client_id: intent.client_id, mic: req.mic, sys: req.sys },
    );
    Ok(ActiveRecording { key, dir, intent, session, _lock: lock })
}

/// Cria `<root>/<base>` (ou `<base>_2`, `_3`...) de forma atômica e devolve `(chave, pasta)`.
fn create_unique_dir(root: &Path, base: &str) -> Result<(String, PathBuf)> {
    for n in 1..=1000 {
        let key = if n == 1 { base.to_string() } else { format!("{base}_{n}") };
        let dir = root.join(&key);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((key, dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Conflict(format!("too many recordings started at {base}")))
}

impl ActiveRecording {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn intent(&self) -> &Intent {
        &self.intent
    }

    pub fn info(&self) -> RecordingInfo {
        RecordingInfo {
            key: self.key.clone(),
            started_at: self.session.started_at().to_string(),
            intent: self.intent.clone(),
            session: self.session.status(),
        }
    }

    /// Pico/RMS desde a última chamada (um único leitor). Ver `recorder::Session::levels`.
    pub fn levels(&self) -> Levels {
        self.session.levels()
    }

    /// Troca **todos** os campos do `Intent` (alvo, título, falantes, idioma) e regrava o sidecar.
    /// Vale também logo antes de `stop` (editar título ao parar).
    pub fn update(&mut self, app: &App, meta: &Meta) -> Result<Intent> {
        let intent = Intent::resolve(app, meta)?;
        self.session.update_extra(intent_extra(&intent))?;
        self.intent = intent.clone();
        Ok(intent)
    }

    /// Para a sessão (≲ 1 s). Depois, o shell chama `finalize` numa thread.
    pub fn stop(self) -> Result<Stopped> {
        // a trava cai ao fim desta função: o `finalize` seguinte já consegue pegá-la
        let sidecar = self.session.stop()?;
        Ok(Stopped { key: self.key, dir: self.dir, sidecar })
    }
}

// ------------------------------------------------------------------ finalizar / recuperar

/// Progresso de `finalize`/`recover` (o shell o repassa no evento `record-finalize-progress`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Progress {
    /// Reparo do WAV (só em `recover` de gravação interrompida).
    Repair { track: String },
    /// Conversão WAV → FLAC de uma trilha (`done`/`of` em quadros, como no `import-progress`).
    Convert { track: String, done: u64, of: u64 },
    /// Criando a chamada no banco e movendo arquivos.
    Call,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRef {
    pub library_id: i64,
    pub call_id: i64,
    pub key: String,
}

/// Transforma `recording/<key>/` (sidecar `complete`) numa chamada:
/// 1. `wav_to_flac` de cada trilha presente → `<pasta da chamada>/mic.flac` / `sys.flac` (streaming);
/// 2. numa transação: `INSERT calls` com `title`/`expected_speakers`/`language` do `Intent`
///    (idioma ausente → configuração `transcription_language` → `pt`), `duration_s` = maior duração
///    entre as trilhas, `dir`/`mic_path`/`sys_path` relativos à raiz, **`transcription_state = 'pending'`**,
///    nenhuma linha em `transcripts`/`speakers`/`blocks`;
/// 3. move `recording.json` para a pasta da chamada; apaga WAVs e `recording/<key>/`.
///
/// Alvo (`Intent.library_id`) que não existe mais ou está indisponível → cai na inbox. `client_id`
/// inexistente → sem cliente. Em qualquer erro a pasta de gravação **permanece** (aparece em
/// `scan_orphans`) e nenhum arquivo parcial fica na biblioteca. Sidecar `recording` → `invalid`
/// (use `recover`). Gravação vazia (< `MIN_AUDIO_S` em todas as trilhas) → apaga a pasta e devolve
/// `empty_recording`. Chave já existente na biblioteca → `conflict`.
pub fn finalize(app: &App, key: &str, progress: &mut dyn FnMut(Progress)) -> Result<CallRef> {
    process(app, key, false, progress)
}

/// Como `finalize`, mas aceita sidecar `recording` (queda): antes de converter faz `recorder::repair_wav`
/// em cada trilha (`Progress::Repair`) e recalcula amostras/duração a partir dos arquivos. Um WAV com
/// menos de 44 bytes (queda logo ao criar o arquivo) conta como trilha vazia; cabeçalho que não é o
/// nosso → `invalid_wav` (a pasta fica; só dá para descartar).
pub fn recover(app: &App, key: &str, progress: &mut dyn FnMut(Progress)) -> Result<CallRef> {
    process(app, key, true, progress)
}

/// Apaga `recording/<key>/` sem criar chamada (`not_found` se não existe).
pub fn discard(app: &App, key: &str) -> Result<()> {
    check_key(key)?;
    let dir = recording_dir(&app.data_dir, key);
    if !dir.is_dir() {
        return Err(Error::not_found(format!("recording {key}")));
    }
    let Some(_lock) = lock_dir(&dir)? else { return Err(busy(key)) };
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Uma trilha com áudio, pronta para virar FLAC.
struct TrackJob {
    name: &'static str,
    wav: PathBuf,
    flac: &'static str,
    frames: u64,
}

fn process(app: &App, key: &str, recovering: bool, progress: &mut dyn FnMut(Progress)) -> Result<CallRef> {
    check_key(key)?;
    let dir = recording_dir(&app.data_dir, key);
    if !dir.is_dir() {
        return Err(Error::not_found(format!("recording {key}")));
    }
    let Some(_lock) = lock_dir(&dir)? else { return Err(busy(key)) };
    let mut sidecar = match Sidecar::read(&dir) {
        Ok(s) => s,
        Err(recorder::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::not_found(format!("recording {key} has no {SIDECAR_FILE}")));
        }
        Err(e) => return Err(e.into()),
    };
    let crashed = sidecar.state == State::Recording;
    if crashed && !recovering {
        return Err(Error::invalid(format!("recording {key} was interrupted (state recording): use recover")));
    }

    // 1. trilhas com áudio (reparando os WAVs de uma queda)
    let mut jobs = Vec::new();
    for (name, present, wav_name, flac) in [
        ("mic", sidecar.mic.is_some(), MIC_WAV, MIC_FLAC),
        ("sys", sidecar.sys.is_some(), SYS_WAV, SYS_FLAC),
    ] {
        let wav = dir.join(wav_name);
        if !present || !wav.is_file() || std::fs::metadata(&wav)?.len() < recorder::wav::HEADER_LEN {
            continue;
        }
        if crashed {
            progress(Progress::Repair { track: name.into() });
            recorder::repair_wav(&wav)?;
        }
        let frames = audio::wav_info(&wav)?.frames;
        if frames > 0 {
            jobs.push(TrackJob { name, wav, flac, frames });
        }
    }
    let longest = jobs.iter().map(|j| j.frames).max().unwrap_or(0) as f64 / f64::from(recorder::SAMPLE_RATE);
    if longest < MIN_AUDIO_S {
        std::fs::remove_dir_all(&dir)?;
        return Err(Error::recording("empty_recording", format!("{key}: no audio recorded")));
    }

    // 2. alvo: o que o usuário escolheu, se ainda existir; senão a inbox / sem cliente
    let intent = intent_of(&sidecar);
    let wanted = intent.as_ref().map(|i| i.library_id);
    let row = wanted
        .and_then(|id| app.library_row(id).ok())
        .filter(|r| r.is_inbox() || r.root.join(library::DB_FILE).is_file());
    let row = match row {
        Some(r) => r,
        None => app.library_row(app.inbox_id()?)?,
    };
    let kept_target = Some(row.id) == wanted;
    let mut lib = Library::open(row)?;
    let client_id = match intent.as_ref().and_then(|i| i.client_id) {
        Some(c) if kept_target && !lib.row.is_inbox() => lib.clients()?.into_iter().find(|x| x.id == c).map(|x| x.id),
        _ => None,
    };
    if lib.call_id_by_key(key)?.is_some() {
        return Err(Error::Conflict(format!("call {key} already exists in library {}", lib.id())));
    }
    let rel_dir = lib.call_dir_for(key, "", client_id)?;
    let call_dir = lib.audio_abs(&rel_dir.to_string_lossy());
    if call_dir.exists() && std::fs::read_dir(&call_dir)?.next().is_some() {
        return Err(Error::Conflict(format!("call folder already exists: {}", call_dir.display())));
    }
    let created_dir = !call_dir.exists();

    // 3. FLAC + sidecar na pasta da chamada, depois a linha no banco; qualquer erro desfaz os arquivos
    // idioma: o do `Intent`, senão a configuração `transcription_language` (`pt`/`pt-BR`...), senão `pt`
    let language = intent
        .as_ref()
        .and_then(|i| i.language.clone())
        .or_else(|| app.setting("transcription_language").ok().flatten().and_then(|v| language_code(&v).map(str::to_string)))
        .unwrap_or_else(|| "pt".to_string());
    let built = build_call(&mut lib, &mut sidecar, &call_dir, &rel_dir, client_id, intent, &language, &jobs, key, progress);
    let call_id = match built {
        Ok(id) => id,
        Err(e) => {
            for f in [MIC_FLAC, SYS_FLAC, SIDECAR_IN_CALL, "recording.json.part"] {
                let _ = std::fs::remove_file(call_dir.join(f));
            }
            for j in &jobs {
                let _ = std::fs::remove_file(fsx::tmp_sibling(&call_dir.join(j.flac)));
            }
            if created_dir {
                let _ = std::fs::remove_dir(&call_dir);
                if let Some(parent) = call_dir.parent() {
                    fsx::prune_empty_dirs(parent, lib.root());
                }
            }
            return Err(e);
        }
    };

    // 4. a chamada existe: o sidecar já está na pasta dela; sai o resto (sem ele a pasta deixa de ser órfã)
    let _ = std::fs::remove_file(dir.join(SIDECAR_FILE));
    let _ = std::fs::remove_dir_all(&dir);
    Ok(CallRef { library_id: lib.id(), call_id, key: key.to_string() })
}

#[allow(clippy::too_many_arguments)]
fn build_call(
    lib: &mut Library,
    sidecar: &mut Sidecar,
    call_dir: &Path,
    rel_dir: &Path,
    client_id: Option<i64>,
    intent: Option<Intent>,
    language: &str,
    jobs: &[TrackJob],
    key: &str,
    progress: &mut dyn FnMut(Progress),
) -> Result<i64> {
    std::fs::create_dir_all(call_dir)?;
    for job in jobs {
        let mut last_pct = u64::MAX;
        audio::wav_to_flac(&job.wav, &call_dir.join(job.flac), &mut |done, of| {
            // ~1 evento por 1 % (o shell repassa cada um para a UI)
            let pct = (done * 100).checked_div(of).unwrap_or(100);
            if pct != last_pct || done == of {
                last_pct = pct;
                progress(Progress::Convert { track: job.name.into(), done, of });
            }
        })?;
    }
    progress(Progress::Call);

    let frames_of = |name: &str| jobs.iter().find(|j| j.name == name).map_or(0, |j| j.frames);
    let longest = jobs.iter().map(|j| j.frames).max().unwrap_or(0);
    // o sidecar da chamada descreve o que ficou de fato (inclusive depois de reparo)
    sidecar.state = State::Complete;
    if let Some(m) = sidecar.mic.as_mut() {
        m.samples = frames_of("mic");
    }
    if let Some(m) = sidecar.sys.as_mut() {
        m.samples = frames_of("sys");
    }
    sidecar.duration_s = Some(longest as f64 / f64::from(recorder::SAMPLE_RATE));
    if sidecar.ended_at.is_none() {
        let newest = jobs.iter().filter_map(|j| std::fs::metadata(&j.wav).ok()?.modified().ok()).max();
        let when = newest.map_or_else(chrono::Local::now, chrono::DateTime::<chrono::Local>::from);
        sidecar.ended_at = Some(when.format("%Y-%m-%dT%H:%M:%S").to_string());
    }
    sidecar.write(call_dir)?;

    let (title, speakers) = intent.map_or((String::new(), None), |i| (i.title, i.expected_speakers));
    let rel = |name: &str| rel_dir.join(name).to_string_lossy().into_owned();
    let has = |name: &str| frames_of(name) > 0;
    let tx = lib.conn.transaction()?;
    tx.execute(
        "INSERT INTO calls (key, client_id, title, slug, started_at, duration_s, language, expected_speakers, dir,
                            mic_path, sys_path, created_at, transcription_state)
         VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending')",
        params![
            key,
            client_id,
            title,
            sidecar.started_at,
            (longest as f64 / f64::from(recorder::SAMPLE_RATE)).round() as i64,
            language,
            speakers,
            rel_dir.to_string_lossy(),
            has("mic").then(|| rel(MIC_FLAC)),
            has("sys").then(|| rel(SYS_FLAC)),
            db::now(),
        ],
    )?;
    let call_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(call_id)
}

/// Gravação que sobrou em disco: queda (`state = recording`) ou parada limpa cuja finalização falhou /
/// não terminou (`state = complete`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Orphan {
    pub key: String,
    pub state: recorder::State,
    pub started_at: String,
    /// Estimada pelo tamanho dos WAVs (queda) ou pelo sidecar (`complete`).
    pub duration_s: f64,
    pub size_bytes: u64,
    pub mic_device: Option<String>,
    pub sys_device: Option<String>,
    /// O que o usuário tinha escolhido (alvo/título...), se o sidecar chegou a guardar.
    pub intent: Option<Intent>,
}

/// Varre `<dados>/recording/*/recording.json`, **mais recentes primeiro** (`started_at` decrescente).
/// `exclude` = chave da gravação ativa agora (não é órfã). Pastas **em uso** (travadas por uma gravação
/// ativa ou por um `finalize`/`recover` em andamento) também ficam de fora. Pastas sem sidecar legível
/// (ou com nome que não é uma chave) são ignoradas.
pub fn scan_orphans(app: &App, exclude: Option<&str>) -> Result<Vec<Orphan>> {
    let root = recording_root(&app.data_dir);
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let key = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() || exclude == Some(key.as_str()) || check_key(&key).is_err() {
            continue;
        }
        let dir = entry.path();
        if is_locked(&dir) {
            continue; // gravando ou finalizando agora, não é órfã
        }
        let Ok(sc) = Sidecar::read(&dir) else {
            eprintln!("recording: ignoring {} (no readable {SIDECAR_FILE})", dir.display());
            continue;
        };
        let (mut size_bytes, mut wav_samples) = (0u64, 0u64);
        for f in std::fs::read_dir(&dir)?.flatten() {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            size_bytes += len;
            let name = f.file_name();
            if name == MIC_WAV || name == SYS_WAV {
                wav_samples = wav_samples.max(len.saturating_sub(recorder::wav::HEADER_LEN) / 2);
            }
        }
        let from_files = wav_samples as f64 / f64::from(recorder::SAMPLE_RATE);
        let duration_s = match sc.state {
            State::Recording => from_files,
            State::Complete => sc.duration_s.unwrap_or(from_files),
        };
        let device = |m: &Option<recorder::StreamMeta>| {
            m.as_ref().map(|m| if m.description.is_empty() { m.device.clone() } else { m.description.clone() })
        };
        out.push(Orphan {
            intent: intent_of(&sc),
            mic_device: device(&sc.mic),
            sys_device: device(&sc.sys),
            key,
            state: sc.state,
            started_at: sc.started_at,
            duration_s,
            size_bytes,
        });
    }
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| b.key.cmp(&a.key)));
    Ok(out)
}

/// Alvos (`Intent`) das gravações em uso agora: a ativa, as sendo finalizadas ou recuperadas (pasta
/// travada). Quem apaga uma biblioteca ou um cliente recusa se algum alvo aponta para ele. Pastas sem
/// sidecar legível ou sem `Intent` ficam de fora (não dizem para onde vão).
pub fn busy_targets(app: &App) -> Result<Vec<Intent>> {
    let entries = match std::fs::read_dir(recording_root(&app.data_dir)) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() || check_key(&entry.file_name().to_string_lossy()).is_err() || !is_locked(&dir) {
            continue;
        }
        if let Some(intent) = Sidecar::read(&dir).ok().and_then(|sc| intent_of(&sc)) {
            out.push(intent);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ chaves e pastas

pub fn recording_root(data_dir: &Path) -> PathBuf {
    data_dir.join(RECORDING_DIR)
}

pub fn recording_dir(data_dir: &Path, key: &str) -> PathBuf {
    recording_root(data_dir).join(key)
}

/// `call_YYYY-MM-DD_HH-MM-SS` da hora local agora (mesmo formato das chamadas importadas).
pub fn new_key() -> String {
    chrono::Local::now().format("call_%Y-%m-%d_%H-%M-%S").to_string()
}

// ------------------------------------------------------------------ "último usado"

/// Padrões do formulário de gravação.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LastUsed {
    pub library_id: Option<i64>,
    pub client_id: Option<i64>,
    pub mic: StreamChoice,
    pub sys: StreamChoice,
}

/// Lê de `settings`. Biblioteca/cliente que não existem mais voltam como `None`.
pub fn last_used(app: &App) -> Result<LastUsed> {
    let num = |k: &str| -> Result<Option<i64>> { Ok(app.setting(k)?.and_then(|v| v.parse().ok())) };
    let choice = |k: &str| -> Result<StreamChoice> {
        Ok(app.setting(k)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
    };
    let library_id = num(keys::LIBRARY)?.filter(|id| app.library_row(*id).is_ok());
    let client_id = match (library_id, num(keys::CLIENT)?) {
        (Some(l), Some(c)) => app.open_library(l).ok().and_then(|lib| lib.find_client(&c.to_string()).ok()).map(|c| c.id),
        _ => None,
    };
    Ok(LastUsed { library_id, client_id, mic: choice(keys::MIC)?, sys: choice(keys::SYS)? })
}

pub fn save_last_used(app: &App, last: &LastUsed) -> Result<()> {
    let opt = |v: Option<i64>| v.map(|n| n.to_string());
    app.set_setting(keys::LIBRARY, opt(last.library_id).as_deref())?;
    app.set_setting(keys::CLIENT, opt(last.client_id).as_deref())?;
    app.set_setting(keys::MIC, Some(&serde_json::to_string(&last.mic)?))?;
    app.set_setting(keys::SYS, Some(&serde_json::to_string(&last.sys)?))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_second_gets_a_suffix_in_the_folder_and_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let (k1, d1) = create_unique_dir(tmp.path(), "call_2026-10-02_09-00-00").unwrap();
        let (k2, d2) = create_unique_dir(tmp.path(), "call_2026-10-02_09-00-00").unwrap();
        let (k3, _) = create_unique_dir(tmp.path(), "call_2026-10-02_09-00-00").unwrap();
        assert_eq!((k1.as_str(), k2.as_str(), k3.as_str()), ("call_2026-10-02_09-00-00", "call_2026-10-02_09-00-00_2", "call_2026-10-02_09-00-00_3"));
        assert!(d1.is_dir() && d2.is_dir());
        assert!(check_key(&k2).is_ok());
    }

    #[test]
    fn check_key_accepts_only_safe_names() {
        assert!(check_key("call_2026-10-02_09-00-00").is_ok());
        for bad in ["", "call_", "x", "call_/x", "call_..", "call_a/b", "call_a b", "call_\\x"] {
            // `call_` sozinho é aceito (não é caminho); o resto não
            assert_eq!(check_key(bad).is_ok(), bad == "call_", "{bad:?}");
        }
    }
}
