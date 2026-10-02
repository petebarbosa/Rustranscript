//! Chaves de configuração (`app.db.settings`, sempre texto) e padrões. Todas passam por `set_setting`
//! (a lista de permitidas está em `src-tauri/src/gui.rs`). `Params::from_settings` lê tudo e valida.

/// Idioma da transcrição: `auto` | `pt` | `en` | `es` (aceita também `pt-BR`...). Padrão `pt`.
pub const LANGUAGE: &str = "transcription_language";
/// `1`: a fila pega sozinha chamadas `pending` (gravações novas). Padrão `1`.
pub const AUTO: &str = "transcription_auto";
/// `1`: a fila está pausada pelo usuário. Padrão `0`.
pub const QUEUE_PAUSED: &str = "transcription_queue_paused";
/// `1`: hotwords = termos do glossário. Padrão `1`.
pub const HOTWORDS: &str = "transcription_hotwords";
/// Padrão `5`.
pub const BEAM_SIZE: &str = "transcription_beam_size";
/// Threads do worker; `0` = núcleos físicos (automático). Padrão `0`.
pub const THREADS: &str = "transcription_threads";
/// Padrão `500`.
pub const VAD_MIN_SILENCE_MS: &str = "transcription_vad_min_silence_ms";
/// `1`: worker com `nice 19` + `ioprio` ocioso. Padrão `1`.
pub const LOW_PRIORITY: &str = "transcription_low_priority";
/// Limiar de agrupamento quando não há `expected_speakers`. Padrão `0.9`.
pub const DIARIZATION_THRESHOLD: &str = "diarization_threshold";
/// Fusão de clusters com menos de X % da fala ... Padrão `5`.
pub const DIARIZATION_MIN_CLUSTER_PCT: &str = "diarization_min_cluster_pct";
/// ... ou menos de X segundos de fala. Padrão `10`.
pub const DIARIZATION_MIN_CLUSTER_S: &str = "diarization_min_cluster_s";
/// `1`: filtro de vazamento (mic ← sys) ligado. Padrão `1`.
pub const BLEED_FILTER: &str = "bleed_filter";
/// Margem do portão de energia, dB. Padrão `15`.
pub const BLEED_MARGIN_DB: &str = "bleed_margin_db";
/// Contenção mínima do texto (0..1). Padrão `0.6`.
pub const BLEED_CONTAINMENT: &str = "bleed_containment";
/// Mínimo de palavras para o texto sozinho remover. Padrão `4`.
pub const BLEED_MIN_WORDS: &str = "bleed_min_words";
/// Tolerância das janelas, segundos. Padrão `0.75`.
pub const BLEED_TOLERANCE_S: &str = "bleed_tolerance_s";

/// Todas as chaves desta fase (a lista de permitidas do shell usa isto).
pub const ALL: &[&str] = &[
    LANGUAGE, AUTO, QUEUE_PAUSED, HOTWORDS, BEAM_SIZE, THREADS, VAD_MIN_SILENCE_MS, LOW_PRIORITY,
    DIARIZATION_THRESHOLD, DIARIZATION_MIN_CLUSTER_PCT, DIARIZATION_MIN_CLUSTER_S, BLEED_FILTER,
    BLEED_MARGIN_DB, BLEED_CONTAINMENT, BLEED_MIN_WORDS, BLEED_TOLERANCE_S,
];
