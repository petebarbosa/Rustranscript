//! `recording.json`: o sidecar de cada gravação em andamento. Escrito de forma atômica (`.part` +
//! rename) no início (`state: "recording"`), reescrito quando as 1ªs leituras dão o alinhamento, e no
//! fim com `state: "complete"` e a duração. É o que a recuperação lê para saber o que havia ali.
//!
//! Depois de importada, a gravação vira uma chamada e o sidecar vai junto para a pasta dela
//! (`<chamada>/recording.json`) — a fase 4 lê o alinhamento mic×sys de lá.
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;

pub const SIDECAR_FILE: &str = "recording.json";
pub const MIC_WAV: &str = "mic.wav";
pub const SYS_WAV: &str = "sys.wav";
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Gravando (ou a app caiu: é o que a recuperação procura).
    Recording,
    /// Parada limpa: WAVs com cabeçalho final e fsync feito.
    Complete,
}

/// Um corte: o dispositivo sumiu (`read` falhou). A lacuna é preenchida com silêncio digital para o
/// índice da amostra continuar igual ao tempo desde `first_sample_unix_ms`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cut {
    /// Índice da 1ª amostra de silêncio inserida.
    pub at_sample: u64,
    pub at_unix_ms: i64,
    /// Duração da lacuna preenchida.
    pub gap_ms: u32,
    /// `"read_error"` | `"device_changed"`.
    pub reason: String,
    /// `false` se a sessão acabou antes de conseguir religar.
    pub reconnected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamMeta {
    /// `mic.wav` / `sys.wav`.
    pub file: String,
    /// Nome (id) e rótulo do dispositivo no momento da abertura.
    pub device: String,
    pub description: String,
    pub is_monitor: bool,
    /// Instante estimado (ms desde a época) da **amostra 0**: `leitura − latência − fragmento`.
    /// `None` até a 1ª leitura (queda logo no início).
    pub first_sample_unix_ms: Option<i64>,
    /// Instante em que a 1ª leitura retornou, e os termos usados na estimativa.
    pub first_read_unix_ms: Option<i64>,
    pub latency_ms: Option<u32>,
    pub fragment_ms: u32,
    /// Amostras gravadas (só confiável em `complete`; na recuperação é recalculado pelo arquivo).
    pub samples: u64,
    pub cuts: Vec<Cut>,
    pub reconnects: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    pub schema: u32,
    pub state: State,
    /// `call_YYYY-MM-DD_HH-MM-SS`.
    pub key: String,
    pub app_version: String,
    /// Hora local de início, `YYYY-MM-DDTHH:MM:SS` (mesmo formato de `calls.started_at`).
    pub started_at: String,
    pub started_unix_ms: i64,
    /// Preenchidos em `complete`.
    pub ended_at: Option<String>,
    pub duration_s: Option<f64>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Sempre `"s16le"`.
    pub format: String,
    /// `None` = trilha desligada (`StreamChoice::Off`).
    pub mic: Option<StreamMeta>,
    pub sys: Option<StreamMeta>,
    /// Livre para o chamador (o núcleo guarda aqui `{"intent": {...}}`: alvo, título, falantes, idioma).
    pub extra: serde_json::Value,
}

impl Sidecar {
    pub fn read(dir: &Path) -> Result<Sidecar> {
        Ok(serde_json::from_slice(&std::fs::read(dir.join(SIDECAR_FILE))?)?)
    }

    /// Escrita atômica: `recording.json.part` → `recording.json`.
    pub fn write(&self, dir: &Path) -> Result<()> {
        let path = dir.join(SIDECAR_FILE);
        let tmp = dir.join(format!("{SIDECAR_FILE}.part"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Quanto o mic começou **depois** do sys, em ms (negativo = antes). `None` se falta alguma
    /// das duas estimativas. A fase 4 usa isto para alinhar as trilhas (refinar por correlação).
    pub fn mic_offset_ms(&self) -> Option<i64> {
        Some(self.mic.as_ref()?.first_sample_unix_ms? - self.sys.as_ref()?.first_sample_unix_ms?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sidecar {
        let stream = |file: &str, first: Option<i64>| StreamMeta {
            file: file.into(),
            device: "dev".into(),
            description: "Dev".into(),
            is_monitor: file == SYS_WAV,
            first_sample_unix_ms: first,
            first_read_unix_ms: first.map(|f| f + 130),
            latency_ms: Some(30),
            fragment_ms: 100,
            samples: 0,
            cuts: vec![],
            reconnects: 0,
        };
        Sidecar {
            schema: SCHEMA,
            state: State::Recording,
            key: "call_2026-10-02_09-00-00".into(),
            app_version: "0.1.0".into(),
            started_at: "2026-10-02T09:00:00".into(),
            started_unix_ms: 1,
            ended_at: None,
            duration_s: None,
            sample_rate: 16_000,
            channels: 1,
            format: "s16le".into(),
            mic: Some(stream(MIC_WAV, Some(1_050))),
            sys: Some(stream(SYS_WAV, Some(1_000))),
            extra: serde_json::json!({"meta": {"title": "x"}}),
        }
    }

    #[test]
    fn roundtrip_atomic_and_offset() {
        let dir = tempfile::tempdir().unwrap();
        let s = sample();
        s.write(dir.path()).unwrap();
        assert!(!dir.path().join("recording.json.part").exists());
        let back = Sidecar::read(dir.path()).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.mic_offset_ms(), Some(50));
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join(SIDECAR_FILE)).unwrap()).unwrap();
        assert_eq!(json["state"], "recording");
    }
}
