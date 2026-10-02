//! Sessão de gravação (mic + sys → `mic.wav`/`sys.wav` + `recording.json`) e monitor de níveis.
//!
//! Arquitetura:
//! - por trilha, uma **thread de captura** que chama `backend.open(..)` *dentro dela* (o fluxo não é
//!   `Send`) e faz `read` em fragmentos de `fragment_ms`; cada fragmento vai para um **canal limitado**
//!   → **thread de escritora** (`WavWriter`) que faz `sync` ~1 s e `patch_header` ~5 s. Nunca I/O na
//!   thread de captura.
//! - `Session::start` só retorna depois de cada thread reportar o resultado do `open` (por canal):
//!   `DeviceNotFound`/`OpenFailed` chegam de forma **síncrona** ao chamador, sem deixar arquivos.
//! - `read` com erro → registra um `Cut`, tenta religar a cada ~1 s (`open` de novo, mesmo nome; se o
//!   nome sumiu, o padrão da categoria) preenchendo a lacuna com silêncio; reflete em `status()`.
//! - 1ª leitura de cada trilha → `first_sample_unix_ms` (ver `CaptureStream::first_sample_time`) e
//!   reescreve o sidecar (ainda `recording`).
//! - `Drop` sem `stop()` = **aborta como uma queda**: threads param, WAVs/sidecar ficam como estão
//!   (`state: "recording"`), recuperáveis.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::backend::{CHANNELS, CaptureBackend, CaptureStream, DeviceInfo, DeviceKind, SAMPLE_RATE};
use crate::levels::{LevelMeter, Levels, StreamLevel};
use crate::sidecar::{Cut, MIC_WAV, SCHEMA, SIDECAR_FILE, SYS_WAV, Sidecar, State, StreamMeta};
use crate::wav::WavWriter;
use crate::{Error, Result};

/// Qual dispositivo usar numa trilha. JSON: `"default"` | `"off"` | `{"named": "<nome do dispositivo>"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamChoice {
    /// Padrão do sistema (fonte padrão / monitor do sink padrão).
    #[default]
    Default,
    /// Não gravar esta trilha (ex.: reunião presencial só com mic).
    Off,
    Named(String),
}

#[derive(Debug, Clone)]
pub struct StartOptions {
    /// Pasta da gravação (`<dados>/recording/<key>`); criada se não existir; não pode já ter `mic.wav`/`sys.wav`.
    pub dir: PathBuf,
    /// `call_YYYY-MM-DD_HH-MM-SS`.
    pub key: String,
    pub mic: StreamChoice,
    pub sys: StreamChoice,
    pub app_version: String,
    /// Vai para `Sidecar.extra`.
    pub extra: serde_json::Value,
    /// Fragmento de captura em ms (20–100; padrão 100 → 1600 amostras).
    pub fragment_ms: u32,
}

