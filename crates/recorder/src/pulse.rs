//! Backend PulseAudio/PipeWire-pulse (libpulse). Só Linux. Receita validada no spike (RECORDING_SPIKE §2–4):
//! - **Lista**: `libpulse-binding` (Mainloop padrão + `get_server_info` + `get_source_info_list`);
//!   `name`/`description` da fonte; `is_monitor = monitor_of_sink.is_some()`; padrão do mic =
//!   `default_source_name`, padrão do sys = `<default_sink_name>.monitor`.
//! - **Captura**: `libpulse-simple-binding` `Simple::new(.., Record, Some(dev), .., Spec{S16NE, 1 ch,
//!   16000 Hz}, None, BufferAttr{fragsize = fragment_ms})`: reamostragem/mistura feitas pelo servidor.
//!   `read` bloqueia até preencher o buffer; `get_latency()` dá a latência (às vezes 0 = indisponível).
//! - Sem `libpulse-dev`: o `libpulse-sys` cai no fallback `libpulse.so.0` (só `libpulse0` em runtime).
//! - Risco conhecido: o fluxo fica preso ao dispositivo resolvido na abertura; se o sink padrão mudar
//!   ou o dispositivo sumir o `read` falha → a sessão religa e registra um corte.
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State as CtxState};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::backend::{CHANNELS, CaptureBackend, CaptureStream, DeviceInfo, DeviceKind, SAMPLE_RATE};
use crate::playback::{PlaybackSink, failed};
use crate::{Error, Result};

/// Formato pedido ao servidor.
pub(crate) fn spec() -> Spec {
    Spec { format: Format::S16NE, channels: CHANNELS as u8, rate: SAMPLE_RATE }
}

/// Tempo máximo para conectar/consultar o servidor (um servidor travado não pode travar a gravação).
const SERVER_TIMEOUT: Duration = Duration::from_secs(3);
const APP_NAME: &str = "Rustranscript";

#[derive(Default)]
pub struct PulseBackend;

impl PulseBackend {
    pub fn new() -> Self {
        PulseBackend
    }
}

/// O que o servidor diz agora: padrões e todas as fontes (microfones e monitores).
struct Snapshot {
    default_source: Option<String>,
    default_sink: Option<String>,
    sources: Vec<DeviceInfo>,
}

impl Snapshot {
    fn monitor_default_name(&self) -> Option<String> {
        self.default_sink.as_ref().map(|s| format!("{s}.monitor"))
    }

    /// Marca `is_default`: fonte padrão (se for microfone) e monitor do sink padrão.
    fn devices(&self) -> Vec<DeviceInfo> {
        let monitor = self.monitor_default_name();
        self.sources
            .iter()
            .map(|d| {
                let is_default = if d.is_monitor {
                    monitor.as_deref() == Some(d.name.as_str())
                } else {
                    self.default_source.as_deref() == Some(d.name.as_str())
                };
                DeviceInfo { is_default, ..d.clone() }
            })
            .collect()
    }

    /// Dispositivo a abrir: o pedido por nome, ou o padrão da categoria já resolvido para um nome real
    /// (assim o sidecar e a reconexão sabem exatamente o que foi aberto).
    fn resolve(&self, kind: DeviceKind, device: Option<&str>) -> Result<DeviceInfo> {
        let all = self.devices();
        if let Some(name) = device {
            return all.into_iter().find(|d| d.name == name).ok_or_else(|| Error::DeviceNotFound(name.to_string()));
        }
        let want_monitor = kind == DeviceKind::Monitor;
        all.iter()
            .find(|d| d.is_default && d.is_monitor == want_monitor)
            .or_else(|| all.iter().find(|d| d.is_monitor == want_monitor))
            .cloned()
            .ok_or_else(|| {
                Error::DeviceNotFound(if want_monitor { "no default output monitor" } else { "no default microphone" }.into())
            })
    }
}

fn unavailable(what: impl std::fmt::Display) -> Error {
    Error::BackendUnavailable(format!("PulseAudio/PipeWire: {what}"))
}

/// `(fonte padrão, sink padrão)` como o servidor informou.
type Defaults = (Option<String>, Option<String>);

