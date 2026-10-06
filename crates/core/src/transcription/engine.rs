//! Quem executa um pedido do protocolo. `FakeEngine` roda em memória (testes do núcleo, sem processo);
//! `ProcessEngine` fala com o worker Python (real, ou `worker.py --fake` com `FAKE_WORKER_ENV`).
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use super::protocol::{self, FromWorker, ToWorker, TurnMsg, WordTime};
use crate::{Error, Result};

/// Resposta do chamador a cada evento recebido durante um pedido.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flow {
    Continue,
    /// Envia `cancel` ao worker e espera o `cancelled` (sem prazo curto: ver SIGKILL em `ProcessEngine`).
    Cancel,
}

/// Como terminou um pedido.
#[derive(Debug, Clone, PartialEq)]
pub enum Terminal {
    /// A mensagem `result` completa.
    Result(FromWorker),
    Cancelled { segments: u64 },
}

pub trait Engine: Send {
    /// Executa UM pedido (`Transcribe`/`Diarize`/`Energy`). Entrega ao `on_event` cada `Progress`/`Segment`
    /// em ordem; o retorno do `on_event` pode pedir cancelamento. Erros: `worker_crashed` (processo caiu —
    /// o próximo `execute` reinicia), `worker_protocol`, ou o `error` do worker mapeado em `Error::Transcription`.
    fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal>;

    /// Encerra com `shutdown` (e SIGKILL do grupo se não sair em `kill_after_s`). Idempotente.
    fn shutdown(&mut self);
}

/// Duração (s) de um FLAC pelo bloco STREAMINFO (taxa de 20 bits + total de amostras de 36 bits).
pub fn flac_duration_s(path: &Path) -> Result<f64> {
    let mut head = [0u8; 42];
    std::fs::File::open(path)?.read_exact(&mut head).map_err(|_| Error::transcription("audio_decode", format!("{}: not a FLAC file", path.display())))?;
    if &head[..4] != b"fLaC" || head[4] & 0x7f != 0 {
        return Err(Error::transcription("audio_decode", format!("{}: no STREAMINFO", path.display())));
    }
    let v = u64::from_be_bytes(head[18..26].try_into().expect("8 bytes"));
    let (rate, total) = (v >> 44, v & 0xF_FFFF_FFFF);
    if rate == 0 {
        return Err(Error::transcription("audio_decode", "FLAC with zero sample rate"));
    }
    Ok(total as f64 / rate as f64)
}

/// Parte de `[start, end]` que sobra fora de `mute` (do primeiro ao último instante audível); `None` = tudo zerado.
/// Mesma regra do `worker.py --fake` (`fake_audible`).
fn audible(mute: &[(f64, f64)], start: f64, end: f64) -> Option<(f64, f64)> {
    let mut pieces = vec![(start, end)];
    for &(ms, me) in mute {
        pieces = pieces
            .into_iter()
            .flat_map(|(a, b)| if me <= a || ms >= b { vec![(a, b)] } else { [(a, ms.max(a)), (me.min(b), b)].into_iter().filter(|(x, y)| y > x).collect() })
            .collect();
    }
    Some((pieces.first()?.0, pieces.last()?.1))
}

fn request_id(req: &ToWorker) -> Option<&str> {
    match req {
        ToWorker::Transcribe { id, .. } | ToWorker::Diarize { id, .. } | ToWorker::Energy { id, .. } | ToWorker::Cancel { id } => Some(id),
        ToWorker::Shutdown => None,
    }
}

/// Motor de teste em memória: determinístico, sem áudio real (a duração vem do STREAMINFO do FLAC). Regra
/// idêntica à do `worker.py --fake` (contrato §9.3): ASR a cada 5 s com texto fixo, turnos de 15 s alternando,
/// energia constante (mic -25 dB se o nome do arquivo contém "mic", senão -20 dB).
#[derive(Debug, Default)]
pub struct FakeEngine {
    /// Pausa entre segmentos emitidos (ms), para testes de cancelamento.
    pub delay_ms: u64,
}

impl FakeEngine {
    pub fn new() -> FakeEngine {
        FakeEngine::default()
    }
}

