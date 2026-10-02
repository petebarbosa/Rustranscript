//! WAV incremental à prova de queda (RECORDING_SPIKE §7). Formato fixo: PCM s16le 16 kHz mono,
//! cabeçalho canônico de **44 bytes** (nós o escrevemos, então é garantido).
//!
//! Protocolo do escritor: cabeçalho com tamanhos provisórios 0 ao criar; a cada ~1 s `sync()`
//! (flush + `sync_data`); a cada ~5 s `patch_header()` — **depois** de o dado estar durável, nunca
//! superestimando; ao parar `finish()` (patch final + fsync). Nunca fazer I/O na thread de captura:
//! o escritor roda em thread própria, alimentada por canal limitado.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{Error, Result};

pub const HEADER_LEN: u64 = 44;
/// Deslocamentos dos dois campos de tamanho no cabeçalho canônico.
pub const RIFF_SIZE_OFFSET: u64 = 4;
pub const DATA_SIZE_OFFSET: u64 = 40;

/// Cabeçalho canônico para `data_bytes` bytes de áudio (`RIFF size = 36 + data`).
pub fn canonical_header(data_bytes: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36u32.wrapping_add(data_bytes)).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&crate::backend::CHANNELS.to_le_bytes());
    h[24..28].copy_from_slice(&crate::backend::SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(crate::backend::SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

/// Bytes acumulados antes de ir para o arquivo (~1 s de áudio; o `sync` descarrega de qualquer forma).
const BUF_FLUSH_BYTES: usize = 64 * 1024;

/// Escritor do WAV incremental. Não faz I/O de captura: quem alimenta é a thread escritora da sessão.
pub struct WavWriter {
    file: File,
    /// Amostras já entregues ao arquivo (sistema operacional), fora o que está em `buf`.
    flushed: u64,
    /// Amostras já duráveis (`sync_data` feito): só estas entram no cabeçalho.
    synced: u64,
    buf: Vec<u8>,
    /// Último valor gravado nos tamanhos do cabeçalho (em amostras), para não regravar à toa.
    patched: u64,
}

fn io_err(path: &Path, what: &str, e: std::io::Error) -> Error {
    Error::Io(std::io::Error::new(e.kind(), format!("{what} {}: {e}", path.display())))
}

impl WavWriter {
    /// Cria o arquivo (falha se existir) com cabeçalho de tamanhos 0.
    pub fn create(path: &Path) -> Result<WavWriter> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| io_err(path, "create", e))?;
        file.write_all(&canonical_header(0)).map_err(|e| io_err(path, "write header", e))?;
        Ok(WavWriter { file, flushed: 0, synced: 0, buf: Vec::with_capacity(BUF_FLUSH_BYTES * 2), patched: 0 })
    }

    /// Acrescenta amostras (buffer interno; nada é garantido em disco até `sync`).
    pub fn write(&mut self, samples: &[i16]) -> Result<()> {
        for s in samples {
            self.buf.extend_from_slice(&s.to_le_bytes());
        }
        if self.buf.len() >= BUF_FLUSH_BYTES {
            self.flush_buf()?;
        }
        Ok(())
    }

    fn flush_buf(&mut self) -> Result<()> {
        if !self.buf.is_empty() {
            self.file.write_all(&self.buf)?;
            self.flushed += (self.buf.len() / 2) as u64;
            self.buf.clear();
        }
        Ok(())
    }

    /// `flush` + `sync_data`. Depois disto as amostras escritas até aqui são duráveis.
    pub fn sync(&mut self) -> Result<()> {
        self.flush_buf()?;
        self.file.sync_data()?;
        self.synced = self.flushed;
        Ok(())
    }

    /// Regrava os dois tamanhos do cabeçalho para as amostras **já sincronizadas** e volta ao fim.
    /// Nunca superestima: se a máquina cair, o cabeçalho descreve no máximo o que está em disco.
    pub fn patch_header(&mut self) -> Result<()> {
        if self.synced == self.patched {
            return Ok(());
        }
        let data = u32::try_from(self.synced * 2).unwrap_or(u32::MAX - 1);
        let h = canonical_header(data);
        self.file.seek(SeekFrom::Start(RIFF_SIZE_OFFSET))?;
        self.file.write_all(&h[4..8])?;
        self.file.seek(SeekFrom::Start(DATA_SIZE_OFFSET))?;
        self.file.write_all(&h[40..44])?;
        self.file.seek(SeekFrom::Start(HEADER_LEN + self.flushed * 2))?;
        self.patched = self.synced;
        Ok(())
    }

    /// Amostras escritas (inclui as ainda não sincronizadas).
    pub fn samples(&self) -> u64 {
        self.flushed + (self.buf.len() / 2) as u64
    }

    /// Flush + sync + patch final + fsync. Devolve o total de amostras.
    pub fn finish(mut self) -> Result<u64> {
        self.sync()?;
        self.patch_header()?;
        self.file.sync_all()?;
        Ok(self.synced)
    }
}

