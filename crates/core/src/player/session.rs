//! Estado puro de uma reprodução (sem thread, sem saída de áudio): mixer + velocidade + conta de posição.
//! Tudo o que decide "que som sai" e "onde estamos" mora aqui, para os testes não dependerem de relógio nem de
//! placa de som; o motor (`engine.rs`) só liga isto a uma saída de verdade.
use std::collections::VecDeque;

use super::mixer::Mixer;
use super::stretch::Stretcher;

/// Velocidades aceitas (fora disto, a mais próxima).
pub const MIN_SPEED: f64 = 0.5;
pub const MAX_SPEED: f64 = 2.0;

pub fn clamp_speed(v: f64) -> f64 {
    if v.is_finite() { v.clamp(MIN_SPEED, MAX_SPEED) } else { 1.0 }
}

/// Liga "amostras de saída entregues" a "posição na entrada": cada bloco produzido deixa uma marca
/// `(saída acumulada, entrada acumulada)`; a posição do que está *tocando* é a interpolação entre as marcas
/// que cercam a saída já tocada (entregue − latência do servidor).
#[derive(Debug, Clone)]
pub struct PositionTracker {
    marks: VecDeque<(u64, u64)>,
}

impl PositionTracker {
    pub fn new(start: u64) -> PositionTracker {
        PositionTracker { marks: VecDeque::from([(0, start)]) }
    }

    pub fn push(&mut self, out_total: u64, src_pos: u64) {
        if self.marks.back().is_some_and(|&(o, _)| o >= out_total) {
            return;
        }
        self.marks.push_back((out_total, src_pos));
        // o que já tocou há muito tempo não é mais consultado (a latência é de décimos de segundo)
        while self.marks.len() > 512 {
            self.marks.pop_front();
        }
    }

    /// Posição de entrada (amostras) quando `played` amostras de saída já tocaram.
    pub fn at(&self, played: u64) -> u64 {
        let (mut prev, mut next) = (self.marks[0], None);
        for &m in self.marks.iter().skip(1) {
            if m.0 <= played {
                prev = m;
            } else {
                next = Some(m);
                break;
            }
        }
        match next {
            Some((o1, s1)) => {
                let (o0, s0) = prev;
                s0 + ((s1 - s0.min(s1)) as f64 * (played - o0) as f64 / (o1 - o0) as f64) as u64
            }
            None => prev.1,
        }
    }
}

pub struct Session {
    mixer: Mixer,
    stretch: Stretcher,
    speed: f64,
    /// Saída produzida desde o último `seek`.
    out_total: u64,
    tracker: PositionTracker,
    /// Entrada que `next_chunk` ainda vai ler: já passou do fim da chamada.
    finished: bool,
    block: Vec<i16>,
}

impl Session {
    pub fn new(mixer: Mixer) -> Session {
        let stretch = Stretcher::new(mixer.rate(), mixer.len());
        Session { mixer, stretch, speed: 1.0, out_total: 0, tracker: PositionTracker::new(0), finished: false, block: Vec::new() }
    }

    pub fn rate(&self) -> u32 {
        self.mixer.rate()
    }

    /// Duração da chamada, em amostras.
    pub fn len(&self) -> u64 {
        self.mixer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mixer.is_empty()
    }

    pub fn duration_s(&self) -> f64 {
        self.mixer.len() as f64 / f64::from(self.mixer.rate())
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Muda a velocidade a partir de `at` (a posição que está tocando agora): quem chama já descartou o que havia
    /// no buffer da saída. A 1× o áudio passa direto; acima disso (ou abaixo), WSOLA.
    pub fn set_speed(&mut self, speed: f64, at: u64) {
        self.speed = clamp_speed(speed);
        self.stretch.set_speed(self.speed);
        self.seek(at);
    }

    /// Posiciona na amostra `pos` (do eixo da chamada) e zera a conta da saída.
    pub fn seek(&mut self, pos: u64) {
        let pos = pos.min(self.mixer.len());
        self.stretch.reset(&mut self.mixer, pos);
        self.out_total = 0;
        self.tracker = PositionTracker::new(pos);
        self.finished = pos >= self.mixer.len();
    }

    pub fn seek_s(&mut self, secs: f64) {
        let pos = (secs.max(0.0) * f64::from(self.rate())).round() as u64;
        self.seek(pos);
    }

    /// Acrescenta a `out` pelo menos `want` amostras de saída (menos só no fim da chamada). `true` = chegou ao fim:
    /// depois disto não há mais o que tocar.
    pub fn next_chunk(&mut self, out: &mut Vec<i16>, want: usize) -> bool {
        let start = out.len();
        while out.len() - start < want && !self.finished {
            if self.speed == 1.0 {
                let n = want - (out.len() - start);
                let at = out.len();
                out.resize(at + n, 0);
                let got = self.mixer.read(&mut out[at..]);
                out.truncate(at + got);
                self.out_total += got as u64;
                self.tracker.push(self.out_total, self.mixer.pos());
                if got < n {
                    self.finished = true;
                }
            } else {
                self.block.clear();
                match self.stretch.next_block(&mut self.mixer, &mut self.block) {
                    Some(src_pos) => {
                        out.extend_from_slice(&self.block);
                        self.out_total += self.block.len() as u64;
                        self.tracker.push(self.out_total, src_pos.min(self.mixer.len()));
                    }
                    None => self.finished = true,
                }
            }
        }
        self.finished
    }

    /// Posição que está tocando (amostras), dado quanto da saída produzida ainda não tocou (`pending`).
    pub fn position(&self, pending: u64) -> u64 {
        self.tracker.at(self.out_total.saturating_sub(pending))
    }

    /// O mesmo em segundos, com a latência da saída.
    pub fn position_s(&self, latency: Option<std::time::Duration>) -> f64 {
        let pending = latency.map_or(0, |l| (l.as_secs_f64() * f64::from(self.rate())) as u64);
        self.position(pending) as f64 / f64::from(self.rate())
    }
}