impl Engine for FakeEngine {
    fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
        match req {
            ToWorker::Transcribe { id, audio, track, language, word_timestamps, start_s, mute, .. } => {
                let dur = flac_duration_s(Path::new(audio))?;
                let who = if track == "mic" { "eu" } else { "fala" };
                let first = (start_s / 5.0).ceil().max(0.0) as u64;
                let mut emitted = 0u64;
                let progress = |stage: &str, audio_s: Option<f64>| FromWorker::Progress {
                    id: id.clone(),
                    stage: stage.into(),
                    audio_s,
                    total_s: Some(dur),
                    done: None,
                    total: None,
                };
                if on_event(&progress("loading_model", None)) == Flow::Cancel {
                    return Ok(Terminal::Cancelled { segments: 0 });
                }
                for k in first.. {
                    let start = k as f64 * 5.0;
                    if start >= dur {
                        break;
                    }
                    // progresso parcial dentro da "janela", antes de ela terminar (como o worker Python)
                    if on_event(&progress("transcribe", Some((start + 2.25).min(dur)))) == Flow::Cancel {
                        return Ok(Terminal::Cancelled { segments: emitted });
                    }
                    if self.delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(self.delay_ms));
                    }
                    let end = (start + 4.5).min(dur);
                    // áudio zerado (cortes): sem fala, o VAD real não emite nada; só a parte que sobra fica
                    let Some((start, end)) = audible(mute, start, end) else {
                        if on_event(&progress("transcribe", Some(end))) == Flow::Cancel {
                            return Ok(Terminal::Cancelled { segments: emitted });
                        }
                        continue;
                    };
                    let text = format!("{who} trecho {k}");
                    let words = word_timestamps.then(|| {
                        let parts: Vec<&str> = text.split(' ').collect();
                        let n = parts.len() as f64;
                        parts
                            .iter()
                            .enumerate()
                            .map(|(i, w)| {
                                WordTime(start + i as f64 * (end - start) / n, start + (i + 1) as f64 * (end - start) / n, (*w).to_string())
                            })
                            .collect()
                    });
                    let seg = FromWorker::Segment { id: id.clone(), start, end, text, words };
                    let mut cancel = on_event(&seg) == Flow::Cancel;
                    emitted += 1;
                    cancel |= on_event(&progress("transcribe", Some(end))) == Flow::Cancel;
                    if cancel {
                        return Ok(Terminal::Cancelled { segments: emitted });
                    }
                }
                if on_event(&progress("transcribe", Some(dur))) == Flow::Cancel {
                    return Ok(Terminal::Cancelled { segments: emitted });
                }
                Ok(Terminal::Result(FromWorker::Result {
                    id: id.clone(),
                    segments: Some(emitted),
                    seconds: Some(dur),
                    language: Some(language.clone().unwrap_or_else(|| "pt".into())),
                    turns: None,
                    speakers: None,
                    step_ms: None,
                    db: None,
                    merge: None,
                }))
            }
            ToWorker::Diarize { id, audio, max_speakers, mute, .. } => {
                let dur = flac_duration_s(Path::new(audio))?;
                let modulo = if max_speakers.unwrap_or(0) >= 3 { 3 } else { 2 };
                let mut turns = Vec::new();
                let mut k = 0i64;
                while (k as f64) * 15.0 < dur {
                    // turno inteiro dentro de áudio zerado não existe; o resto fica só com a parte audível
                    if let Some((start, end)) = audible(mute, k as f64 * 15.0, ((k + 1) as f64 * 15.0).min(dur)) {
                        turns.push(TurnMsg { start, end, speaker: k % modulo });
                    }
                    k += 1;
                }
                for stage in ["diarize_segmentation", "diarize_embedding"] {
                    let ev = FromWorker::Progress { id: id.clone(), stage: stage.into(), audio_s: None, total_s: Some(dur), done: None, total: None };
                    if on_event(&ev) == Flow::Cancel {
                        return Ok(Terminal::Cancelled { segments: 0 });
                    }
                }
                let speakers = turns.iter().map(|t| t.speaker).collect::<std::collections::BTreeSet<_>>().len() as i64;
                Ok(Terminal::Result(FromWorker::Result {
                    id: id.clone(),
                    segments: None,
                    seconds: Some(dur),
                    language: None,
                    turns: Some(turns),
                    speakers: Some(speakers),
                    step_ms: None,
                    db: None,
                    merge: Some(serde_json::json!({ "raw": speakers, "final": speakers, "clusters": [] })),
                }))
            }
            ToWorker::Energy { id, audio, step_ms, mute } => {
                let dur = flac_duration_s(Path::new(audio))?;
                let step = f64::from((*step_ms).max(1)) / 1000.0;
                let n = (dur / step).ceil() as usize;
                let level = if Path::new(audio).file_name().is_some_and(|f| f.to_string_lossy().contains("mic")) { -25.0 } else { -20.0 };
                // o passo todo dentro de áudio zerado é o piso (-120 dB), como o RMS de zeros no worker real
                let db = (0..n).map(|i| if audible(mute, i as f64 * step, (i as f64 + 1.0) * step).is_none() { -120.0 } else { level }).collect();
                Ok(Terminal::Result(FromWorker::Result {
                    id: id.clone(),
                    segments: None,
                    seconds: Some(dur),
                    language: None,
                    turns: None,
                    speakers: None,
                    step_ms: Some(*step_ms),
                    db: Some(db),
                    merge: None,
                }))
            }
            ToWorker::Cancel { .. } | ToWorker::Shutdown => Err(Error::invalid("not a request")),
        }
    }

    fn shutdown(&mut self) {}
}

