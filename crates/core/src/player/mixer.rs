//! Mistura mic + sys numa trilha só, no eixo de tempo das transcrições: os trechos têm o tempo do sys e o mic
//! entra deslocado pelo `mic_offset` do sidecar (a mesma regra de `transcription::assemble`; instante negativo
//! do mic é descartado). A mistura é a soma saturada; lê-se em blocos, sem nunca carregar uma trilha inteira.
use std::path::Path;

use super::decode::TrackReader;
use crate::{Error, Result};

struct MixTrack {
    reader: TrackReader,
    /// Índice (no eixo da chamada) em que a 1ª amostra da trilha toca; negativo = a trilha começa "antes".
    start: i64,
}

impl MixTrack {
    fn end(&self) -> i64 {
        self.start + self.reader.frames as i64
    }
}

pub struct Mixer {
    tracks: Vec<MixTrack>,
    rate: u32,
    /// Amostras do eixo da chamada: o fim da trilha que acaba por último.
    len: u64,
    pos: u64,
    acc: Vec<i32>,
    scratch: Vec<i16>,
}

impl Mixer {
    /// `mic_offset_s`: quanto o mic começou depois do sys (`Sidecar::mic_offset_ms`); só vale com as duas trilhas.
    pub fn open(sys: Option<&Path>, mic: Option<&Path>, mic_offset_s: f64) -> Result<Mixer> {
        let sys = sys.map(TrackReader::open).transpose()?;
        let mic = mic.map(TrackReader::open).transpose()?;
        let rate = match (&sys, &mic) {
            (Some(s), Some(m)) if s.rate != m.rate => {
                return Err(Error::Audio(format!("tracks have different sample rates ({} and {} Hz)", s.rate, m.rate)));
            }
            (Some(r), _) | (_, Some(r)) => r.rate,
            (None, None) => return Err(Error::Audio("no audio tracks".into())),
        };
        let both = sys.is_some() && mic.is_some();
        let offset = if both { (mic_offset_s * f64::from(rate)).round() as i64 } else { 0 };
        let mut tracks = Vec::new();
        tracks.extend(sys.map(|reader| MixTrack { reader, start: 0 }));
        tracks.extend(mic.map(|reader| MixTrack { reader, start: offset }));
        Ok(Mixer::from_tracks(tracks, rate))
    }

    fn from_tracks(tracks: Vec<MixTrack>, rate: u32) -> Mixer {
        let len = tracks.iter().map(|t| t.end()).max().unwrap_or(0).max(0) as u64;
        Mixer { tracks, rate, len, pos: 0, acc: Vec::new(), scratch: Vec::new() }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Duração em amostras.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn seek(&mut self, pos: u64) {
        self.pos = pos.min(self.len);
    }

    /// Entrega as próximas amostras misturadas (menos que o pedido só no fim). Trilha ilegível no meio da
    /// leitura vira silêncio desse ponto em diante: o player não para por causa de um arquivo cortado.
    pub fn read(&mut self, out: &mut [i16]) -> usize {
        let n = (out.len() as u64).min(self.len - self.pos) as usize;
        if n == 0 {
            return 0;
        }
        let from = self.pos as i64;
        self.acc.clear();
        self.acc.resize(n, 0);
        for t in &mut self.tracks {
            let (a, b) = (from.max(t.start), (from + n as i64).min(t.end()));
            if a >= b {
                continue;
            }
            let local = (a - t.start) as u64;
            if t.reader.pos() != local && t.reader.seek(local).is_err() {
                continue;
            }
            self.scratch.clear();
            self.scratch.resize((b - a) as usize, 0);
            let got = t.reader.read(&mut self.scratch).unwrap_or(0);
            let base = (a - from) as usize;
            for (acc, &s) in self.acc[base..base + got].iter_mut().zip(&self.scratch) {
                *acc += i32::from(s);
            }
        }
        for (o, &a) in out.iter_mut().zip(&self.acc) {
            *o = a.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        }
        self.pos += n as u64;
        n
    }
}

#[cfg(test)]
impl Mixer {
    /// Só os testes montam trilhas à mão (sem sidecar).
    pub(crate) fn open_at(tracks: &[(&Path, i64)]) -> Result<Mixer> {
        let mut out = Vec::new();
        for (p, start) in tracks {
            out.push(MixTrack { reader: TrackReader::open(p)?, start: *start });
        }
        let rate = out.first().map(|t| t.reader.rate).ok_or_else(|| Error::Audio("no audio tracks".into()))?;
        Ok(Mixer::from_tracks(out, rate))
    }
}
