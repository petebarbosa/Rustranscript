//! Picos da onda sonora: um byte por 1/`PEAKS_PER_S` de segundo (o maior valor absoluto da mistura mic + sys,
//! 0–255). Calculados uma vez, em streaming (memória constante: 2 h = ~360 KB de picos), e guardados em
//! `peaks.bin` ao lado do áudio; a UI recebe uma versão reduzida à largura da barra (`downsample`).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::mixer::Mixer;
use super::source::CallAudio;
use crate::Result;

/// Picos por segundo guardados. Fino o bastante para os cortes (issue #23) e para a UI reduzir à largura da barra.
pub const PEAKS_PER_S: u32 = 50;
pub const PEAKS_FILE: &str = "peaks.bin";
const MAGIC: &[u8; 8] = b"TARYPK01";
const BLOCK: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peaks {
    pub per_s: u32,
    /// Duração em amostras da chamada (para saber quanto cada pico cobre).
    pub frames: u64,
    pub rate: u32,
    pub data: Vec<u8>,
}

/// Amostras por pico.
fn bucket_len(rate: u32) -> usize {
    (rate / PEAKS_PER_S).max(1) as usize
}

fn peak_byte(max_abs: u32) -> u8 {
    // arredonda para cima: som baixo mas não nulo nunca vira zero
    ((max_abs * 255).div_ceil(32768)).min(255) as u8
}

/// Lê a chamada toda, uma vez, do começo ao fim.
pub fn compute(mixer: &mut Mixer, mut progress: impl FnMut(u64, u64)) -> Peaks {
    let (rate, frames) = (mixer.rate(), mixer.len());
    let bucket = bucket_len(rate);
    let mut data = Vec::with_capacity((frames as usize).div_ceil(bucket));
    mixer.seek(0);
    let mut buf = vec![0i16; BLOCK];
    let (mut cur, mut filled) = (0u32, 0usize);
    loop {
        let n = mixer.read(&mut buf);
        if n == 0 {
            break;
        }
        for &s in &buf[..n] {
            cur = cur.max(u32::from(s.unsigned_abs()));
            filled += 1;
            if filled == bucket {
                data.push(peak_byte(cur));
                (cur, filled) = (0, 0);
            }
        }
        progress(mixer.pos(), frames);
    }
    if filled > 0 {
        data.push(peak_byte(cur));
    }
    Peaks { per_s: PEAKS_PER_S, frames, rate, data }
}

/// Reduz a `buckets` valores (o maior de cada faixa). Nunca devolve mais do que há.
pub fn downsample(data: &[u8], buckets: usize) -> Vec<u8> {
    if buckets == 0 || data.is_empty() {
        return Vec::new();
    }
    if data.len() <= buckets {
        return data.to_vec();
    }
    (0..buckets)
        .map(|i| {
            let (a, b) = (i * data.len() / buckets, ((i + 1) * data.len() / buckets).max(i * data.len() / buckets + 1));
            data[a..b.min(data.len())].iter().copied().max().unwrap_or(0)
        })
        .collect()
}

// ---------------------------------------------------------------- cache em disco

fn mtime_ns(p: &Path) -> i128 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128)
}

/// Cabeçalho que identifica o que os picos descrevem: tamanho e data de cada trilha e o deslocamento do mic.
/// Qualquer mudança nos arquivos (ou no formato) invalida o cache.
fn fingerprint(audio: &CallAudio) -> Vec<u8> {
    let mut h = MAGIC.to_vec();
    h.extend(PEAKS_PER_S.to_le_bytes());
    for t in [&audio.sys, &audio.mic] {
        match t {
            Some(p) => {
                h.push(1);
                h.extend(std::fs::metadata(p).map_or(0, |m| m.len()).to_le_bytes());
                h.extend(mtime_ns(p).to_le_bytes());
            }
            None => h.push(0),
        }
    }
    h.extend(audio.mic_offset_s.to_le_bytes());
    h
}

pub fn cache_path(audio: &CallAudio) -> PathBuf {
    audio.dir.join(PEAKS_FILE)
}

fn read_cache(audio: &CallAudio) -> Option<Peaks> {
    let bytes = std::fs::read(cache_path(audio)).ok()?;
    let head = fingerprint(audio);
    let rest = bytes.strip_prefix(head.as_slice())?;
    let (meta, data) = rest.split_at_checked(12)?;
    let frames = u64::from_le_bytes(meta[..8].try_into().ok()?);
    let rate = u32::from_le_bytes(meta[8..12].try_into().ok()?);
    // dados truncados (queda no meio da gravação do cache): recalcula
    (data.len() == (frames as usize).div_ceil(bucket_len(rate))).then(|| Peaks { per_s: PEAKS_PER_S, frames, rate, data: data.to_vec() })
}

/// Gravação atômica (`.part` + rename). Falhar não é erro: o player só perde o cache.
fn write_cache(audio: &CallAudio, p: &Peaks) {
    let mut bytes = fingerprint(audio);
    bytes.extend(p.frames.to_le_bytes());
    bytes.extend(p.rate.to_le_bytes());
    bytes.extend(&p.data);
    let path = cache_path(audio);
    let tmp = path.with_extension("bin.part");
    let ok = std::fs::File::create(&tmp).and_then(|mut f| f.write_all(&bytes)).is_ok();
    if !ok || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Os picos da chamada: do cache se ainda vale, senão calcula (lendo os FLACs) e guarda.
pub fn load_or_compute(audio: &CallAudio, progress: impl FnMut(u64, u64)) -> Result<Peaks> {
    if let Some(p) = read_cache(audio) {
        return Ok(p);
    }
    let mut mixer = audio.open_mixer()?;
    let peaks = compute(&mut mixer, progress);
    write_cache(audio, &peaks);
    Ok(peaks)
}