/// Como lançar o worker.
#[derive(Debug, Clone)]
pub struct WorkerLaunch {
    /// Interpretador (`<dados>/runtime/venv/bin/python`, ou o do `FAKE_WORKER_ENV`).
    pub python: PathBuf,
    /// `worker.py` extraído do binário para `<dados>/runtime/worker.py`.
    pub script: PathBuf,
    /// `--fake`
    pub fake: bool,
    /// `nice 19` + `ioprio` ocioso aplicados no filho (`pre_exec`).
    pub low_priority: bool,
    /// Segundos entre o `cancel` ignorado e o SIGKILL do grupo (padrão 120).
    pub kill_after_s: u64,
}

/// Linha de stdout do worker (ou fim do fluxo), lida por uma thread própria.
enum Wire {
    Line(String),
    Eof,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Wire>,
    hello: FromWorker,
}

/// Worker real em subprocesso: stdin/stdout em JSON-lines, stderr para um arquivo de log.
/// `PR_SET_PDEATHSIG(SIGKILL)` + conferência de `getppid`: spawnar a partir de uma thread longeva (o sinal
/// dispara quando a THREAD que fez o spawn termina, não só o processo).
pub struct ProcessEngine {
    pub launch: WorkerLaunch,
    proc: Option<Proc>,
}

impl std::fmt::Debug for ProcessEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessEngine").field("launch", &self.launch).field("pid", &self.pid()).finish()
    }
}

const LOG_MAX_BYTES: u64 = 1024 * 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
/// Intervalo sem mensagem do worker depois do qual o chamador é consultado mesmo assim.
const TICK: Duration = Duration::from_millis(250);

/// `stage` de um `Progress` SINTÉTICO que o `ProcessEngine` entrega ao `on_event` a cada `TICK` sem mensagem do
/// worker (só para consultar o `Flow`; quem consome ignora). Nunca vem do worker.
pub const TICK_STAGE: &str = "tick";

impl ProcessEngine {
    /// Lança o processo e valida o `hello` (`protocol == PROTOCOL`, senão `Error::Transcription("runtime_outdated")`).
    pub fn spawn(launch: WorkerLaunch) -> Result<ProcessEngine> {
        let mut e = ProcessEngine { launch, proc: None };
        e.start()?;
        Ok(e)
    }

    /// PID do worker vivo (testes e diagnóstico).
    pub fn pid(&self) -> Option<u32> {
        self.proc.as_ref().map(|p| p.child.id())
    }

    /// Mensagem `hello` do processo atual.
    pub fn hello(&self) -> Option<&FromWorker> {
        self.proc.as_ref().map(|p| &p.hello)
    }