impl Drop for WavWriter {
    /// Descartar sem `finish` = queda: entrega o que está no buffer ao sistema (sem `sync` nem patch do
    /// cabeçalho), como se o processo tivesse morrido logo depois de escrever.
    fn drop(&mut self) {
        let _ = self.flush_buf();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairReport {
    /// Amostras válidas depois do reparo.
    pub samples: u64,
    /// Bytes descartados no fim (amostra cortada pela queda).
    pub dropped_bytes: u64,
    /// `true` se os tamanhos do cabeçalho estavam desatualizados (queda antes do patch final).
    pub header_was_stale: bool,
}

/// Recuperação após queda: `data = (tamanho_do_arquivo − 44)` arredondado para baixo a múltiplo de 2,
/// reescreve `RIFF size = 36 + data` e `data size = data`, `set_len(44 + data)`. Idempotente.
/// `InvalidWav` se o arquivo tem < 44 bytes ou o cabeçalho não é o canônico que escrevemos
/// (RIFF/WAVE/fmt 16 kHz mono s16). Depois disso `core_lib::audio::wav_to_flac` aceita o arquivo.
pub fn repair_wav(path: &Path) -> Result<RepairReport> {
    let mut file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| io_err(path, "open", e))?;
    let len = file.metadata()?.len();
    if len < HEADER_LEN {
        return Err(Error::InvalidWav(format!("{}: {len} bytes, shorter than the 44-byte header", path.display())));
    }
    let mut head = [0u8; 44];
    file.read_exact(&mut head)?;
    // tudo menos os dois tamanhos tem de ser igual ao cabeçalho que nós escrevemos
    let mut expected = canonical_header(0);
    expected[4..8].copy_from_slice(&head[4..8]);
    expected[40..44].copy_from_slice(&head[40..44]);
    if head != expected {
        return Err(Error::InvalidWav(format!("{}: not a 16 kHz mono s16 WAV written by this app", path.display())));
    }
    let payload = len - HEADER_LEN;
    // o formato do cabeçalho cabe em u32: acima de ~4 GiB (≈ 37 h) trunca-se o excedente
    let data = (payload & !1).min(u64::from(u32::MAX - 1));
    let canon = canonical_header(data as u32);
    let stale = head[4..8] != canon[4..8] || head[40..44] != canon[40..44];
    if stale {
        file.seek(SeekFrom::Start(RIFF_SIZE_OFFSET))?;
        file.write_all(&canon[4..8])?;
        file.seek(SeekFrom::Start(DATA_SIZE_OFFSET))?;
        file.write_all(&canon[40..44])?;
    }
    if payload != data {
        file.set_len(HEADER_LEN + data)?;
    }
    file.sync_all()?;
    Ok(RepairReport { samples: data / 2, dropped_bytes: payload - data, header_was_stale: stale })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_canonical_and_hound_compatible() {
        let h = canonical_header(32_000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 36 + 32_000);
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 32_000);
        let mut bytes = h.to_vec();
        bytes.extend(std::iter::repeat_n(0u8, 32_000));
        let r = std::io::Cursor::new(bytes);
        let reader = hound::WavReader::new(r).unwrap();
        assert_eq!((reader.spec().sample_rate, reader.spec().channels, reader.duration()), (16_000, 1, 16_000));
    }

    fn tone(n: usize) -> Vec<i16> {
        (0..n).map(|i| ((i as f64 * 0.05).sin() * 8000.0) as i16).collect()
    }

    fn sizes(path: &Path) -> (u32, u32) {
        let b = std::fs::read(path).unwrap();
        (u32::from_le_bytes(b[4..8].try_into().unwrap()), u32::from_le_bytes(b[40..44].try_into().unwrap()))
    }

    fn hound_frames(path: &Path) -> Vec<i16> {
        hound::WavReader::open(path).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
    }

