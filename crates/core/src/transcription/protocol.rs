//! Protocolo JSON-lines com o worker Python, versão 1 (uma mensagem por linha, UTF-8). O stdout do worker é
//! SÓ protocolo (logs no stderr). Um pedido por vez. Tempos sempre em segundos **do arquivo da trilha**.
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 1;

/// Pai → worker. `type` em snake_case. `id` é escolhido pelo pai e ecoado em toda resposta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToWorker {
    /// Faz ASR de uma trilha. `start_s` > 0 (retomada): o worker decodifica o áudio, descarta o que vem antes
    /// de `start_s`, roda o VAD/Whisper no resto e SOMA `start_s` a todos os tempos emitidos.
    Transcribe {
        id: String,
        audio: String,
        track: String,
        model_dir: String,
        /// `None` = detectar
        language: Option<String>,
        hotwords: Option<String>,
        beam_size: u32,
        threads: u32,
        word_timestamps: bool,
        vad_min_silence_ms: u32,
        start_s: f64,
        /// Cortes de áudio (#23): intervalos `(início, fim)` em segundos **do arquivo da trilha** que o worker
        /// zera logo depois de decodificar, antes de qualquer processamento. Os tempos emitidos continuam os do
        /// arquivo (nada é fatiado nem concatenado). Ausente/vazio = áudio inteiro.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mute: Vec<(f64, f64)>,
    },
    /// Agrupa os falantes de uma trilha (segmentação pyannote + embedding CAM++ + clustering).
    Diarize {
        id: String,
        audio: String,
        seg_model: String,
        emb_model: String,
        /// dica (nunca garantia); `None` = usar `threshold`
        num_clusters: Option<u32>,
        threshold: f64,
        threads: u32,
        /// Como em `Transcribe`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mute: Vec<(f64, f64)>,
    },
    /// Envelope de energia: dBFS (RMS) por passo de `step_ms` (o que foi zerado vira o piso, -120 dB).
    Energy {
        id: String,
        audio: String,
        step_ms: u32,
        /// Como em `Transcribe`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mute: Vec<(f64, f64)>,
    },
    /// Cancelamento cooperativo: o worker termina a janela corrente e responde `cancelled` (até ~12 s).
    Cancel { id: String },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordTime(pub f64, pub f64, pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnMsg {
    pub start: f64,
    pub end: f64,
    pub speaker: i64,
}

/// Worker → pai.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromWorker {
    /// Primeira linha, sem pedido. `protocol` diferente de `PROTOCOL` = runtime desatualizado.
    Hello {
        protocol: u32,
        worker: String,
        pid: u32,
        python: String,
        faster_whisper: Option<String>,
        sherpa_onnx: Option<String>,
        #[serde(default)]
        fake: bool,
    },
    /// `stage`: `loading_model` | `transcribe` | `diarize_segmentation` | `diarize_embedding` | `energy`.
    Progress {
        id: String,
        stage: String,
        #[serde(default)]
        audio_s: Option<f64>,
        #[serde(default)]
        total_s: Option<f64>,
        #[serde(default)]
        done: Option<u64>,
        #[serde(default)]
        total: Option<u64>,
    },
    /// Um trecho transcrito (streamed; o pai grava no banco na hora).
    Segment {
        id: String,
        start: f64,
        end: f64,
        text: String,
        #[serde(default)]
        words: Option<Vec<WordTime>>,
    },
    /// Fim normal. Campos por tipo de pedido: transcribe = `segments`, `seconds`, `language`;
    /// diarize = `turns`, `speakers`; energy = `step_ms`, `db`.
    Result {
        id: String,
        #[serde(default)]
        segments: Option<u64>,
        #[serde(default)]
        seconds: Option<f64>,
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        turns: Option<Vec<TurnMsg>>,
        #[serde(default)]
        speakers: Option<i64>,
        #[serde(default)]
        step_ms: Option<u32>,
        #[serde(default)]
        db: Option<Vec<f32>>,
    },
    Cancelled { id: String, #[serde(default)] segments: u64 },
    /// `code`: `audio_decode` | `model_missing` | `oom` | `bad_request` | `not_implemented` | `exception`.
    /// `fatal`: o worker não segue (o pai o reinicia).
    Error {
        #[serde(default)]
        id: Option<String>,
        code: String,
        detail: String,
        #[serde(default)]
        fatal: bool,
    },
    Bye,
}

pub fn to_line(msg: &ToWorker) -> String {
    serde_json::to_string(msg).expect("ToWorker always serializes")
}

pub fn parse_line(line: &str) -> crate::Result<FromWorker> {
    serde_json::from_str(line.trim()).map_err(|e| crate::Error::transcription("worker_protocol", format!("{e}: {line:.200}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_v1() {
        let m = ToWorker::Energy { id: "j-1".into(), audio: "/a.flac".into(), step_ms: 100, mute: vec![] };
        assert_eq!(to_line(&m), r#"{"type":"energy","id":"j-1","audio":"/a.flac","step_ms":100}"#);
        assert_eq!(to_line(&ToWorker::Shutdown), r#"{"type":"shutdown"}"#);
        let t = ToWorker::Transcribe {
            id: "j-2".into(), audio: "/s.flac".into(), track: "sys".into(), model_dir: "/m".into(), language: None,
            hotwords: Some("a, b".into()), beam_size: 5, threads: 4, word_timestamps: true, vad_min_silence_ms: 500, start_s: 12.5, mute: vec![(1.0, 2.5)],
        };
        let v: serde_json::Value = serde_json::from_str(&to_line(&t)).unwrap();
        assert_eq!(v["type"], "transcribe");
        assert!(v["language"].is_null());
        assert_eq!(v["start_s"], 12.5);
        assert_eq!(v["mute"], serde_json::json!([[1.0, 2.5]]));
        // sem cortes o campo nem vai no fio (o formato v1 de antes não muda) e a leitura aceita sem ele
        let e = ToWorker::Energy { id: "j-1".into(), audio: "/a.flac".into(), step_ms: 100, mute: vec![] };
        assert!(!to_line(&e).contains("mute"));
        let back: ToWorker = serde_json::from_str(r#"{"type":"energy","id":"x","audio":"/a","step_ms":50}"#).unwrap();
        assert_eq!(back, ToWorker::Energy { id: "x".into(), audio: "/a".into(), step_ms: 50, mute: vec![] });
    }

    #[test]
    fn parses_worker_lines() {
        let h = parse_line(r#"{"type":"hello","protocol":1,"worker":"0.1.0","pid":7,"python":"3.12.15","faster_whisper":null,"sherpa_onnx":null}"#).unwrap();
        assert!(matches!(h, FromWorker::Hello { protocol: 1, fake: false, .. }));
        let s = parse_line(r#"{"type":"segment","id":"j","start":1.0,"end":2.5,"text":"oi","words":[[1.0,1.4,"oi"]]}"#).unwrap();
        assert!(matches!(s, FromWorker::Segment { words: Some(ref w), .. } if w.len() == 1));
        let r = parse_line(r#"{"type":"result","id":"j","turns":[{"start":0.7,"end":6.1,"speaker":0}],"speakers":1}"#).unwrap();
        assert!(matches!(r, FromWorker::Result { turns: Some(ref t), .. } if t.len() == 1));
        assert_eq!(parse_line("não é json").unwrap_err().code(), "worker_protocol");
    }
}