impl StartOptions {
    pub fn new(dir: impl Into<PathBuf>, key: impl Into<String>) -> Self {
        StartOptions {
            dir: dir.into(),
            key: key.into(),
            mic: StreamChoice::Default,
            sys: StreamChoice::Default,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            extra: serde_json::Value::Null,
            fragment_ms: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamStatus {
    /// Nome (id) e rótulo do dispositivo em uso.
    pub device: String,
    pub description: String,
    pub is_monitor: bool,
    /// `false` enquanto desconectado e religando.
    pub alive: bool,
    pub samples: u64,
    pub cuts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStatus {
    /// Tempo decorrido (relógio de parede desde o `start`).
    pub elapsed_s: f64,
    pub mic: Option<StreamStatus>,
    pub sys: Option<StreamStatus>,
    /// Soma dos cortes das duas trilhas.
    pub cuts: u32,
}

/// Fragmentos que cabem no canal entre a captura e o escritor (256 × 100 ms ≈ 25 s de folga se o disco travar).
const CHANNEL_FRAGMENTS: usize = 256;
/// Cadência de `sync_data` e do patch do cabeçalho (o patch só vale para o que o `sync` tornou durável).
const SYNC_EVERY: Duration = Duration::from_secs(1);
const PATCH_EVERY: Duration = Duration::from_secs(5);
/// Esperas entre tentativas de religar um fluxo que morreu (a última se repete).
const RECONNECT_BACKOFF_MS: [u64; 4] = [100, 250, 500, 1000];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix_ms(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn local_now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn fragment_samples(fragment_ms: u32) -> usize {
    (u64::from(SAMPLE_RATE) * u64::from(fragment_ms) / 1000) as usize
}

fn samples_to_ms(n: u64) -> u32 {
    (n * 1000 / u64::from(SAMPLE_RATE)).min(u64::from(u32::MAX)) as u32
}

// ------------------------------------------------------------------ trilha (estado compartilhado)

/// Uma trilha em andamento: o que a thread de captura publica e o que `status`/`levels`/sidecar leem.
struct Track {
    kind: DeviceKind,
    /// `None` = padrão do sistema (segue o padrão ao religar); `Some` = dispositivo pedido por nome.
    request: Option<String>,
    fragment_ms: u32,
    alive: AtomicBool,
    /// Amostras produzidas pela captura, inclusive o silêncio que preenche cortes.
    samples: AtomicU64,
    /// Há mudança de metadados ainda não gravada no sidecar (a thread escritora é quem grava).
    dirty: AtomicBool,
    meter: Mutex<LevelMeter>,
    meta: Mutex<StreamMeta>,
}

impl Track {
    fn new(kind: DeviceKind, choice: &StreamChoice, file: &str, fragment_ms: u32) -> Option<Arc<Track>> {
        let request = match choice {
            StreamChoice::Off => return None,
            StreamChoice::Default => None,
            StreamChoice::Named(n) => Some(n.clone()),
        };
        let meta = StreamMeta {
            file: file.to_string(),
            device: request.clone().unwrap_or_else(|| "default".into()),
            description: String::new(),
            is_monitor: kind == DeviceKind::Monitor,
            first_sample_unix_ms: None,
            first_read_unix_ms: None,
            latency_ms: None,
            fragment_ms,
            samples: 0,
            cuts: vec![],
            reconnects: 0,
        };
        Some(Arc::new(Track {
            kind,
            request,
            fragment_ms,
            alive: AtomicBool::new(true),
            samples: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            meter: Mutex::new(LevelMeter::default()),
            meta: Mutex::new(meta),
        }))
    }

    fn samples(&self) -> u64 {
        self.samples.load(Ordering::Relaxed)
    }

    fn snapshot(&self) -> StreamMeta {
        let mut m = lock(&self.meta).clone();
        m.samples = self.samples();
        m
    }

    fn status(&self) -> StreamStatus {
        let m = lock(&self.meta);
        StreamStatus {
            device: m.device.clone(),
            description: m.description.clone(),
            is_monitor: m.is_monitor,
            alive: self.alive.load(Ordering::Relaxed),
            samples: self.samples(),
            cuts: m.cuts.len() as u32,
        }
    }

    fn level(&self) -> StreamLevel {
        // `take` sempre diz `alive: true`; quem sabe é a trilha
        StreamLevel { alive: self.alive.load(Ordering::Relaxed), ..lock(&self.meter).take() }
    }

    fn set_device(&self, info: &DeviceInfo) {
        let mut m = lock(&self.meta);
        m.device = info.name.clone();
        m.description = info.description.clone();
        m.is_monitor = info.is_monitor;
    }
}

fn levels_of(mic: &Option<Arc<Track>>, sys: &Option<Arc<Track>>) -> Levels {
    Levels { mic: mic.as_ref().map(|t| t.level()), sys: sys.as_ref().map(|t| t.level()) }
}

// ------------------------------------------------------------------ thread de captura

struct CaptureCtx {
    backend: Arc<dyn CaptureBackend>,
    track: Arc<Track>,
    stop: Arc<AtomicBool>,
    /// `None` no `Monitor` (não grava nada).
    tx: Option<SyncSender<Vec<i16>>>,
}

impl CaptureCtx {
    fn open(&self) -> Result<Box<dyn CaptureStream>> {
        self.backend.open(self.track.kind, self.track.request.as_deref(), self.track.fragment_ms)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Dorme `ms` acordando cedo se a sessão parar.
    fn nap(&self, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        while !self.stopped() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&self, chunk: Vec<i16>) {
        if let Some(tx) = &self.tx {
            // erro = escritor encerrou (só acontece no desligamento); nada a fazer
            let _ = tx.send(chunk);
        }
    }

    /// Preenche `n` amostras de silêncio digital (corte): mantém o índice da amostra igual ao tempo.
    fn send_silence(&self, mut n: u64) {
        let frag = fragment_samples(self.track.fragment_ms).max(1) as u64;
        while n > 0 {
            let take = n.min(frag);
            self.send(vec![0i16; take as usize]);
            self.track.samples.fetch_add(take, Ordering::Relaxed);
            n -= take;
        }
    }

    /// Religa depois de uma falha: tenta o dispositivo pedido; se ele sumiu, o padrão da categoria.
    /// `None` = a sessão parou antes de conseguir.
    fn reconnect(&self) -> Option<Box<dyn CaptureStream>> {
        for attempt in 0.. {
            self.nap(RECONNECT_BACKOFF_MS[attempt.min(RECONNECT_BACKOFF_MS.len() - 1)]);
            if self.stopped() {
                return None;
            }
            let opened = match self.open() {
                Err(Error::DeviceNotFound(_)) if self.track.request.is_some() => {
                    self.backend.open(self.track.kind, None, self.track.fragment_ms)
                }
                other => other,
            };
            if let Ok(stream) = opened {
                return Some(stream);
            }
        }
        None
    }
}

/// Instante da amostra 0 da trilha no relógio monotônico: base de todos os alinhamentos.
struct TrackClock(Instant);

impl TrackClock {
    /// Quantas amostras já deveriam existir no instante `t`.
    fn expected_samples(&self, t: Instant) -> u64 {
        (t.saturating_duration_since(self.0).as_secs_f64() * f64::from(SAMPLE_RATE)).round() as u64
    }
}

/// Primeira leitura de um fluxo (a 1ª do dispositivo ou a de depois de religar): estima quando a
/// amostra 0 do fragmento foi capturada (`leitura − latência − fragmento`, spike §4).
fn first_read(stream: &dyn CaptureStream, got: usize) -> (Instant, SystemTime, Option<Duration>) {
    let (now, sys_now) = (Instant::now(), SystemTime::now());
    let latency = stream.latency();
    let frag = Duration::from_secs_f64(got as f64 / f64::from(SAMPLE_RATE));
    let back = latency.unwrap_or_default() + frag;
    (now.checked_sub(back).unwrap_or(now), sys_now.checked_sub(back).unwrap_or(sys_now), latency)
}

fn capture_thread(ctx: CaptureCtx, opened: mpsc::Sender<Result<()>>) {
    let track = ctx.track.clone();
    let mut stream = match ctx.open() {
        Ok(s) => {
            track.set_device(s.device());
            let _ = opened.send(Ok(()));
            Some(s)
        }
        Err(e) => {
            let _ = opened.send(Err(e));
            return;
        }
    };
    let frag = fragment_samples(track.fragment_ms).max(1);
    let mut buf = vec![0i16; frag];
    let mut clock: Option<TrackClock> = None;
    let mut fresh = true; // a próxima leitura é a primeira do fluxo atual
    let mut cut_started: Option<Instant> = None; // desconectado desde...

    while !ctx.stopped() {
        let Some(s) = stream.as_mut() else {
            // desconectado: religa (bloqueia até conseguir ou parar)
            stream = ctx.reconnect();
            fresh = true;
            continue;
        };
        if let Err(_e) = s.read(&mut buf) {
            if ctx.stopped() {
                break;
            }
            // o fluxo morreu: registra o corte e passa a religar
            stream = None;
            track.alive.store(false, Ordering::SeqCst);
            cut_started = Some(Instant::now());
            let mut m = lock(&track.meta);
            m.cuts.push(Cut {
                at_sample: track.samples(),
                at_unix_ms: unix_ms(SystemTime::now()),
                gap_ms: 0,
                reason: "read_error".into(),
                reconnected: false,
            });
            drop(m);
            track.dirty.store(true, Ordering::SeqCst);
            continue;
        }
        if fresh {
            fresh = false;
            let (start, start_sys, latency) = first_read(s.as_ref(), frag);
            match &clock {
                None => {
                    // 1ª leitura da trilha: define o relógio e o alinhamento (o backend pode saber melhor)
                    let first_sys = s.first_sample_time().unwrap_or(start_sys);
                    let skew = SystemTime::now().duration_since(first_sys).unwrap_or_default();
                    let first = Instant::now().checked_sub(skew).unwrap_or(start);
                    clock = Some(TrackClock(first));
                    let mut m = lock(&track.meta);
                    m.first_sample_unix_ms = Some(unix_ms(first_sys));
                    m.first_read_unix_ms = Some(unix_ms(SystemTime::now()));
                    m.latency_ms = latency.map(|d| d.as_millis() as u32);
                }
                Some(c) => {
                    // religou: o silêncio cobre o relógio entre o corte e o início deste fragmento
                    let gap = c.expected_samples(start).saturating_sub(track.samples());
                    ctx.send_silence(gap);
                    let info = s.device().clone();
                    let mut m = lock(&track.meta);
                    let changed = m.device != info.name;
                    if let Some(cut) = m.cuts.last_mut() {
                        cut.gap_ms = samples_to_ms(gap);
                        cut.reconnected = true;
                        if changed {
                            cut.reason = "device_changed".into();
                        }
                    }
                    m.reconnects += 1;
                    m.device = info.name;
                    m.description = info.description;
                    m.is_monitor = info.is_monitor;
                    drop(m);
                    cut_started = None;
                    track.alive.store(true, Ordering::SeqCst);
                }
            }
            track.dirty.store(true, Ordering::SeqCst);
        }
        lock(&track.meter).push(&buf);
        ctx.send(buf.clone());
        track.samples.fetch_add(buf.len() as u64, Ordering::Relaxed);
    }

    // parou desconectado: a trilha continua do tamanho do tempo decorrido
    if let (Some(_), Some(c)) = (cut_started, &clock) {
        let gap = c.expected_samples(Instant::now()).saturating_sub(track.samples());
        ctx.send_silence(gap);
        if let Some(cut) = lock(&track.meta).cuts.last_mut() {
            cut.gap_ms = samples_to_ms(gap);
        }
    }
}

// ------------------------------------------------------------------ thread escritora

fn writer_thread(
    rx: mpsc::Receiver<Vec<i16>>,
    mut wav: WavWriter,
    track: Arc<Track>,
    shared: Arc<Shared>,
) -> Result<u64> {
    let (mut last_sync, mut last_patch) = (Instant::now(), Instant::now());
    let mut failed: Option<Error> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(chunk) => {
                // depois de uma falha de disco continua esvaziando o canal (a captura nunca trava)
                if failed.is_none()
                    && let Err(e) = wav.write(&chunk)
                {
                    failed = Some(e);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if failed.is_some() {
            continue;
        }
        let step = (|| -> Result<()> {
            if last_sync.elapsed() >= SYNC_EVERY {
                wav.sync()?;
                last_sync = Instant::now();
            }
            // o patch vem sempre depois do sync: o cabeçalho nunca promete mais do que está em disco
            if last_patch.elapsed() >= PATCH_EVERY {
                wav.patch_header()?;
                last_patch = Instant::now();
                track.dirty.store(true, Ordering::SeqCst);
            }
            Ok(())
        })();
        if let Err(e) = step {
            failed = Some(e);
            continue;
        }
        if track.dirty.swap(false, Ordering::SeqCst) {
            let _ = shared.persist_with(|_| {});
        }
    }
    if shared.abort.load(Ordering::SeqCst) {
        // queda simulada: `Drop` do escritor entrega o buffer ao SO, sem sync nem patch
        return Err(Error::Session("aborted".into()));
    }
    match failed {
        Some(e) => Err(e),
        None => wav.finish(),
    }
}

// ------------------------------------------------------------------ sessão

struct Shared {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    abort: AtomicBool,
    /// Sidecar em memória (guarda `extra`); toda gravação em disco passa por este *mutex*, porque o
    /// arquivo temporário é sempre o mesmo.
    sidecar: Mutex<Sidecar>,
    mic: Option<Arc<Track>>,
    sys: Option<Arc<Track>>,
}

impl Shared {
    fn tracks(&self) -> impl Iterator<Item = &Arc<Track>> {
        self.mic.iter().chain(self.sys.iter())
    }

    fn max_samples(&self) -> u64 {
        self.tracks().map(|t| t.samples()).max().unwrap_or(0)
    }

    /// Atualiza trilhas/duração a partir do estado vivo, aplica `f` e grava de forma atômica.
    fn persist_with(&self, f: impl FnOnce(&mut Sidecar)) -> Result<Sidecar> {
        let mut sc = lock(&self.sidecar);
        sc.mic = self.mic.as_ref().map(|t| t.snapshot());
        sc.sys = self.sys.as_ref().map(|t| t.snapshot());
        sc.duration_s = Some(self.max_samples() as f64 / f64::from(SAMPLE_RATE));
        f(&mut sc);
        sc.write(&self.dir)?;
        Ok(sc.clone())
    }
}

struct TrackThreads {
    track: Arc<Track>,
    capture: JoinHandle<()>,
    writer: JoinHandle<Result<u64>>,
}

pub struct Session {
    shared: Arc<Shared>,
    threads: Vec<TrackThreads>,
    started: Instant,
    started_at: String,
    key: String,
    finished: bool,
}

impl Session {
    /// Abre os dispositivos, cria `mic.wav`/`sys.wav` (as trilhas não `Off`) e `recording.json`
    /// (`state: "recording"`, escrita atômica **antes** de abrir os fluxos) e dispara as threads.
    /// Erros síncronos: `DeviceNotFound`, `OpenFailed`, `BackendUnavailable`, `Session` (nada para gravar
    /// — as duas `Off` —, pasta já usada) e `Io`. Em erro não sobra nada que a sessão tenha criado
    /// (a pasta só é removida se foi criada aqui).
    pub fn start(backend: Arc<dyn CaptureBackend>, opts: StartOptions) -> Result<Session> {
        if opts.mic == StreamChoice::Off && opts.sys == StreamChoice::Off {
            return Err(Error::Session("nothing to record: mic and sys are both off".into()));
        }
        if opts.key.is_empty() {
            return Err(Error::Session("empty recording key".into()));
        }
        for f in [MIC_WAV, SYS_WAV, SIDECAR_FILE] {
            if opts.dir.join(f).exists() {
                return Err(Error::Session(format!("recording folder already in use: {}", opts.dir.display())));
            }
        }
        let dir_existed = opts.dir.exists();
        std::fs::create_dir_all(&opts.dir)?;
        let result = Self::start_in(backend, &opts);
        if result.is_err() {
            if dir_existed {
                for f in [MIC_WAV, SYS_WAV, SIDECAR_FILE, "recording.json.part"] {
                    let _ = std::fs::remove_file(opts.dir.join(f));
                }
            } else {
                let _ = std::fs::remove_dir_all(&opts.dir);
            }
        }
        result
    }

    fn start_in(backend: Arc<dyn CaptureBackend>, opts: &StartOptions) -> Result<Session> {
        let fragment_ms = opts.fragment_ms.clamp(20, 100);
        let mic = Track::new(DeviceKind::Mic, &opts.mic, MIC_WAV, fragment_ms);
        let sys = Track::new(DeviceKind::Monitor, &opts.sys, SYS_WAV, fragment_ms);
        let started_at = local_now();
        let base = Sidecar {
            schema: SCHEMA,
            state: State::Recording,
            key: opts.key.clone(),
            app_version: opts.app_version.clone(),
            started_at: started_at.clone(),
            started_unix_ms: unix_ms(SystemTime::now()),
            ended_at: None,
            duration_s: Some(0.0),
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            format: "s16le".into(),
            mic: None,
            sys: None,
            extra: opts.extra.clone(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            dir: opts.dir.clone(),
            stop: stop.clone(),
            abort: AtomicBool::new(false),
            sidecar: Mutex::new(base),
            mic,
            sys,
        });
        // arquivos e sidecar primeiro: se algo falhar daqui em diante, `start` limpa
        let mut writers = Vec::new();
        for t in shared.tracks() {
            let file = lock(&t.meta).file.clone();
            writers.push((t.clone(), WavWriter::create(&opts.dir.join(file))?));
        }
        shared.persist_with(|_| {})?;

        let started = Instant::now();
        let mut session = Session { shared: shared.clone(), threads: Vec::new(), started, started_at, key: opts.key.clone(), finished: false };
        let mut opened = Vec::new();
        for (track, wav) in writers {
            let (tx, rx) = mpsc::sync_channel(CHANNEL_FRAGMENTS);
            let (otx, orx) = mpsc::channel();
            let writer = {
                let (t, sh) = (track.clone(), shared.clone());
                std::thread::Builder::new().name("rec-writer".into()).spawn(move || writer_thread(rx, wav, t, sh))?
            };
            let ctx = CaptureCtx { backend: backend.clone(), track: track.clone(), stop: stop.clone(), tx: Some(tx) };
            let capture = std::thread::Builder::new().name("rec-capture".into()).spawn(move || capture_thread(ctx, otx))?;
            session.threads.push(TrackThreads { track, capture, writer });
            opened.push(orx);
        }
        // cada thread abre o seu fluxo e avisa; o 1º erro (mic antes de sys) volta ao chamador
        let mut first_err = None;
        for orx in opened {
            match orx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    first_err.get_or_insert(e);
                }
                Err(_) => {
                    first_err.get_or_insert(Error::Capture("capture thread died while opening".into()));
                }
            }
        }
        if let Some(e) = first_err {
            session.shutdown(true);
            return Err(e);
        }
        shared.persist_with(|_| {})?; // agora com o dispositivo efetivo
        Ok(session)
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn dir(&self) -> &Path {
        &self.shared.dir
    }

    /// Hora local de início (`YYYY-MM-DDTHH:MM:SS`).
    pub fn started_at(&self) -> &str {
        &self.started_at
    }

    pub fn status(&self) -> SessionStatus {
        let (mic, sys) = (self.shared.mic.as_ref().map(|t| t.status()), self.shared.sys.as_ref().map(|t| t.status()));
        let cuts = mic.iter().chain(sys.iter()).map(|s| s.cuts).sum();
        SessionStatus { elapsed_s: self.started.elapsed().as_secs_f64(), mic, sys, cuts }
    }

    /// Pico/RMS desde a última chamada. **Um único leitor** (o ticker do shell, ~10 Hz).
    pub fn levels(&self) -> Levels {
        levels_of(&self.shared.mic, &self.shared.sys)
    }

    /// Troca `Sidecar.extra` (reescreve o sidecar de forma atômica). Usado para editar título/alvo
    /// durante a gravação.
    pub fn update_extra(&self, extra: serde_json::Value) -> Result<()> {
        self.shared.persist_with(|sc| sc.extra = extra).map(|_| ())
    }

    /// Para as threads e espera: `abort` = queda (o escritor só entrega o buffer ao SO). Devolve o
    /// resultado de cada escritor. Idempotente.
    fn shutdown(&mut self, abort: bool) -> Vec<Result<u64>> {
        self.shared.abort.store(abort, Ordering::SeqCst);
        self.shared.stop.store(true, Ordering::SeqCst);
        let mut results = Vec::new();
        // as threads de captura primeiro: ao soltarem os remetentes, os escritores esvaziam o canal e acabam
        for t in std::mem::take(&mut self.threads) {
            let _ = t.capture.join();
            results.push(t.writer.join().unwrap_or_else(|_| Err(Error::Capture("writer thread panicked".into()))));
        }
        results
    }

    /// Para as threads (descarrega o que está nos canais), `finish()` dos WAVs (patch final + fsync),
    /// reescreve o sidecar com `state: "complete"`, duração e amostras, e o devolve. Rápido (≲ 1 s).
    /// Se um escritor falhou (disco cheio...) devolve o erro e o sidecar continua `recording`.
    pub fn stop(mut self) -> Result<Sidecar> {
        let tracks: Vec<Arc<Track>> = self.threads.iter().map(|t| t.track.clone()).collect();
        let results = self.shutdown(false);
        self.finished = true;
        let mut finals = Vec::new();
        for (track, r) in tracks.into_iter().zip(results) {
            finals.push((track, r?));
        }
        for (track, n) in &finals {
            track.samples.store(*n, Ordering::Relaxed);
        }
        self.shared.persist_with(|sc| {
            sc.state = State::Complete;
            sc.ended_at = Some(local_now());
        })
    }
}

impl Drop for Session {
    /// Sem `stop` = queda: as threads param, os WAVs ficam como estão (sem patch do cabeçalho) e o
    /// sidecar continua `recording`. Recuperável com `repair_wav`.
    fn drop(&mut self) {
        if !self.finished {
            self.shutdown(true);
        }
    }
}

/// Abre as trilhas só para medir níveis (formulário de gravação: medidores dos seletores). Não grava
/// nada. Parar = `stop()` ou `drop`. Falha de abertura numa trilha: `start` devolve o erro.
pub struct Monitor {
    mic: Option<Arc<Track>>,
    sys: Option<Arc<Track>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(backend: Arc<dyn CaptureBackend>, mic: &StreamChoice, sys: &StreamChoice) -> Result<Monitor> {
        let mut m = Monitor {
            mic: Track::new(DeviceKind::Mic, mic, MIC_WAV, 50),
            sys: Track::new(DeviceKind::Monitor, sys, SYS_WAV, 50),
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        let mut opened = Vec::new();
        for track in m.mic.iter().chain(m.sys.iter()).cloned().collect::<Vec<_>>() {
            let (otx, orx) = mpsc::channel();
            let ctx = CaptureCtx { backend: backend.clone(), track, stop: m.stop.clone(), tx: None };
            m.threads.push(std::thread::Builder::new().name("rec-monitor".into()).spawn(move || capture_thread(ctx, otx))?);
            opened.push(orx);
        }
        let mut first_err = None;
        for orx in opened {
            match orx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    first_err.get_or_insert(e);
                }
                Err(_) => {
                    first_err.get_or_insert(Error::Capture("capture thread died while opening".into()));
                }
            }
        }
        match first_err {
            Some(e) => Err(e), // `Drop` do monitor para as outras threads
            None => Ok(m),
        }
    }

    /// Mesma semântica de `Session::levels` (um único leitor).
    pub fn levels(&self) -> Levels {
        levels_of(&self.mic, &self.sys)
    }

    pub fn stop(self) {}
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
