//! Player de áudio da tela da chamada (issue #22). A reprodução é do Rust, não da webview (no Linux o WebKitGTK
//! toca mídia pelo GStreamer, que não lê o protocolo de assets do Tauri): decodifica os FLACs, mistura mic e sys
//! e entrega à saída de áudio (`recorder::PlaybackSink`, libpulse no Linux). A UI manda tocar/pausar/pular/velocidade
//! e recebe eventos de posição.
//!
//! - `decode`: `TrackReader`, um FLAC em streaming com busca por amostra (symphonia).
//! - `mixer`: mic + sys no eixo de tempo das transcrições (aplica o `mic_offset` do sidecar).
//! - `stretch`: velocidade sem mudar o tom (WSOLA próprio). `session`: tudo o que decide o som e a posição, sem relógio.
//! - `engine`: a thread (`Player`), os comandos e os eventos. `peaks`: onda sonora e seu cache (`peaks.bin`).
//! - `source`: caminhos no disco, deslocamento do sidecar e o motivo de não haver áudio.
pub mod decode;
pub mod engine;
pub mod mixer;
pub mod peaks;
pub mod session;
pub mod source;
pub mod stretch;

pub use engine::{EventFn, PlayState, Player, PlayerEvent};
pub use peaks::{PEAKS_FILE, PEAKS_PER_S, Peaks};
pub use session::{MAX_SPEED, MIN_SPEED, Session};
pub use source::{CallAudio, NoAudio, resolve};

#[cfg(test)]
mod tests;
