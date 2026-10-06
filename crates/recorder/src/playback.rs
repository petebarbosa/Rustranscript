//! Saída de áudio do player (issue #22): o espelho da captura. Linux = libpulse (`pulse.rs`, o mesmo servidor
//! da gravação); testes e `TARY_FAKE_AUDIO` = `FakeSink`, que só guarda as amostras. O motor do player (no
//! núcleo) só conhece estes traits.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::FAKE_ENV;
use crate::{Error, Result};

/// Fluxo de saída (síncrono, bloqueante), s16 mono. Como o `Simple` do libpulse, **não é `Send`**: o motor
/// o abre dentro da própria thread (por isso o que cruza threads é o `SinkOpener`).
pub trait PlaybackSink {
    /// Entrega as amostras ao servidor; bloqueia enquanto o buffer dele estiver cheio (é o relógio do player).
    fn write(&mut self, samples: &[i16]) -> Result<()>;

    /// Quanto do que já foi escrito ainda não saiu pelo alto-falante (buffer do servidor + dispositivo).
    /// `None` = o servidor não soube dizer (a posição então ignora a latência).
    fn latency(&self) -> Option<Duration>;

    /// Descarta o que está no buffer do servidor (pausar e pular precisam soar na hora).
    fn flush(&mut self) -> Result<()>;

    /// Bloqueia até tudo o que foi escrito ter tocado (fim da chamada).
    fn drain(&mut self) -> Result<()>;
}

/// Abre a saída para um áudio de `rate` Hz mono. Chamado na thread do player.
pub type SinkOpener = Arc<dyn Fn(u32) -> Result<Box<dyn PlaybackSink>> + Send + Sync>;

/// Saída da execução atual: `FakeSink` com `TARY_FAKE_AUDIO` definida (sem som, em tempo real — a UI e os testes
/// de ponta a ponta andam como se tocasse), senão o libpulse.
pub fn default_sink_opener() -> SinkOpener {
    if std::env::var(FAKE_ENV).ok().is_some_and(|v| !v.is_empty() && v != "0") {
        return Arc::new(|rate| Ok(Box::new(FakeSink::realtime(rate)) as Box<dyn PlaybackSink>));
    }
    #[cfg(all(target_os = "linux", feature = "pulse"))]
    {
        Arc::new(crate::pulse::open_sink)
    }
    #[cfg(not(all(target_os = "linux", feature = "pulse")))]
    {
        Arc::new(|_| Err(Error::BackendUnavailable("no playback backend for this platform/build".into())))
    }
}

/// Saída falsa. Guarda tudo o que recebeu (`captured`); com `realtime` dorme o tempo das amostras escritas
/// (menos uma folga de buffer), como um servidor de verdade faria.
pub struct FakeSink {
    rate: u32,
    realtime: bool,
    /// Amostras entregues desde a abertura, na ordem (um `flush` não as apaga: o teste vê o que foi escrito).
    pub captured: Arc<Mutex<Vec<i16>>>,
    queued: u64,
    started: std::time::Instant,
}

impl FakeSink {
    pub fn new(rate: u32) -> FakeSink {
        FakeSink { rate, realtime: false, captured: Arc::default(), queued: 0, started: std::time::Instant::now() }
    }

    pub fn realtime(rate: u32) -> FakeSink {
        FakeSink { realtime: true, ..FakeSink::new(rate) }
    }

    /// Amostras que o "alto-falante" já consumiu no relógio de parede.
    fn played(&self) -> u64 {
        if !self.realtime {
            return self.queued;
        }
        ((self.started.elapsed().as_secs_f64() * f64::from(self.rate)) as u64).min(self.queued)
    }
}

/// Folga que o servidor de verdade guarda (aqui só para a espera do `write` falso).
const FAKE_BUFFER: Duration = Duration::from_millis(150);

impl PlaybackSink for FakeSink {
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        self.captured.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(samples);
        self.queued += samples.len() as u64;
        if self.realtime {
            let ahead = Duration::from_secs_f64((self.queued - self.played()) as f64 / f64::from(self.rate));
            if ahead > FAKE_BUFFER {
                std::thread::sleep(ahead - FAKE_BUFFER);
            }
        }
        Ok(())
    }

    fn latency(&self) -> Option<Duration> {
        Some(Duration::from_secs_f64((self.queued - self.played()) as f64 / f64::from(self.rate)))
    }

    fn flush(&mut self) -> Result<()> {
        // o que estava no buffer some: o relógio recomeça sem dívida
        self.queued = 0;
        self.started = std::time::Instant::now();
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        if self.realtime {
            std::thread::sleep(Duration::from_secs_f64((self.queued - self.played()) as f64 / f64::from(self.rate)));
        }
        Ok(())
    }
}

#[cfg(all(target_os = "linux", feature = "pulse"))]
pub(crate) fn failed(what: impl std::fmt::Display) -> Error {
    Error::Playback(what.to_string())
}