/// Conecta, lê servidor + fontes e desconecta. Mainloop próprio e de vida curta (não é `Send`).
fn snapshot() -> Result<Snapshot> {
    let mut ml = Mainloop::new().ok_or_else(|| unavailable("could not create mainloop"))?;
    let mut ctx = Context::new(&ml, APP_NAME).ok_or_else(|| unavailable("could not create context"))?;
    ctx.connect(None, FlagSet::NOAUTOSPAWN, None).map_err(unavailable)?;
    let deadline = Instant::now() + SERVER_TIMEOUT;

    // `iterate(false)` + espera curta, para respeitar o prazo
    let spin = |ml: &mut Mainloop| -> Result<()> {
        if Instant::now() > deadline {
            return Err(unavailable("timed out talking to the server"));
        }
        match ml.iterate(false) {
            IterateResult::Quit(_) | IterateResult::Err(_) => Err(unavailable("mainloop stopped")),
            IterateResult::Success(0) => {
                std::thread::sleep(Duration::from_millis(2));
                Ok(())
            }
            IterateResult::Success(_) => Ok(()),
        }
    };

    loop {
        spin(&mut ml)?;
        match ctx.get_state() {
            CtxState::Ready => break,
            CtxState::Failed | CtxState::Terminated => return Err(unavailable("connection refused or lost")),
            _ => {}
        }
    }

    let defaults: Rc<RefCell<Option<Defaults>>> = Rc::default();
    let sources: Rc<RefCell<Option<Vec<DeviceInfo>>>> = Rc::default();
    let op1 = {
        let out = defaults.clone();
        ctx.introspect().get_server_info(move |i| {
            let own = |c: &Option<std::borrow::Cow<'_, str>>| c.as_ref().map(|s| s.to_string()).filter(|s| !s.is_empty());
            *out.borrow_mut() = Some((own(&i.default_source_name), own(&i.default_sink_name)));
        })
    };
    let op2 = {
        let (out, acc) = (sources.clone(), Rc::new(RefCell::new(Vec::new())));
        ctx.introspect().get_source_info_list(move |r| match r {
            ListResult::Item(s) => {
                let name = s.name.as_ref().map(|n| n.to_string()).unwrap_or_default();
                let description = s.description.as_ref().map(|d| d.to_string()).unwrap_or_else(|| name.clone());
                acc.borrow_mut().push(DeviceInfo { name, description, is_monitor: s.monitor_of_sink.is_some(), is_default: false });
            }
            ListResult::End | ListResult::Error => *out.borrow_mut() = Some(std::mem::take(&mut *acc.borrow_mut())),
        })
    };
    while defaults.borrow().is_none() || sources.borrow().is_none() {
        spin(&mut ml)?;
    }
    drop((op1, op2));
    ctx.disconnect();
    let (default_source, default_sink) = defaults.borrow_mut().take().unwrap_or_default();
    let sources = sources.borrow_mut().take().unwrap_or_default();
    Ok(Snapshot { default_source, default_sink, sources })
}

impl CaptureBackend for PulseBackend {
    fn id(&self) -> &'static str {
        "pulse"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(snapshot()?.devices())
    }

    fn open(&self, kind: DeviceKind, device: Option<&str>, fragment_ms: u32) -> Result<Box<dyn CaptureStream>> {
        let info = snapshot()?.resolve(kind, device)?;
        // `fragsize` = o fragmento pedido; o resto fica a cargo do servidor (`u32::MAX` = padrão)
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: SAMPLE_RATE * 2 * fragment_ms.clamp(10, 1000) / 1000,
        };
        let simple = Simple::new(None, APP_NAME, Direction::Record, Some(&info.name), "recording", &spec(), None, Some(&attr))
            .map_err(|e| Error::OpenFailed(format!("{}: {e}", info.name)))?;
        Ok(Box::new(PulseStream { simple, info, bytes: Vec::new() }))
    }
}

/// Fluxo de captura (síncrono, bloqueante). Não é `Send`: vive na thread de captura que o abriu.
struct PulseStream {
    simple: Simple,
    info: DeviceInfo,
    bytes: Vec<u8>,
}

impl CaptureStream for PulseStream {
    fn device(&self) -> &DeviceInfo {
        &self.info
    }

    fn read(&mut self, buf: &mut [i16]) -> Result<()> {
        self.bytes.resize(buf.len() * 2, 0);
        self.simple.read(&mut self.bytes).map_err(|e| Error::Capture(format!("{}: {e}", self.info.name)))?;
        let (pairs, _) = self.bytes.as_chunks::<2>();
        for (out, b) in buf.iter_mut().zip(pairs) {
            *out = i16::from_ne_bytes(*b);
        }
        Ok(())
    }

    fn latency(&self) -> Option<Duration> {
        // 0 = o servidor não soube dizer
        self.simple.get_latency().ok().map(|l| Duration::from_micros(l.0)).filter(|d| !d.is_zero())
    }
}

