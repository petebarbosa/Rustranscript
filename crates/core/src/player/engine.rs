//! Motor do player: uma thread por chamada aberta, dona da `Session` e da saída de áudio. A UI manda comandos
//! (tocar, pausar, pular, velocidade) por um canal e recebe `PlayerEvent`s (~10 por segundo tocando).
//!
//! A saída é aberta na primeira vez que se toca (uma chamada só olhada não cria fluxo no mixer do sistema) e
//! dentro da thread (o `Simple` do libpulse não é `Send`). O relógio do player é o `write` da saída: ele bloqueia
//! enquanto o buffer do servidor (~200 ms) estiver cheio.
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use recorder::{PlaybackSink, SinkOpener};
use serde::Serialize;

use super::session::{Session, clamp_speed};
use super::source::CallAudio;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayState {
    Paused,
    Playing,
    /// Tocou até o fim (a posição fica na duração; tocar de novo recomeça).
    Ended,
    /// A saída falhou (servidor de áudio sumiu...): ver `error`.
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerEvent {
    pub state: PlayState,
    pub position_s: f64,
    pub duration_s: f64,
    pub speed: f64,
    /// `{code, detail}` quando `state == Error`.
    pub error: Option<(String, String)>,
}

pub type EventFn = Arc<dyn Fn(PlayerEvent) + Send + Sync>;

#[derive(Debug)]
enum Cmd {
    Play,
    Pause,
    Seek(f64),
    Speed(f64),
    /// Cortes de áudio (#23) em segundos da linha original: troca os que o player pula.
    Cuts(Vec<(f64, f64)>),
    Close,
}

/// Quanto de saída se entrega ao servidor por vez (e a cadência dos eventos de posição).
const CHUNK: Duration = Duration::from_millis(100);

pub struct Player {
    tx: Sender<Cmd>,
    join: Option<JoinHandle<()>>,
    pub duration_s: f64,
}

impl Player {
    /// Abre os FLACs e sobe a thread, parada em 0 s, pulando `cuts` (segundos; vazio = tudo). Erro = arquivos ilegíveis.
    pub fn open(audio: &CallAudio, cuts: &[(f64, f64)], opener: SinkOpener, on_event: EventFn) -> Result<Player> {
        Player::with_session(Session::with_cuts(audio.open_mixer()?, cuts), opener, on_event)
    }

    pub fn with_session(session: Session, opener: SinkOpener, on_event: EventFn) -> Result<Player> {
        let duration_s = session.duration_s();
        let (tx, rx) = channel();
        let join = std::thread::Builder::new()
            .name("tary-player".into())
            .spawn(move || Engine { session, opener, sink: None, state: PlayState::Paused, on_event, last_emit: Instant::now() }.run(rx))
            .map_err(Error::Io)?;
        Ok(Player { tx, join: Some(join), duration_s })
    }

    pub fn play(&self) {
        let _ = self.tx.send(Cmd::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(Cmd::Seek(secs));
    }

    pub fn set_speed(&self, speed: f64) {
        let _ = self.tx.send(Cmd::Speed(speed));
    }

    /// Troca os cortes que o player pula, sem parar: continua de onde estava (se caiu num corte, no fim dele).
    pub fn set_cuts(&self, cuts: Vec<(f64, f64)>) {
        let _ = self.tx.send(Cmd::Cuts(cuts));
    }

    /// Para e espera a thread acabar.
    pub fn close(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.tx.send(Cmd::Close);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Engine {
    session: Session,
    opener: SinkOpener,
    sink: Option<Box<dyn PlaybackSink>>,
    state: PlayState,
    on_event: EventFn,
    last_emit: Instant,
}

impl Engine {
    fn run(mut self, rx: Receiver<Cmd>) {
        let mut buf: Vec<i16> = Vec::new();
        loop {
            // parado: espera um comando; tocando: só olha se há (o `write` já dá o ritmo)
            let cmd = if self.state == PlayState::Playing {
                match rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            };
            match cmd {
                Some(Cmd::Close) => return,
                Some(c) => self.command(c),
                None => {}
            }
            if self.state == PlayState::Playing {
                self.step(&mut buf);
            }
        }
    }

    fn emit(&mut self, error: Option<(String, String)>) {
        let pos = match self.state {
            PlayState::Ended => self.session.duration_s(),
            _ => self.position_s().min(self.session.duration_s()),
        };
        self.last_emit = Instant::now();
        (self.on_event)(PlayerEvent { state: self.state, position_s: pos, duration_s: self.session.duration_s(), speed: self.session.speed(), error });
    }

    /// O que está tocando agora: o que foi produzido menos o que o servidor ainda guarda.
    fn position_s(&self) -> f64 {
        self.session.position_s(self.sink.as_ref().and_then(|s| s.latency()))
    }

    /// Para o som na hora e deixa a sessão exatamente onde o ouvinte estava (pausar e mudar a velocidade).
    fn settle(&mut self) -> u64 {
        let at = (self.position_s() * f64::from(self.session.rate())).round() as u64;
        if let Some(s) = self.sink.as_mut() {
            let _ = s.flush();
        }
        at
    }

    fn fail(&mut self, e: impl std::fmt::Display, code: &str) {
        let at = self.settle();
        self.session.seek(at);
        self.sink = None;
        self.state = PlayState::Error;
        self.emit(Some((code.to_string(), e.to_string())));
        self.state = PlayState::Paused;
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Close => {}
            Cmd::Play => {
                if self.state == PlayState::Playing {
                    return;
                }
                if self.sink.is_none() {
                    match (self.opener)(self.session.rate()) {
                        Ok(s) => self.sink = Some(s),
                        Err(e) => return self.fail(e.detail(), e.code()),
                    }
                }
                if self.state == PlayState::Ended || self.session.position(0) >= self.session.len() {
                    self.session.seek(0);
                }
                self.state = PlayState::Playing;
                self.emit(None);
            }
            Cmd::Pause => {
                if self.state != PlayState::Playing {
                    return;
                }
                let at = self.settle();
                self.session.seek(at);
                self.state = PlayState::Paused;
                self.emit(None);
            }
            Cmd::Seek(secs) => {
                let secs = if secs.is_finite() { secs.clamp(0.0, self.session.duration_s()) } else { 0.0 };
                if let Some(s) = self.sink.as_mut() {
                    let _ = s.flush();
                }
                self.session.seek_s(secs);
                if self.state == PlayState::Ended {
                    self.state = PlayState::Paused;
                }
                self.emit(None);
            }
            Cmd::Cuts(cuts) => {
                let at = if self.state == PlayState::Playing { self.settle() } else { (self.position_s() * f64::from(self.session.rate())).round() as u64 };
                self.session.set_cuts(&cuts, at);
                self.emit(None);
            }
            Cmd::Speed(v) => {
                let v = clamp_speed(v);
                let at = if self.state == PlayState::Playing { self.settle() } else { (self.position_s() * f64::from(self.session.rate())).round() as u64 };
                self.session.set_speed(v, at);
                self.emit(None);
            }
        }
    }

    /// Entrega ~100 ms de saída; os eventos de posição saem na mesma cadência.
    fn step(&mut self, buf: &mut Vec<i16>) {
        let want = (CHUNK.as_secs_f64() * f64::from(self.session.rate())) as usize;
        buf.clear();
        let eos = self.session.next_chunk(buf, want);
        if !buf.is_empty()
            && let Some(sink) = self.sink.as_mut()
            && let Err(e) = sink.write(buf)
        {
            return self.fail(e.detail(), e.code());
        }
        if eos {
            if let Some(sink) = self.sink.as_mut() {
                let _ = sink.drain();
            }
            self.state = PlayState::Ended;
            self.emit(None);
        } else if self.last_emit.elapsed() >= CHUNK {
            self.emit(None);
        }
    }
}
