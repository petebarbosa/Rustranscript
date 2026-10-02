//! Captura de áudio da fase 3: mic + áudio do sistema → WAV incremental à prova de queda + sidecar.
//! Não depende do núcleo (o núcleo é que depende deste crate). Contrato completo: `RECORDING_CONTRACT.md`.
//!
//! - `backend`: traits `CaptureBackend`/`CaptureStream` + `default_backend()` (env `TRANSCRICOES_FAKE_AUDIO`).
//! - `pulse` (Linux, feature `pulse`): libpulse. `fake`: tons sintéticos, sem dispositivos.
//! - `session`: `Session` (gravar), `Monitor` (só níveis), `StreamChoice`.
//! - `wav`: `WavWriter`, `repair_wav`. `sidecar`: `recording.json`. `levels`: medidores.
pub mod backend;
pub mod error;
pub mod fake;
pub mod levels;
#[cfg(all(target_os = "linux", feature = "pulse"))]
pub mod pulse;
pub mod session;
pub mod sidecar;
pub mod wav;

pub use backend::{
    CHANNELS, CaptureBackend, CaptureStream, DeviceInfo, DeviceKind, FAKE_ENV, SAMPLE_RATE, UnavailableBackend, default_backend,
};
pub use error::{Error, Result};
pub use fake::FakeBackend;
pub use levels::{LevelMeter, Levels, StreamLevel};
pub use session::{Monitor, Session, SessionStatus, StartOptions, StreamChoice, StreamStatus};
pub use sidecar::{Cut, Sidecar, State, StreamMeta};
pub use wav::{RepairReport, WavWriter, repair_wav};