    fn start(&mut self) -> Result<()> {
        let l = &self.launch;
        let log_path = l.script.parent().unwrap_or(Path::new(".")).join("worker.log");
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
        if log.metadata()?.len() > LOG_MAX_BYTES {
            log.set_len(0)?;
        }
        let mut cmd = Command::new(&l.python);
        cmd.arg(&l.script);
        if l.fake {
            cmd.arg("--fake");
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(log));
        super::host_env(&mut cmd);
        cmd.env("HF_HUB_OFFLINE", "1").env("PYTHONUNBUFFERED", "1").env("PYTHONIOENCODING", "utf-8");
        cmd.process_group(0);
        let parent = unsafe { libc::getpid() };
        let low_priority = l.low_priority;
        // SAFETY: só chamadas async-signal-safe (prctl/getppid/setpriority/syscall) entre o fork e o exec.
        unsafe {
            cmd.pre_exec(move || {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::getppid() != parent {
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                if low_priority {
                    libc::setpriority(libc::PRIO_PROCESS, 0, 19);
                    // ioprio_set(IOPRIO_WHO_PROCESS, 0, IOPRIO_CLASS_IDLE << 13)
                    libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| Error::transcription("runtime_missing", format!("{}: {e}", l.python.display())))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) if !l.trim().is_empty() => {
                        if tx.send(Wire::Line(l)).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let _ = tx.send(Wire::Eof);
        });
        let mut proc = Proc { child, stdin, rx, hello: FromWorker::Bye };
        let hello = match proc.rx.recv_timeout(HELLO_TIMEOUT) {
            Ok(Wire::Line(line)) => protocol::parse_line(&line),
            Ok(Wire::Eof) | Err(RecvTimeoutError::Disconnected) => Err(Error::transcription("worker_crashed", "worker exited before hello")),
            Err(RecvTimeoutError::Timeout) => Err(Error::transcription("worker_crashed", "no hello from worker in 60 s")),
        };
        match hello {
            Ok(h @ FromWorker::Hello { protocol: p, .. }) if p == protocol::PROTOCOL => {
                proc.hello = h;
                self.proc = Some(proc);
                Ok(())
            }
            Ok(FromWorker::Hello { protocol: p, .. }) => {
                kill_group(&mut proc.child);
                Err(Error::transcription("runtime_outdated", format!("worker protocol {p}, expected {}", protocol::PROTOCOL)))
            }
            Ok(other) => {
                kill_group(&mut proc.child);
                Err(Error::transcription("worker_protocol", format!("expected hello, got {other:?}")))
            }
            Err(e) => {
                kill_group(&mut proc.child);
                Err(e)
            }
        }
    }

    /// O chamador pediu para parar. `hard`: SIGKILL do grupo na hora (`Some(Cancelled)`); senão envia `cancel` e
    /// segue lendo até o `cancelled` (`None`).
    fn request_cancel(&mut self, id: &str, hard: bool, cancel_at: &mut Option<Instant>) -> Result<Option<Terminal>> {
        if hard {
            self.drop_proc();
            return Ok(Some(Terminal::Cancelled { segments: 0 }));
        }
        *cancel_at = Some(Instant::now());
        let proc = self.proc.as_mut().expect("alive");
        if send_line(proc, &ToWorker::Cancel { id: id.to_string() }).is_err() {
            self.drop_proc();
            return Err(Error::transcription("worker_crashed", "worker closed its input"));
        }
        Ok(None)
    }

    fn drop_proc(&mut self) {
        if let Some(mut p) = self.proc.take() {
            kill_group(&mut p.child);
        }
    }
}

/// SIGKILL no grupo de processos do worker e colheita do filho.
fn kill_group(child: &mut Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn send_line(proc: &mut Proc, msg: &ToWorker) -> std::io::Result<()> {
    let mut line = protocol::to_line(msg);
    line.push('\n');
    proc.stdin.write_all(line.as_bytes())?;
    proc.stdin.flush()
}

/// `error` do worker → erro do núcleo. `exception` (e o resto) vira `job_failed`.
fn worker_error(code: &str, detail: &str) -> Error {
    match code {
        "audio_decode" => Error::transcription("audio_decode", detail),
        "oom" => Error::transcription("oom", detail),
        "model_missing" => Error::transcription("models_missing", detail),
        _ => Error::transcription("job_failed", format!("{code}: {detail}")),
    }
}

impl Engine for ProcessEngine {
    fn execute(&mut self, req: &ToWorker, on_event: &mut dyn FnMut(&FromWorker) -> Flow) -> Result<Terminal> {
        let id = request_id(req).ok_or_else(|| Error::invalid("not a request"))?.to_string();
        if matches!(req, ToWorker::Cancel { .. }) {
            return Err(Error::invalid("not a request"));
        }
        if self.proc.is_none() {
            self.start()?;
        }
        let kill_after = Duration::from_secs(self.launch.kill_after_s);
        // diarize/energy não têm saída parcial gravada (são refeitos na retomada): cancelar = matar já.
        // transcribe é cooperativo: os segmentos já gravados valem, então espera o `cancelled`.
        let hard_cancel = matches!(req, ToWorker::Diarize { .. } | ToWorker::Energy { .. });
        let proc = self.proc.as_mut().expect("started");
        if send_line(proc, req).is_err() {
            self.drop_proc();
            return Err(Error::transcription("worker_crashed", "worker closed its input"));
        }
        let mut cancel_at: Option<Instant> = None;
        loop {
            let proc = self.proc.as_mut().expect("alive");
            let wire = match proc.rx.recv_timeout(TICK) {
                Ok(w) => Some(w),
                Err(RecvTimeoutError::Timeout) => {
                    if cancel_at.is_some_and(|t| t.elapsed() > kill_after) {
                        // o worker ignorou o cancel: o que chegou já foi gravado pelo chamador
                        self.drop_proc();
                        return Ok(Terminal::Cancelled { segments: 0 });
                    }
                    None
                }
                Err(RecvTimeoutError::Disconnected) => Some(Wire::Eof),
            };
            // sem mensagem: avisa o chamador mesmo assim (a fase de embeddings não emite nada por minutos)
            let Some(wire) = wire else {
                if cancel_at.is_none() {
                    let tick = FromWorker::Progress { id: id.clone(), stage: TICK_STAGE.into(), audio_s: None, total_s: None, done: None, total: None };
                    if on_event(&tick) == Flow::Cancel {
                        if let Some(done) = self.request_cancel(&id, hard_cancel, &mut cancel_at)? {
                            return Ok(done);
                        }
                    }
                }
                continue;
            };
            let proc = self.proc.as_mut().expect("alive");
            let line = match wire {
                Wire::Line(l) => l,
                Wire::Eof => {
                    let status = proc.child.wait().map(|s| s.to_string()).unwrap_or_default();
                    self.proc = None;
                    return Err(Error::transcription("worker_crashed", format!("worker exited ({status})")));
                }
            };
            let msg = match protocol::parse_line(&line) {
                Ok(m) => m,
                Err(e) => {
                    self.drop_proc();
                    return Err(e);
                }
            };
            match &msg {
                FromWorker::Progress { id: mid, .. } | FromWorker::Segment { id: mid, .. } if *mid == id => {
                    if on_event(&msg) == Flow::Cancel && cancel_at.is_none() {
                        if let Some(done) = self.request_cancel(&id, hard_cancel, &mut cancel_at)? {
                            return Ok(done);
                        }
                    }
                }
                FromWorker::Result { id: mid, .. } if *mid == id => return Ok(Terminal::Result(msg)),
                FromWorker::Cancelled { id: mid, segments } if *mid == id => return Ok(Terminal::Cancelled { segments: *segments }),
                FromWorker::Error { id: mid, code, detail, fatal } if mid.as_deref() == Some(id.as_str()) || mid.is_none() => {
                    if *fatal {
                        self.drop_proc();
                    }
                    return Err(worker_error(code, detail));
                }
                FromWorker::Bye => {
                    self.drop_proc();
                    return Err(Error::transcription("worker_crashed", "worker said bye"));
                }
                _ => {} // mensagem de outro pedido (atrasada) ou hello repetido: ignora
            }
        }
    }

    /// `shutdown` ao worker; se não sair em 10 s, SIGKILL do grupo. Idempotente.
    fn shutdown(&mut self) {
        let Some(mut p) = self.proc.take() else { return };
        let _ = send_line(&mut p, &ToWorker::Shutdown);
        drop(p.stdin);
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while Instant::now() < deadline {
            if matches!(p.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        kill_group(&mut p.child);
    }
}

impl Drop for ProcessEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
