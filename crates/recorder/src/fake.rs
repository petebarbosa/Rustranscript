//! Backend sintético: tons senoidais e silêncio, determinístico, sem dispositivos nem servidor de áudio.
//! Ativado por `RSTT_FAKE_AUDIO=1` (tempo real) ou `=fast` (sem pausas) em `default_backend()`.
//! Usado pelos testes e para desenvolver a UI/shell sem microfone. Nunca grava áudio real.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::backend::{CaptureBackend, CaptureStream, DeviceInfo, DeviceKind, SAMPLE_RATE};
use crate::{Error, Result};

/// Nomes fixos (fazem parte do contrato: testes e a UI em modo fake dependem deles).
pub const MIC: &str = "fake_mic";
/// Mic mudo: só zeros (para testar o aviso de "mic sem sinal").
pub const MIC_SILENT: &str = "fake_mic_silent";
/// Mic que "desconecta" uma vez depois de ~1 s de áudio (para testar religar + corte no sidecar).
pub const MIC_FLAKY: &str = "fake_mic_flaky";
pub const MONITOR: &str = "fake_sink.monitor";
pub const MONITOR_2: &str = "fake_sink_2.monitor";

/// Frequência do tom: mic 440 Hz, monitores 880 Hz; amplitude 0,25 do fundo de escala.
pub const MIC_HZ: f64 = 440.0;
pub const SYS_HZ: f64 = 880.0;
pub const AMPLITUDE: f64 = 0.25;

#[derive(Debug, Clone)]
pub struct FakeConfig {
    /// `true`: `read` dorme até a hora real das amostras (tempo decorrido e níveis realistas).
    pub realtime: bool,
    /// Latência informada por `latency()`.
    pub latency: Duration,
    /// Quanto áudio o `MIC_FLAKY` entrega antes de falhar (uma vez por backend).
    pub flaky_after: Duration,
}

impl Default for FakeConfig {
    fn default() -> Self {
        FakeConfig { realtime: true, latency: Duration::from_millis(20), flaky_after: Duration::from_secs(1) }
    }
}

pub struct FakeBackend {
    cfg: FakeConfig,
    flaked: Arc<AtomicBool>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::with_config(FakeConfig::default())
    }

    /// Sem pausas: gera áudio o mais rápido possível (testes).
    pub fn fast() -> Self {
        Self::with_config(FakeConfig { realtime: false, ..FakeConfig::default() })
    }

    pub fn with_config(cfg: FakeConfig) -> Self {
        FakeBackend { cfg, flaked: Arc::new(AtomicBool::new(false)) }
    }

    /// `"fast"` → sem pausas; qualquer outro valor → tempo real.
    pub fn from_env_value(value: &str) -> Self {
        if value.eq_ignore_ascii_case("fast") { Self::fast() } else { Self::new() }
    }

    fn devices() -> Vec<DeviceInfo> {
        let d = |name: &str, description: &str, is_monitor, is_default| DeviceInfo {
            name: name.into(),
            description: description.into(),
            is_monitor,
            is_default,
        };
        vec![
            d(MIC, "Fake Microphone", false, true),
            d(MIC_SILENT, "Fake Microphone (silent)", false, false),
            d(MIC_FLAKY, "Fake Microphone (flaky)", false, false),
            d(MONITOR, "Monitor of Fake Output", true, true),
            d(MONITOR_2, "Monitor of Fake Output 2", true, false),
        ]
    }
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for FakeBackend {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(Self::devices())
    }

    fn open(&self, kind: DeviceKind, device: Option<&str>, fragment_ms: u32) -> Result<Box<dyn CaptureStream>> {
        let all = Self::devices();
        let info = match device {
            Some(name) => all.into_iter().find(|d| d.name == name).ok_or_else(|| Error::DeviceNotFound(name.to_string()))?,
            None => all
                .into_iter()
                .find(|d| d.is_default && d.is_monitor == (kind == DeviceKind::Monitor))
                .ok_or_else(|| Error::DeviceNotFound("default".into()))?,
        };
        let flaky = info.name == MIC_FLAKY && !self.flaked.load(Ordering::SeqCst);
        Ok(Box::new(FakeStream {
            hz: if info.is_monitor { SYS_HZ } else { MIC_HZ },
            silent: info.name == MIC_SILENT,
            info,
            cfg: self.cfg.clone(),
            flaky_flag: flaky.then(|| self.flaked.clone()),
            produced: 0,
            started: None,
            _fragment_ms: fragment_ms,
        }))
    }
}

