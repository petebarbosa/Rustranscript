//! Parâmetros efetivos de uma tarefa: configurações (`keys`) + opções da tarefa (`JobOptions`) + dados da chamada.
use serde::{Deserialize, Serialize};

use crate::{App, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BleedParams {
    pub enabled: bool,
    pub margin_db: f64,
    pub containment: f64,
    pub min_words: usize,
    pub tolerance_s: f64,
}

impl Default for BleedParams {
    fn default() -> Self {
        BleedParams { enabled: true, margin_db: 15.0, containment: 0.6, min_words: 4, tolerance_s: 0.75 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// `auto` (o worker detecta) | `pt` | `en` | `es`
    pub language: String,
    pub hotwords: bool,
    pub beam_size: u32,
    /// Já resolvido: nunca 0 (0 nas configurações = núcleos físicos).
    pub threads: u32,
    pub vad_min_silence_ms: u32,
    pub low_priority: bool,
    pub diarization_threshold: f64,
    pub min_cluster_pct: f64,
    pub min_cluster_s: f64,
    pub bleed: BleedParams,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            language: "pt".into(),
            hotwords: true,
            beam_size: 5,
            threads: 4,
            vad_min_silence_ms: 500,
            low_priority: true,
            diarization_threshold: 0.9,
            min_cluster_pct: 5.0,
            min_cluster_s: 10.0,
            bleed: BleedParams::default(),
        }
    }
}

/// Opções por tarefa (corpo de `transcribe_enqueue`; vai em `options_json`). Tudo opcional: ausente = configuração.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobOptions {
    #[serde(default)]
    pub language: Option<String>,
    /// Pessoas do outro lado (sem contar o Eu) = `num_clusters` do worker. Ausente = `calls.expected_speakers`.
    #[serde(default)]
    pub expected_speakers: Option<i64>,
    #[serde(default)]
    pub bleed_filter: Option<bool>,
    #[serde(default)]
    pub bleed_margin_db: Option<f64>,
    #[serde(default)]
    pub diarization_threshold: Option<f64>,
}

/// Texto da configuração → valor (ausente/inválido = `None`).
fn parsed<T: std::str::FromStr>(app: &App, key: &str) -> Result<Option<T>> {
    Ok(app.setting(key)?.and_then(|v| v.trim().parse::<T>().ok()))
}

fn flag(app: &App, key: &str, default: bool) -> Result<bool> {
    Ok(match app.setting(key)?.as_deref().map(str::trim) {
        Some("1") => true,
        Some("0") => false,
        _ => default,
    })
}

impl Params {
    /// Lê as configurações (valor inválido/ausente = padrão) e aplica `options` por cima.
    pub fn from_settings(app: &App, options: &JobOptions) -> Result<Params> {
        use super::keys;
        let d = Params::default();
        let language = match options.language.as_deref().or(app.setting(keys::LANGUAGE)?.as_deref()) {
            Some(l) => crate::recording::language_code(l).unwrap_or("pt").to_string(),
            None => d.language.clone(),
        };
        let threads = match parsed::<u32>(app, keys::THREADS)? {
            Some(n) if n > 0 => n.min(16),
            _ => (num_cpus::get_physical() as u32).clamp(1, 16),
        };
        let b = BleedParams::default();
        let bleed = BleedParams {
            enabled: options.bleed_filter.unwrap_or(flag(app, keys::BLEED_FILTER, b.enabled)?),
            margin_db: options
                .bleed_margin_db
                .or(parsed::<f64>(app, keys::BLEED_MARGIN_DB)?)
                .filter(|v| v.is_finite() && (0.0..=60.0).contains(v))
                .unwrap_or(b.margin_db),
            containment: parsed::<f64>(app, keys::BLEED_CONTAINMENT)?
                .filter(|v| (0.0..=1.0).contains(v))
                .unwrap_or(b.containment),
            min_words: parsed::<usize>(app, keys::BLEED_MIN_WORDS)?.filter(|v| (1..=50).contains(v)).unwrap_or(b.min_words),
            tolerance_s: parsed::<f64>(app, keys::BLEED_TOLERANCE_S)?
                .filter(|v| v.is_finite() && (0.0..=10.0).contains(v))
                .unwrap_or(b.tolerance_s),
        };
        Ok(Params {
            language,
            hotwords: flag(app, keys::HOTWORDS, d.hotwords)?,
            beam_size: parsed::<u32>(app, keys::BEAM_SIZE)?.filter(|v| (1..=10).contains(v)).unwrap_or(d.beam_size),
            threads,
            vad_min_silence_ms: parsed::<u32>(app, keys::VAD_MIN_SILENCE_MS)?
                .filter(|v| (0..=10_000).contains(v))
                .unwrap_or(d.vad_min_silence_ms),
            low_priority: flag(app, keys::LOW_PRIORITY, d.low_priority)?,
            diarization_threshold: options
                .diarization_threshold
                .or(parsed::<f64>(app, keys::DIARIZATION_THRESHOLD)?)
                .filter(|v| v.is_finite() && *v > 0.0 && *v <= 2.0)
                .unwrap_or(d.diarization_threshold),
            min_cluster_pct: parsed::<f64>(app, keys::DIARIZATION_MIN_CLUSTER_PCT)?
                .filter(|v| (0.0..=100.0).contains(v))
                .unwrap_or(d.min_cluster_pct),
            min_cluster_s: parsed::<f64>(app, keys::DIARIZATION_MIN_CLUSTER_S)?
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or(d.min_cluster_s),
            bleed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validation_and_options_override() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::open(dir.path()).unwrap();
        let p = Params::from_settings(&app, &JobOptions::default()).unwrap();
        assert_eq!(p.language, "pt");
        assert_eq!(p.beam_size, 5);
        assert!(p.threads >= 1 && p.threads <= 16);
        assert_eq!(p.bleed, BleedParams::default());

        app.set_setting("transcription_language", Some("pt-BR")).unwrap();
        app.set_setting("transcription_beam_size", Some("99")).unwrap(); // inválido -> padrão
        app.set_setting("transcription_threads", Some("3")).unwrap();
        app.set_setting("bleed_margin_db", Some("abc")).unwrap();
        app.set_setting("diarization_threshold", Some("0.8")).unwrap();
        let p = Params::from_settings(&app, &JobOptions::default()).unwrap();
        assert_eq!((p.language.as_str(), p.beam_size, p.threads, p.bleed.margin_db), ("pt", 5, 3, 15.0));
        assert_eq!(p.diarization_threshold, 0.8);

        let o = JobOptions { language: Some("auto".into()), bleed_filter: Some(false), bleed_margin_db: Some(20.0), diarization_threshold: Some(0.7), ..Default::default() };
        let p = Params::from_settings(&app, &o).unwrap();
        assert_eq!(p.language, "auto");
        assert!(!p.bleed.enabled);
        assert_eq!((p.bleed.margin_db, p.diarization_threshold), (20.0, 0.7));
    }
}
