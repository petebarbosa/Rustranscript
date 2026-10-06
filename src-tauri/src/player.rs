//! Player de áudio da tela da chamada (issue #22), no shell Tauri. O motor (decodificar, misturar, tocar pelo
//! PulseAudio) é do núcleo (`core_lib::player`); aqui ficam o estado (uma chamada aberta por vez), os comandos da
//! UI e o evento de posição.
//!
//! - `player_open`: resolve o áudio da chamada (sob a trava do banco, só consultas) e sobe o motor, parado em 0 s.
//!   Sem áudio devolve `available: false` e o motivo (`deleted`/`none`/`missing`), sem erro: a tela explica.
//! - `player_peaks`: a onda sonora, em `buckets` valores 0–255. Roda **fora** da trava do banco: a 1ª vez em uma
//!   chamada de 2 h decodifica tudo (alguns segundos); depois vem do `peaks.bin` ao lado do áudio.
//! - `player_play`/`pause`/`seek`/`speed`/`close`: só mandam comandos ao motor.
//! - Cortes (#23): o player não toca os trechos cortados. `player_open` os lê do banco; depois de qualquer mudança
//!   (salvar/remover corte, excluir/restaurar trecho, desfazer) a UI chama `player_set_cuts`, que relê e atualiza o
//!   motor aberto sem mexer onde o ouvinte está.
//! - Evento `player-position` (~10 Hz tocando; a cada comando): `PositionEvent`.
use std::sync::{Arc, Mutex};

use core_lib::Error;
use core_lib::player::{self, NoAudio, Player, PlayerEvent, peaks};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::gui::{AppState, CmdError, R, with_app};

pub const EV_POSITION: &str = "player-position";

pub struct PlayerState {
    cur: Mutex<Option<Open>>,
}

struct Open {
    library_id: i64,
    call_id: i64,
    player: Player,
}

impl PlayerState {
    pub fn new() -> PlayerState {
        PlayerState { cur: Mutex::new(None) }
    }

    fn with<T>(&self, f: impl FnOnce(&Player) -> T) -> R<T> {
        match self.cur.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(o) => Ok(f(&o.player)),
            None => Err(CmdError { code: "invalid".into(), detail: "no call is open in the player".into() }),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionEvent {
    pub library_id: i64,
    pub call_id: i64,
    #[serde(flatten)]
    pub event: PlayerEvent,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerInfo {
    pub available: bool,
    /// `deleted` | `none` | `missing` quando `available` é falso.
    pub reason: Option<&'static str>,
    pub duration_s: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeaksReply {
    pub per_s: u32,
    pub duration_s: f64,
    /// 0–255 (o maior valor absoluto da mistura em cada faixa).
    pub data: Vec<u8>,
}

fn resolve(state: &State<AppState>, library_id: i64, call_id: i64) -> R<Result<player::CallAudio, NoAudio>> {
    with_app(state, |app| player::resolve(&app.open_library(library_id)?, call_id))
}

#[tauri::command(async)]
pub fn player_open(handle: AppHandle, state: State<AppState>, pl: State<PlayerState>, library_id: i64, call_id: i64) -> R<PlayerInfo> {
    // fecha a chamada anterior (e espera a thread acabar) antes de abrir outra
    let prev = pl.cur.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(prev);
    let audio = match resolve(&state, library_id, call_id)? {
        Ok(a) => a,
        Err(why) => return Ok(PlayerInfo { available: false, reason: Some(why.code()), duration_s: 0.0 }),
    };
    let cuts = effective_cuts(&state, library_id, call_id)?;
    let emit = {
        let handle = handle.clone();
        Arc::new(move |event: PlayerEvent| {
            let _ = handle.emit(EV_POSITION, PositionEvent { library_id, call_id, event });
        })
    };
    let player = Player::open(&audio, &cuts, recorder::default_sink_opener(), emit).map_err(|e| match e {
        // arquivo ilegível/cortado: a UI mostra como erro de áudio, não como "sem áudio"
        Error::Audio(d) => CmdError { code: "audio_decode".into(), detail: d },
        other => other.into(),
    })?;
    let info = PlayerInfo { available: true, reason: None, duration_s: player.duration_s };
    *pl.cur.lock().unwrap_or_else(|e| e.into_inner()) = Some(Open { library_id, call_id, player });
    Ok(info)
}

/// Cortes vivos da chamada, já fundidos (o que o player e o worker usam).
fn effective_cuts(state: &State<AppState>, library_id: i64, call_id: i64) -> R<Vec<(f64, f64)>> {
    with_app(state, |app| app.open_library(library_id)?.effective_cuts(call_id))
}

/// Relê os cortes da chamada e os aplica ao player aberto (se for dela). Sem player aberto não é erro.
#[tauri::command(async)]
pub fn player_set_cuts(state: State<AppState>, pl: State<PlayerState>, library_id: i64, call_id: i64) -> R<()> {
    let open = pl.cur.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|o| o.library_id == library_id && o.call_id == call_id);
    if !open {
        return Ok(());
    }
    let cuts = effective_cuts(&state, library_id, call_id)?;
    pl.with(|p| p.set_cuts(cuts))
}

#[tauri::command(async)]
pub fn player_peaks(state: State<AppState>, library_id: i64, call_id: i64, buckets: usize) -> R<PeaksReply> {
    let audio = resolve(&state, library_id, call_id)?.map_err(|_| CmdError { code: "no_audio".into(), detail: format!("call {library_id}:{call_id}") })?;
    let p = peaks::load_or_compute(&audio, |_, _| {})?;
    Ok(PeaksReply { per_s: p.per_s, duration_s: p.frames as f64 / f64::from(p.rate), data: peaks::downsample(&p.data, buckets.clamp(1, 20_000)) })
}

#[tauri::command(async)]
pub fn player_play(pl: State<PlayerState>) -> R<()> {
    pl.with(|p| p.play())
}

#[tauri::command(async)]
pub fn player_pause(pl: State<PlayerState>) -> R<()> {
    pl.with(|p| p.pause())
}

#[tauri::command(async)]
pub fn player_seek(pl: State<PlayerState>, seconds: f64) -> R<()> {
    pl.with(|p| p.seek(seconds))
}

#[tauri::command(async)]
pub fn player_speed(pl: State<PlayerState>, speed: f64) -> R<()> {
    pl.with(|p| p.set_speed(speed))
}

/// Fecha o player (a tela da chamada saiu, ou o áudio foi apagado). Sem nada aberto não é erro.
#[tauri::command(async)]
pub fn player_close(pl: State<PlayerState>, library_id: Option<i64>, call_id: Option<i64>) -> R<()> {
    let mut cur = pl.cur.lock().unwrap_or_else(|e| e.into_inner());
    // a tela de uma chamada não fecha o player que outra tela já abriu (navegação rápida entre chamadas)
    let same = cur.as_ref().is_some_and(|o| library_id.is_none_or(|l| l == o.library_id) && call_id.is_none_or(|c| c == o.call_id));
    let taken = if same { cur.take() } else { None };
    drop(cur);
    drop(taken);
    Ok(())
}