/// Buffer de reprodução do servidor: pequeno para pausar/pular soarem na hora (o padrão do servidor é ~2 s).
const PLAYBACK_BUFFER_MS: u32 = 200;

/// Abre a saída padrão do servidor (a variável `PULSE_SINK` do processo é respeitada, pois não se escolhe
/// um sink pelo nome: quem usa o player usa o que o sistema usa).
pub fn open_sink(rate: u32) -> Result<Box<dyn PlaybackSink>> {
    let spec = Spec { format: Format::S16NE, channels: CHANNELS as u8, rate };
    if !spec.is_valid() {
        return Err(failed(format!("invalid sample rate {rate}")));
    }
    let bytes = |ms: u32| (u64::from(rate) * 2 * u64::from(ms) / 1000) as u32;
    // `tlength` = o que fica em fila; `prebuf` = quanto encher antes de começar; `minreq` = o resto, a cargo do servidor
    let attr = BufferAttr {
        maxlength: u32::MAX,
        tlength: bytes(PLAYBACK_BUFFER_MS),
        prebuf: bytes(PLAYBACK_BUFFER_MS / 2),
        minreq: u32::MAX,
        fragsize: u32::MAX,
    };
    let simple = Simple::new(None, APP_NAME, Direction::Playback, None, "playback", &spec, None, Some(&attr))
        .map_err(|e| Error::BackendUnavailable(format!("PulseAudio/PipeWire: {e}")))?;
    Ok(Box::new(PulseSink { simple, bytes: Vec::new() }))
}

/// Fluxo de reprodução (síncrono, bloqueante). Não é `Send`: vive na thread do player.
struct PulseSink {
    simple: Simple,
    bytes: Vec<u8>,
}

impl PlaybackSink for PulseSink {
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        self.bytes.clear();
        self.bytes.extend(samples.iter().flat_map(|s| s.to_ne_bytes()));
        self.simple.write(&self.bytes).map_err(failed)
    }

    fn latency(&self) -> Option<Duration> {
        self.simple.get_latency().ok().map(|l| Duration::from_micros(l.0))
    }

    fn flush(&mut self) -> Result<()> {
        self.simple.flush().map_err(failed)
    }

    fn drain(&mut self) -> Result<()> {
        self.simple.drain().map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_is_valid_s16_16k_mono() {
        let s = spec();
        assert!(s.is_valid());
        assert_eq!((s.rate, s.channels), (16_000, 1));
    }

    fn dev(name: &str, is_monitor: bool) -> DeviceInfo {
        DeviceInfo { name: name.into(), description: name.into(), is_monitor, is_default: false }
    }

    fn snap(default_source: Option<&str>, default_sink: Option<&str>) -> Snapshot {
        Snapshot {
            default_source: default_source.map(Into::into),
            default_sink: default_sink.map(Into::into),
            sources: vec![dev("mic_a", false), dev("mic_b", false), dev("out_a.monitor", true), dev("out_b.monitor", true)],
        }
    }

    #[test]
    fn defaults_are_marked_once_per_kind() {
        let d = snap(Some("mic_b"), Some("out_b")).devices();
        let flagged: Vec<_> = d.iter().filter(|x| x.is_default).map(|x| x.name.as_str()).collect();
        assert_eq!(flagged, ["mic_b", "out_b.monitor"]);
        assert!(d.iter().filter(|x| x.is_monitor).all(|x| x.name.ends_with(".monitor")));
    }

    #[test]
    fn default_resolves_to_a_real_name_and_named_must_exist() {
        let s = snap(Some("mic_b"), Some("out_b"));
        assert_eq!(s.resolve(DeviceKind::Mic, None).unwrap().name, "mic_b");
        assert_eq!(s.resolve(DeviceKind::Monitor, None).unwrap().name, "out_b.monitor");
        assert_eq!(s.resolve(DeviceKind::Mic, Some("mic_a")).unwrap().name, "mic_a");
        assert_eq!(s.resolve(DeviceKind::Mic, Some("nope")).unwrap_err().code(), "device_not_found");
        // padrão desconhecido (ou fonte padrão que é um monitor): cai no 1º da categoria
        let odd = snap(Some("out_a.monitor"), None);
        assert_eq!(odd.resolve(DeviceKind::Mic, None).unwrap().name, "mic_a");
        assert_eq!(odd.resolve(DeviceKind::Monitor, None).unwrap().name, "out_a.monitor");
        let none = Snapshot { default_source: None, default_sink: None, sources: vec![dev("mic_a", false)] };
        assert_eq!(none.resolve(DeviceKind::Monitor, None).unwrap_err().code(), "device_not_found");
    }
}