    #[test]
    fn finish_writes_exact_header() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let data = tone(48_000);
        let mut w = WavWriter::create(&p).unwrap();
        // header provisório com tamanhos 0 desde o início
        assert_eq!(sizes(&p), (36, 0));
        for chunk in data.chunks(1600) {
            w.write(chunk).unwrap();
        }
        assert_eq!(w.samples(), 48_000);
        assert_eq!(w.finish().unwrap(), 48_000);
        assert_eq!(sizes(&p), (36 + 96_000, 96_000));
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 44 + 96_000);
        assert_eq!(hound_frames(&p), data);
        let r = repair_wav(&p).unwrap();
        assert_eq!((r.samples, r.dropped_bytes, r.header_was_stale), (48_000, 0, false));
    }

    #[test]
    fn create_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        WavWriter::create(&p).unwrap();
        assert!(WavWriter::create(&p).is_err());
    }

    #[test]
    fn patch_header_never_overestimates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let mut w = WavWriter::create(&p).unwrap();
        w.write(&tone(100)).unwrap();
        w.patch_header().unwrap(); // nada durável ainda: continua 0
        assert_eq!(sizes(&p).1, 0);
        w.sync().unwrap();
        w.write(&tone(50)).unwrap(); // ainda no buffer
        w.patch_header().unwrap();
        assert_eq!(sizes(&p), (36 + 200, 200), "só o que o sync tornou durável");
        assert_eq!(w.samples(), 150);
        // o cursor voltou ao fim: a escrita seguinte não sobrescreve o cabeçalho nem o dado
        w.write(&tone(10)).unwrap();
        assert_eq!(w.finish().unwrap(), 160);
        assert_eq!(hound_frames(&p).len(), 160);
    }

    #[test]
    fn crash_without_finish_zero_sizes_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let data = tone(30_000);
        let mut w = WavWriter::create(&p).unwrap();
        w.write(&data).unwrap();
        drop(w); // queda: sem sync, sem patch
        assert_eq!(sizes(&p).1, 0);
        assert!(hound::WavReader::open(&p).unwrap().duration() == 0, "sem reparo o leitor vê 0 quadros");
        let r = repair_wav(&p).unwrap();
        assert_eq!(r, RepairReport { samples: 30_000, dropped_bytes: 0, header_was_stale: true });
        assert_eq!(hound_frames(&p), data);
        assert!(!repair_wav(&p).unwrap().header_was_stale, "idempotente");
    }

    #[test]
    fn crash_mid_sample_drops_the_odd_byte() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let data = tone(5_000);
        let mut w = WavWriter::create(&p).unwrap();
        w.write(&data).unwrap();
        w.sync().unwrap();
        w.patch_header().unwrap();
        drop(w);
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&[0x7f]).unwrap(); // metade de uma amostra
        drop(f);
        let r = repair_wav(&p).unwrap();
        assert_eq!((r.samples, r.dropped_bytes), (5_000, 1));
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 44 + 10_000);
        assert_eq!(hound_frames(&p), data);
    }

    #[test]
    fn stale_header_after_late_crash_recovers_everything_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let data = tone(80_000);
        let mut w = WavWriter::create(&p).unwrap();
        w.write(&data[..16_000]).unwrap();
        w.sync().unwrap();
        w.patch_header().unwrap(); // cabeçalho diz 1 s
        w.write(&data[16_000..]).unwrap();
        drop(w); // mais 4 s chegaram ao arquivo depois do patch
        assert_eq!(hound_frames(&p).len(), 16_000, "o cabeçalho velho esconde o resto");
        let r = repair_wav(&p).unwrap();
        assert_eq!((r.samples, r.header_was_stale), (80_000, true));
        assert_eq!(hound_frames(&p), data);
    }

    #[test]
    fn pipe_style_ffffffff_sizes_and_truncated_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let data = tone(1_000);
        let mut bytes = canonical_header(u32::MAX).to_vec();
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend(data.iter().flat_map(|s| s.to_le_bytes()));
        bytes.truncate(bytes.len() - 1); // cortada no meio da última amostra
        std::fs::write(&p, &bytes).unwrap();
        let r = repair_wav(&p).unwrap();
        assert_eq!((r.samples, r.dropped_bytes, r.header_was_stale), (999, 1, true));
        assert_eq!(hound_frames(&p), data[..999]);
    }

    #[test]
    fn header_only_file_repairs_to_zero_samples() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        drop(WavWriter::create(&p).unwrap());
        assert_eq!(repair_wav(&p).unwrap().samples, 0);
    }

    #[test]
    fn invalid_files_are_rejected_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let short = dir.path().join("short.wav");
        std::fs::write(&short, [0u8; 20]).unwrap();
        assert_eq!(repair_wav(&short).unwrap_err().code(), "invalid_wav");
        assert_eq!(std::fs::metadata(&short).unwrap().len(), 20);

        let other = dir.path().join("other.wav"); // 44 bytes, mas não é o nosso formato (48 kHz)
        let mut h = canonical_header(0);
        h[24..28].copy_from_slice(&48_000u32.to_le_bytes());
        let mut b = h.to_vec();
        b.extend_from_slice(&[1, 2, 3, 4]);
        std::fs::write(&other, &b).unwrap();
        assert_eq!(repair_wav(&other).unwrap_err().code(), "invalid_wav");
        assert_eq!(std::fs::read(&other).unwrap(), b);
        assert_eq!(repair_wav(&dir.path().join("missing.wav")).unwrap_err().code(), "io");
    }
}
