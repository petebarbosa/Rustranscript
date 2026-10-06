//! Velocidade sem mudar o tom: WSOLA (waveform-similarity overlap-add) em streaming, próprio e puro Rust.
//!
//! Cada bloco de saída (`HOP` amostras) mistura, com janelas complementares, o fim do bloco anterior com um
//! quadro novo da entrada. Esse quadro é tirado perto de `nominal` (que avança `HOP × velocidade` por bloco), no
//! ponto dentro de ±`SEARCH` que mais se parece com a continuação natural do bloco anterior: as fases das
//! ondas casam e a voz soa inteira, só mais rápida. Sem dependência nativa (as alternativas — rubberband,
//! signalsmith-stretch — exigem C++ no build); para fala a 16 kHz o custo é desprezível.
use super::mixer::Mixer;

/// Passo de saída (20 ms a 16 kHz); o quadro tem 2×`HOP` (janela de 40 ms, superposição de 50%).
const HOP_MS: u32 = 20;
/// Meia largura da busca (12 ms: acima do maior período de pitch da voz, ~12 ms em 80 Hz).
const SEARCH_MS: u32 = 12;
const CHUNK: usize = 4096;

pub struct Stretcher {
    hop: usize,
    search: usize,
    /// Janela de entrada (`rise`) e de saída (`fall = 1 − rise`) da superposição.
    rise: Vec<f32>,
    fall: Vec<f32>,
    /// Entrada guardada: `src[0]` é a amostra `base` do eixo da chamada.
    src: Vec<f32>,
    base: u64,
    /// O mixer não tem mais nada: o que falta da entrada é silêncio.
    ended: bool,
    started: bool,
    /// Onde o próximo quadro deveria começar, na entrada (cresce `hop × velocidade` por bloco).
    nominal: f64,
    /// Início da continuação natural do bloco anterior na entrada (onde o quadro anterior seguiria sozinho).
    natural: u64,
    /// Metade final, já com a janela de saída, do quadro anterior.
    tail: Vec<f32>,
    speed: f64,
    total: u64,
    chunk: Vec<i16>,
}

impl Stretcher {
    pub fn new(rate: u32, total: u64) -> Stretcher {
        let hop = (u64::from(rate) * u64::from(HOP_MS) / 1000).max(8) as usize;
        let search = (u64::from(rate) * u64::from(SEARCH_MS) / 1000) as usize;
        let rise: Vec<f32> = (0..hop).map(|i| 0.5 * (1.0 - (std::f32::consts::PI * (i as f32 + 0.5) / hop as f32).cos())).collect();
        let fall = rise.iter().map(|r| 1.0 - r).collect();
        Stretcher {
            hop,
            search,
            rise,
            fall,
            src: Vec::new(),
            base: 0,
            ended: false,
            started: false,
            nominal: 0.0,
            natural: 0,
            tail: Vec::new(),
            speed: 1.0,
            total,
            chunk: vec![0; CHUNK],
        }
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
    }

    /// Recomeça do ponto `pos` do eixo da chamada (o mixer é posicionado junto).
    pub fn reset(&mut self, mixer: &mut Mixer, pos: u64) {
        mixer.seek(pos);
        self.src.clear();
        self.base = mixer.pos();
        self.ended = false;
        self.started = false;
        self.tail.clear();
    }

    /// Garante a entrada até `end` (exclusivo); depois do fim da chamada, zeros.
    fn ensure(&mut self, mixer: &mut Mixer, end: u64) {
        while self.base + (self.src.len() as u64) < end && !self.ended {
            let n = mixer.read(&mut self.chunk);
            if n == 0 {
                self.ended = true;
            }
            self.src.extend(self.chunk[..n].iter().map(|&s| f32::from(s)));
        }
        if self.ended {
            let want = (end.saturating_sub(self.base)) as usize;
            if self.src.len() < want {
                self.src.resize(want, 0.0);
            }
        }
    }

    /// Produz um bloco de `hop` amostras em `out`. Devolve a posição de entrada (amostras do eixo da chamada) que
    /// ele representa — o início do próximo quadro — ou `None` quando a entrada acabou.
    pub fn next_block(&mut self, mixer: &mut Mixer, out: &mut Vec<i16>) -> Option<u64> {
        let hop = self.hop;
        let step = hop as f64 * self.speed;
        if !self.started {
            let p0 = self.base;
            if p0 >= self.total {
                return None;
            }
            self.ensure(mixer, p0 + 2 * hop as u64);
            let at = 0;
            out.extend(self.src[at..at + hop].iter().map(|&v| to_i16(v)));
            self.tail = (0..hop).map(|i| self.src[at + hop + i] * self.fall[i]).collect();
            self.natural = p0 + hop as u64;
            self.nominal = p0 as f64 + step;
            self.started = true;
            return Some(self.nominal as u64);
        }
        let target = self.nominal.round() as u64;
        if target >= self.total {
            return None;
        }
        let lo = target.saturating_sub(self.search as u64).max(self.base);
        let hi = target + self.search as u64;
        self.ensure(mixer, hi + 2 * hop as u64);
        let rel = |a: u64| (a - self.base) as usize;

        // o candidato cujo começo mais se parece com a continuação natural do bloco anterior (correlação normalizada)
        let template = &self.src[rel(self.natural)..rel(self.natural) + hop];
        let mut best = (lo, f32::MIN);
        for c in lo..=hi {
            let cand = &self.src[rel(c)..rel(c) + hop];
            let (dot, energy) = cand.iter().zip(template).fold((0.0f32, 1.0f32), |(d, e), (&x, &t)| (d + x * t, e + x * x));
            let score = dot / energy.sqrt();
            if score > best.1 {
                best = (c, score);
            }
        }
        let c = rel(best.0);
        out.extend((0..hop).map(|i| to_i16(self.tail[i] + self.rise[i] * self.src[c + i])));
        for i in 0..hop {
            self.tail[i] = self.fall[i] * self.src[c + hop + i];
        }
        self.natural = best.0 + hop as u64;
        self.nominal += step;

        // esquece a entrada que nenhuma busca futura vai olhar (aos poucos, para não copiar a cada bloco)
        let keep_from = self.natural.min((self.nominal.round() as u64).saturating_sub(self.search as u64));
        let drop = rel(keep_from.max(self.base));
        if drop > 8 * CHUNK {
            self.src.drain(..drop);
            self.base += drop as u64;
        }
        Some(self.nominal as u64)
    }
}

fn to_i16(v: f32) -> i16 {
    v.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}
