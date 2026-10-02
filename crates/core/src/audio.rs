//! WAV → FLAC sem perda (o FLAC preserva taxa, canais e profundidade do WAV original).
use std::fs::File;
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::config::Encoder;
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};

use crate::{Error, Result, fsx};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub float: bool,
    /// Amostras por canal.
    pub frames: u64,
}

impl WavInfo {
    pub fn duration_s(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }
}

pub fn wav_info(path: &Path) -> Result<WavInfo> {
    let reader = hound::WavReader::open(path).map_err(|e| Error::Audio(format!("{}: {e}", path.display())))?;
    let spec = reader.spec();
    Ok(WavInfo {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        bits: spec.bits_per_sample,
        float: spec.sample_format == hound::SampleFormat::Float,
        frames: reader.duration() as u64,
    })
}

/// Quadros FLAC codificados por thread em cada lote. Com 8 threads e blocos de 4096 amostras, o lote
/// guarda ~4 MB de PCM em memória, independentemente da duração da gravação.
const FRAMES_PER_THREAD: usize = 32;
const MAX_THREADS: usize = 16;

/// Lê o WAV em blocos de quadros intercalados, já na profundidade de bits da saída.
struct WavBlocks {
    reader: hound::WavReader<BufReader<File>>,
    info: WavInfo,
    out_bits: usize,
    buf: Vec<i32>,
}

impl WavBlocks {
    /// Devolve até `frames` quadros (amostras de todos os canais); menos só no fim do arquivo.
    fn next(&mut self, frames: usize) -> std::result::Result<&[i32], hound::Error> {
        let channels = self.info.channels as usize;
        let want = frames * channels;
        self.buf.clear();
        if self.info.float {
            // FLAC não guarda ponto flutuante: converte para 16 bits.
            for s in self.reader.samples::<f32>().take(want) {
                self.buf.push((s?.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i32);
            }
        } else {
            let shift = self.info.bits as usize - self.out_bits;
            for s in self.reader.samples::<i32>().take(want) {
                self.buf.push(s? >> shift);
            }
        }
        // descarta um quadro incompleto no fim (arquivo cortado no meio de uma gravação)
        self.buf.truncate(self.buf.len() / channels * channels);
        Ok(&self.buf)
    }
}

/// Bytes de `fLaC` + bloco STREAMINFO (42 bytes): o cabeçalho que é regravado no fim.
fn header_bytes(info: &StreamInfo) -> Result<Vec<u8>> {
    let mut sink = ByteSink::new();
    Stream::with_stream_info(info.clone()).write(&mut sink).map_err(|e| Error::Audio(format!("flac write: {e:?}")))?;
    Ok(sink.as_slice().to_vec())
}

/// Codifica em streaming para `tmp`: grava um STREAMINFO provisório, vai anexando quadros em lotes pequenos
/// (codificados em paralelo, gravados em ordem) e, no fim, volta ao início e regrava o STREAMINFO com total de
/// amostras, MD5 e tamanhos de quadro/bloco. A memória fica limitada ao tamanho de um lote.
fn encode_to(
    src: &Path,
    tmp: &Path,
    mut blocks: WavBlocks,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let info = blocks.info;
    let enc_err = |e: &dyn std::fmt::Display| Error::Audio(format!("flac encode {}: {e}", src.display()));
    let config = Encoder::default().into_verified().map_err(|e| Error::Audio(format!("flac config: {e:?}")))?;
    let block_size = config.block_size;
    let channels = info.channels as usize;
    let mut stream_info = StreamInfo::new(info.sample_rate as usize, channels, blocks.out_bits).map_err(|e| enc_err(&e))?;
    stream_info.set_block_sizes(block_size, block_size).map_err(|e| enc_err(&e))?;

    let mut out = BufWriter::new(File::create(tmp)?);
    out.write_all(&header_bytes(&stream_info)?)?;

    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(MAX_THREADS);
    let mut bufs: Vec<FrameBuf> =
        (0..threads * FRAMES_PER_THREAD).map(|_| FrameBuf::with_size(channels, block_size)).collect::<std::result::Result<_, _>>().map_err(|e| enc_err(&e))?;
    let mut ctx = Context::new(blocks.out_bits, channels);
    let mut sink = ByteSink::new();
    let (mut done, mut frames_written) = (0u64, 0usize);
    loop {
        // leitura sequencial (o MD5 depende da ordem)
        let mut filled = 0;
        while filled < bufs.len() {
            let block = blocks.next(block_size).map_err(|e| enc_err(&e))?;
            if block.is_empty() {
                break;
            }
            let fb = &mut bufs[filled];
            fb.fill_interleaved(block).map_err(|e| enc_err(&e))?;
            ctx.fill_interleaved(block).map_err(|e| enc_err(&e))?;
            filled += 1;
            done += (block.len() / channels) as u64;
            progress(done, info.frames);
        }
        if filled == 0 {
            break;
        }
        // codificação paralela do lote
        let per_thread = filled.div_ceil(threads);
        let first = frames_written;
        let (config, shared_info) = (&config, &stream_info);
        let encoded: Vec<Vec<flacenc::component::Frame>> = std::thread::scope(|scope| {
            let handles: Vec<_> = bufs[..filled]
                .chunks(per_thread)
                .enumerate()
                .map(|(i, chunk)| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .enumerate()
                            .map(|(j, fb)| {
                                flacenc::encode_fixed_size_frame(config, fb, first + i * per_thread + j, shared_info)
                                    .map_err(|e| e.to_string())
                            })
                            .collect::<std::result::Result<Vec<_>, String>>()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("flac encoder thread panicked")).collect::<std::result::Result<_, _>>()
        })
        .map_err(|e: String| enc_err(&e))?;
        for frame in encoded.into_iter().flatten() {
            sink.clear();
            frame.write(&mut sink).map_err(|e| Error::Audio(format!("flac write: {e:?}")))?;
            out.write_all(sink.as_slice())?;
            stream_info.update_frame_info(&frame);
            frames_written += 1;
        }
        if filled < bufs.len() {
            break;
        }
    }

    if frames_written > 1 {
        // como o encoder de referência: o último bloco, mais curto, não conta como bloco mínimo
        stream_info.set_block_sizes(block_size, block_size).map_err(|e| enc_err(&e))?;
    }
    stream_info.set_total_samples(ctx.total_samples());
    stream_info.set_md5_digest(&ctx.md5_digest());
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header_bytes(&stream_info)?)?;
    file.flush()?;
    Ok(())
}

