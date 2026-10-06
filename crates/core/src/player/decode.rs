//! Leitor de uma trilha FLAC em streaming: decodifica pacote a pacote (memória constante, qualquer duração) e
//! busca por amostra. Os FLACs gravados pela app (`audio.rs`, flacenc) não têm tabela de busca: o symphonia
//! cai numa busca binária pelos quadros e a precisão por amostra vem de decodificar e descartar até o ponto.
use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

use crate::{Error, Result};

/// Posições abaixo disto (maior que qualquer quadro FLAC comum) são alcançadas reabrindo o arquivo.
const REOPEN_BELOW: u64 = 16_384;

pub struct TrackReader {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    pub rate: u32,
    /// Amostras por canal (do STREAMINFO).
    pub frames: u64,
    /// Índice da próxima amostra que `read` entrega.
    pos: u64,
    /// Sobra do último pacote decodificado (já em mono) e quanto dela já foi entregue.
    pending: Vec<i16>,
    taken: usize,
    /// Buffer de decodificação intercalado (reaproveitado).
    raw: Vec<i16>,
}

fn audio_err(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Audio(format!("{}: {e}", path.display()))
}

impl TrackReader {
    pub fn open(path: &Path) -> Result<TrackReader> {
        let file = File::open(path).map_err(|e| audio_err(path, e))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("flac");
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| audio_err(path, e))?;
        let track = format.default_track(TrackType::Audio).ok_or_else(|| audio_err(path, "no audio track"))?;
        let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| audio_err(path, "missing codec parameters"))?;
        let rate = params.sample_rate.ok_or_else(|| audio_err(path, "unknown sample rate"))?;
        let frames = track.num_frames.ok_or_else(|| audio_err(path, "unknown length (no total in the FLAC header)"))?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| audio_err(path, e))?;
        let track_id = track.id;
        Ok(TrackReader { path: path.to_path_buf(), format, decoder, track_id, rate, frames, pos: 0, pending: Vec::new(), taken: 0, raw: Vec::new() })
    }

    /// Índice da próxima amostra a entregar.
    pub fn pos(&self) -> u64 {
        self.pos
    }

    /// Entrega até `out.len()` amostras mono (vários canais viram a média). Devolve quantas entregou;
    /// menos que o pedido só no fim da trilha (ou se o arquivo estiver cortado).
    pub fn read(&mut self, out: &mut [i16]) -> Result<usize> {
        // o total do cabeçalho manda: nada além dele (nem lixo no fim do arquivo, nem depois de um `seek` ao fim)
        let max = out.len().min(self.frames.saturating_sub(self.pos) as usize);
        let out = &mut out[..max];
        let mut n = 0;
        while n < out.len() {
            if self.taken == self.pending.len() && !self.fill()? {
                break;
            }
            let take = (self.pending.len() - self.taken).min(out.len() - n);
            out[n..n + take].copy_from_slice(&self.pending[self.taken..self.taken + take]);
            self.taken += take;
            n += take;
        }
        self.pos += n as u64;
        Ok(n)
    }

    /// Decodifica o próximo pacote para `pending`; `false` no fim.
    fn fill(&mut self) -> Result<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                // arquivo cortado no meio de um quadro: trata como fim
                Err(SymError::IoError(_)) | Err(SymError::DecodeError(_)) => return Ok(false),
                Err(e) => return Err(Error::Audio(format!("decode: {e}"))),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let ch = buf.spec().channels().count().max(1);
                    self.raw.clear();
                    buf.copy_to_vec_interleaved::<i16>(&mut self.raw);
                    self.pending.clear();
                    if ch == 1 {
                        self.pending.extend_from_slice(&self.raw);
                    } else {
                        self.pending.extend(self.raw.chunks_exact(ch).map(|f| (f.iter().map(|&s| i32::from(s)).sum::<i32>() / ch as i32) as i16));
                    }
                    self.taken = 0;
                    if !self.pending.is_empty() {
                        return Ok(true);
                    }
                }
                // quadro corrompido: segue como silêncio do tamanho dele, para o tempo não deslizar
                Err(SymError::DecodeError(_)) => {
                    let dur = packet.dur.get() as usize;
                    self.pending.clear();
                    self.pending.resize(dur, 0);
                    self.taken = 0;
                    if dur > 0 {
                        return Ok(true);
                    }
                }
                Err(e) => return Err(Error::Audio(format!("decode: {e}"))),
            }
        }
    }

    /// Posiciona na amostra `sample` (exata). Além do fim = fim.
    pub fn seek(&mut self, sample: u64) -> Result<()> {
        self.pending.clear();
        self.taken = 0;
        if sample >= self.frames {
            self.pos = self.frames;
            return Ok(());
        }
        // Defeito do symphonia 0.6: pular para o 1º quadro (o início do arquivo) não zera o estado do parser, e
        // depois de ler até o fim o pacote seguinte falha ("unexpected end of file"). Reabrir é barato (só o
        // cabeçalho) e o resto até `sample` é decodificado e descartado.
        if sample < REOPEN_BELOW {
            *self = TrackReader::open(&self.path)?;
        }
        let to = SeekTo::Timestamp { ts: Timestamp::new(sample as i64), track_id: self.track_id };
        let at = self.format.seek(SeekMode::Accurate, to).map_err(|e| Error::Audio(format!("seek: {e}")))?;
        self.decoder.reset();
        self.pos = at.actual_ts.get().max(0) as u64;
        // do quadro encontrado até a amostra pedida: decodifica e descarta
        let mut skip = sample.saturating_sub(self.pos);
        let mut scratch = [0i16; 4096];
        while skip > 0 {
            let n = self.read(&mut scratch[..skip.min(4096) as usize])?;
            if n == 0 {
                break;
            }
            skip -= n as u64;
        }
        Ok(())
    }
}