struct FakeStream {
    info: DeviceInfo,
    cfg: FakeConfig,
    hz: f64,
    silent: bool,
    /// `Some` enquanto este fluxo ainda vai falhar; ao falhar, marca o backend como "já falhou".
    flaky_flag: Option<Arc<AtomicBool>>,
    produced: u64,
    started: Option<Instant>,
    _fragment_ms: u32,
}

impl CaptureStream for FakeStream {
    fn device(&self) -> &DeviceInfo {
        &self.info
    }

    fn read(&mut self, buf: &mut [i16]) -> Result<()> {
        if let Some(flag) = &self.flaky_flag
            && self.produced as f64 / SAMPLE_RATE as f64 >= self.cfg.flaky_after.as_secs_f64()
        {
            flag.store(true, Ordering::SeqCst);
            return Err(Error::Capture("fake device unplugged".into()));
        }
        // seno calculado pelo índice da amostra: mesmo resultado independente do tamanho dos fragmentos
        for (i, out) in buf.iter_mut().enumerate() {
            *out = if self.silent {
                0
            } else {
                let n = (self.produced + i as u64) as f64;
                ((2.0 * std::f64::consts::PI * self.hz * n / SAMPLE_RATE as f64).sin() * AMPLITUDE * 32767.0) as i16
            };
        }
        self.produced += buf.len() as u64;
        if self.cfg.realtime {
            let start = *self.started.get_or_insert_with(Instant::now);
            let due = start + Duration::from_secs_f64(self.produced as f64 / SAMPLE_RATE as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        Ok(())
    }

    fn latency(&self) -> Option<Duration> {
        Some(self.cfg.latency)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_tone_independent_of_fragment_size() {
        let b = FakeBackend::fast();
        let mut a = b.open(DeviceKind::Mic, None, 100).unwrap();
        let mut c = b.open(DeviceKind::Mic, Some(MIC), 20).unwrap();
        let (mut x, mut y) = (vec![0i16; 1600], vec![0i16; 1600]);
        a.read(&mut x).unwrap();
        for chunk in y.chunks_mut(320) {
            c.read(chunk).unwrap();
        }
        assert_eq!(x, y);
        assert!(x.iter().any(|&s| s != 0));
    }

    #[test]
    fn defaults_silent_and_flaky() {
        let b = FakeBackend::fast();
        assert_eq!(b.open(DeviceKind::Monitor, None, 100).unwrap().device().name, MONITOR);
        let mut s = b.open(DeviceKind::Mic, Some(MIC_SILENT), 100).unwrap();
        let mut buf = vec![1i16; 160];
        s.read(&mut buf).unwrap();
        assert!(buf.iter().all(|&v| v == 0));
        assert!(matches!(b.open(DeviceKind::Mic, Some("nope"), 100), Err(Error::DeviceNotFound(_))));

        let mut f = b.open(DeviceKind::Mic, Some(MIC_FLAKY), 100).unwrap();
        let mut one_sec = vec![0i16; SAMPLE_RATE as usize];
        f.read(&mut one_sec).unwrap();
        assert!(f.read(&mut buf).is_err(), "falha depois de ~1 s");
        let mut again = b.open(DeviceKind::Mic, Some(MIC_FLAKY), 100).unwrap();
        again.read(&mut one_sec).unwrap();
        again.read(&mut buf).unwrap(); // religado: não falha de novo
    }

    #[test]
    fn exactly_one_default_per_kind() {
        let d = FakeBackend::fast().list_devices().unwrap();
        assert_eq!(d.iter().filter(|x| x.is_default && !x.is_monitor).count(), 1);
        assert_eq!(d.iter().filter(|x| x.is_default && x.is_monitor).count(), 1);
    }
}