/// Converte `src` em `dst` (escrita atômica: `dst.part` → `dst`). `progress(feito, total)` em quadros.
pub fn wav_to_flac(src: &Path, dst: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<WavInfo> {
    let info = wav_info(src)?;
    if info.frames == 0 {
        return Err(Error::Audio(format!("{}: no audio", src.display())));
    }
    let reader = hound::WavReader::open(src).map_err(|e| Error::Audio(format!("{}: {e}", src.display())))?;
    let out_bits = if info.float { 16 } else { (info.bits as usize).min(24) };
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = fsx::tmp_sibling(dst);
    let blocks = WavBlocks { reader, info, out_bits, buf: Vec::new() };
    let result = encode_to(src, &tmp, blocks, progress).and_then(|()| Ok(std::fs::rename(&tmp, dst)?));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sine(path: &Path, rate: u32, secs: f32) {
        let spec = hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(rate as f32 * secs) as u32 {
            let v = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 8000.0;
            w.write_sample(v as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    /// Ruído determinístico (xorshift) + seno: exercita preditores e resíduos de verdade.
    fn synth(frames: usize, channels: usize) -> Vec<i16> {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut out = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            for c in 0..channels {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let noise = ((x >> 40) % 600) as f32 - 300.0;
                let tone = (i as f32 * (220.0 + 110.0 * c as f32) * std::f32::consts::TAU / 16_000.0).sin() * 9000.0;
                out.push((tone + noise) as i16);
            }
        }
        out
    }

    fn write_pcm(path: &Path, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec { channels, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    /// Converte e confere: decodifica (claxon) == origem, STREAMINFO e MD5 corretos, sem `.part` sobrando.
    fn roundtrip(channels: u16, frames: usize) {
        use md5::{Digest, Md5};
        let dir = tempfile::tempdir().unwrap();
        let (wav, flac) = (dir.path().join("a.wav"), dir.path().join("a.flac"));
        let samples = synth(frames, channels as usize);
        write_pcm(&wav, channels, &samples);
        let mut calls = 0u64;
        let mut last = (0, 0);
        let info = wav_to_flac(&wav, &flac, &mut |a, b| {
            assert!(a >= last.0, "progress must be monotonic");
            calls += 1;
            last = (a, b);
        })
        .unwrap();
        assert_eq!((info.channels, info.frames), (channels, frames as u64));
        assert_eq!(last, (frames as u64, frames as u64));
        assert!(calls >= (frames / 4096) as u64);
        assert!(!fsx::tmp_sibling(&flac).exists());

        let mut reader = claxon::FlacReader::open(&flac).unwrap();
        let si = reader.streaminfo();
        assert_eq!((si.sample_rate, si.channels, si.bits_per_sample), (16_000, channels as u32, 16));
        assert_eq!(si.samples, Some(frames as u64));
        assert!(si.min_block_size <= si.max_block_size && si.max_block_size == 4096);
        if frames > 4096 {
            assert_eq!(si.min_block_size, 4096);
        }
        assert!(si.min_frame_size.is_some_and(|m| m > 0) && si.max_frame_size.is_some_and(|m| m > 0));
        let mut expect = Md5::new();
        for s in &samples {
            expect.update(s.to_le_bytes());
        }
        assert_eq!(si.md5sum[..], expect.finalize()[..], "STREAMINFO MD5 must match the PCM");
        let decoded: Vec<i32> = reader.samples().map(|s| s.unwrap()).collect();
        assert_eq!(decoded.len(), samples.len());
        assert!(decoded.iter().zip(&samples).all(|(&d, &s)| d == s as i32), "lossless: decoded samples must equal the source");
    }

    #[test]
    fn lossless_when_length_is_not_a_block_multiple() {
        roundtrip(1, 4096 * 3 + 777);
    }

    #[test]
    fn lossless_when_shorter_than_one_block() {
        roundtrip(1, 1000);
    }

    #[test]
    fn lossless_exact_block_multiple() {
        roundtrip(1, 4096 * 5);
    }

    #[test]
    fn lossless_stereo_interleaving() {
        roundtrip(2, 4096 * 7 + 13);
    }

    /// ~3 minutos (muitos lotes de threads) com a última frase incompleta.
    #[test]
    fn lossless_multi_minute_many_batches() {
        roundtrip(1, 16_000 * 180 + 4321);
    }

    #[test]
    fn float_wav_is_stored_as_16_bits() {
        let dir = tempfile::tempdir().unwrap();
        let (wav, flac) = (dir.path().join("f.wav"), dir.path().join("f.flac"));
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        let src: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.05).sin() * 1.5).collect(); // passa de 1.0: satura
        for &v in &src {
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        wav_to_flac(&wav, &flac, &mut |_, _| {}).unwrap();
        let mut r = claxon::FlacReader::open(&flac).unwrap();
        assert_eq!((r.streaminfo().bits_per_sample, r.streaminfo().samples), (16, Some(10_000)));
        let got: Vec<i32> = r.samples().map(|s| s.unwrap()).collect();
        let want: Vec<i32> = src.iter().map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i32).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn failure_leaves_no_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let (wav, flac) = (dir.path().join("bad.wav"), dir.path().join("bad.flac"));
        write_pcm(&wav, 1, &synth(20_000, 1));
        let len = std::fs::metadata(&wav).unwrap().len();
        // corta no meio de um quadro de dados: o cabeçalho continua prometendo mais amostras
        std::fs::OpenOptions::new().write(true).open(&wav).unwrap().set_len(len - 3001).unwrap();
        let res = wav_to_flac(&wav, &flac, &mut |_, _| {});
        assert!(matches!(res, Err(Error::Audio(_))));
        assert!(!flac.exists() && !fsx::tmp_sibling(&flac).exists());
    }

    #[test]
    fn converts_wav_to_smaller_flac() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        let flac = dir.path().join("out/mic.flac");
        write_sine(&wav, 16_000, 2.0);
        let mut last = (0, 0);
        let info = wav_to_flac(&wav, &flac, &mut |a, b| last = (a, b)).unwrap();
        assert_eq!((info.sample_rate, info.channels, info.frames), (16_000, 1, 32_000));
        assert_eq!(last, (32_000, 32_000));
        let bytes = std::fs::read(&flac).unwrap();
        assert_eq!(&bytes[..4], b"fLaC");
        assert!(bytes.len() < std::fs::metadata(&wav).unwrap().len() as usize);
    }
}
