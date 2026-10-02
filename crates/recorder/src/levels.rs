//! Medidores de nível (para os VU meters da UI). Valores lineares 0..1 (fundo de escala = 1).
use serde::{Deserialize, Serialize};

use crate::backend::SAMPLE_RATE;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamLevel {
    /// Pico absoluto desde a última leitura (0..1).
    pub peak: f32,
    /// RMS desde a última leitura (0..1).
    pub rms: f32,
    /// Segundos seguidos de silêncio digital (todas as amostras = 0): mic mudo/morto (um mic do spike
    /// entregava zeros). A UI avisa quando passa de ~3 s.
    pub silent_s: f32,
    /// `false` enquanto o fluxo está desconectado e a sessão tenta religar.
    pub alive: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    /// `None` = trilha desligada.
    pub mic: Option<StreamLevel>,
    pub sys: Option<StreamLevel>,
}

/// Acumula amostras na thread de captura; `take` é chamado por **um único** leitor (o ticker do shell).
#[derive(Debug, Default)]
pub struct LevelMeter {
    sum_sq: f64,
    count: u64,
    peak: u32,
    silent_run: u64,
}

impl LevelMeter {
    pub fn push(&mut self, samples: &[i16]) {
        for &s in samples {
            let a = (s as i32).unsigned_abs();
            self.peak = self.peak.max(a);
            self.sum_sq += (a as f64) * (a as f64);
            self.silent_run = if s == 0 { self.silent_run + 1 } else { 0 };
        }
        self.count += samples.len() as u64;
    }

    /// Nível desde a última chamada e zera pico/RMS (o contador de silêncio continua).
    /// Sem amostras novas: pico e RMS 0, `silent_s` como estava.
    pub fn take(&mut self) -> StreamLevel {
        let rms = if self.count == 0 { 0.0 } else { (self.sum_sq / self.count as f64).sqrt() / 32768.0 };
        let level = StreamLevel {
            peak: (self.peak as f32 / 32768.0).min(1.0),
            rms: rms as f32,
            silent_s: self.silent_run as f32 / SAMPLE_RATE as f32,
            alive: true,
        };
        self.sum_sq = 0.0;
        self.count = 0;
        self.peak = 0;
        level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_rms_and_silence() {
        let mut m = LevelMeter::default();
        m.push(&[0; 16_000]);
        let l = m.take();
        assert_eq!((l.peak, l.rms), (0.0, 0.0));
        assert!((l.silent_s - 1.0).abs() < 1e-6);
        m.push(&[16384, -16384, 16384, -16384]);
        let l = m.take();
        assert!((l.peak - 0.5).abs() < 1e-4 && (l.rms - 0.5).abs() < 1e-4);
        assert_eq!(l.silent_s, 0.0);
        assert_eq!(m.take().peak, 0.0, "take zera o pico");
    }
}
