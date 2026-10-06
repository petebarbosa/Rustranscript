//! Contrato de captura. Linux = libpulse (`pulse.rs`); testes e desenvolvimento = `fake.rs`;
//! Windows (depois) = WASAPI loopback. Todo o resto do crate só conhece estes traits.
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Formato fixo de tudo que é gravado: s16le, 16 kHz, mono (o servidor de áudio reamostra/mistura).
pub const SAMPLE_RATE: u32 = 16_000;
pub const CHANNELS: u16 = 1;

/// Variável de ambiente que troca o backend real pelo `FakeBackend` (`1` = tempo real, `fast` = sem pausas).
pub const FAKE_ENV: &str = "TARY_FAKE_AUDIO";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Identificador estável (nome da fonte no PulseAudio); é isto que se guarda nas configurações.
    pub name: String,
    /// Rótulo para mostrar ao usuário.
    pub description: String,
    /// Fonte que captura a saída de um sink ("Monitor of …").
    pub is_monitor: bool,
    /// Padrão do sistema na sua categoria: fonte padrão (mic) ou monitor do sink padrão (sys).
    pub is_default: bool,
}

/// Qual lado se quer: decide o padrão quando `device = None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Microfone: padrão = fonte padrão do sistema.
    Mic,
    /// Áudio do sistema: padrão = monitor do sink padrão.
    Monitor,
}

pub trait CaptureBackend: Send + Sync {
    /// `"pulse"`, `"fake"` ou `"unavailable"`.
    fn id(&self) -> &'static str;

    /// Todas as fontes: microfones (`is_monitor = false`) e monitores. `is_default` em no máximo um de cada.
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;

    /// Abre um fluxo s16le 16 kHz mono (`device = None` → padrão de `kind`). Bloqueia até o servidor
    /// aceitar (pode levar centenas de ms). `fragment_ms` = tamanho de fragmento pedido (20–100).
    /// Erros: `DeviceNotFound`, `OpenFailed`, `BackendUnavailable`.
    ///
    /// O fluxo devolvido **não precisa ser `Send`** (o `Simple` do libpulse não é): a sessão chama
    /// `open` dentro da própria thread de captura e só o backend cruza threads.
    fn open(&self, kind: DeviceKind, device: Option<&str>, fragment_ms: u32) -> Result<Box<dyn CaptureStream>>;
}

pub trait CaptureStream {
    /// Dispositivo efetivamente aberto (padrão já resolvido).
    fn device(&self) -> &DeviceInfo;

    /// Bloqueia até preencher `buf` inteiro (amostras s16 mono 16 kHz). Erro = o fluxo morreu;
    /// quem chama decide religar (`open` de novo) e registrar o corte.
    fn read(&mut self, buf: &mut [i16]) -> Result<()>;

    /// Latência do fluxo agora (`None` = indisponível). Chamada depois da 1ª leitura, e de tempos em tempos.
    fn latency(&self) -> Option<Duration>;

    /// Instante de captura da 1ª amostra, **se o backend sabe com precisão** (timestamp do servidor).
    /// Padrão `None`: a sessão usa `agora_após_1ª_leitura − latência − duração_do_fragmento`
    /// (receita do spike §4; ±~30 ms com mic USB).
    fn first_sample_time(&self) -> Option<SystemTime> {
        None
    }
}

/// Backend usado quando não há nenhum para a plataforma: tudo falha com `BackendUnavailable`.
pub struct UnavailableBackend(pub String);

impl CaptureBackend for UnavailableBackend {
    fn id(&self) -> &'static str {
        "unavailable"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Err(Error::BackendUnavailable(self.0.clone()))
    }

    fn open(&self, _: DeviceKind, _: Option<&str>, _: u32) -> Result<Box<dyn CaptureStream>> {
        Err(Error::BackendUnavailable(self.0.clone()))
    }
}

/// Backend da execução atual: `FakeBackend` se `TARY_FAKE_AUDIO` estiver definida
/// (`fast` = sem pausas; qualquer outro valor não vazio e diferente de `0` = tempo real), senão o
/// real da plataforma (libpulse no Linux).
pub fn default_backend() -> Arc<dyn CaptureBackend> {
    if let Some(v) = std::env::var(FAKE_ENV).ok().filter(|v| !v.is_empty() && v != "0") {
        return Arc::new(crate::fake::FakeBackend::from_env_value(&v));
    }
    #[cfg(all(target_os = "linux", feature = "pulse"))]
    {
        Arc::new(crate::pulse::PulseBackend::new())
    }
    #[cfg(not(all(target_os = "linux", feature = "pulse")))]
    {
        Arc::new(UnavailableBackend("no capture backend for this platform/build".into()))
    }
}
