//! De onde vem o áudio de uma chamada: resolve `mic.flac`/`sys.flac` no disco (caminhos relativos à raiz da
//! biblioteca), o deslocamento mic×sys do sidecar e o motivo de não haver áudio (para a UI explicar).
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;

use super::mixer::Mixer;
use crate::{Error, Library, Result};

/// Por que a chamada não tem o que tocar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoAudio {
    /// O áudio foi apagado (`calls.audio_deleted_at`).
    Deleted,
    /// Nenhuma trilha registrada (importada sem áudio).
    None,
    /// Há trilhas registradas, mas os arquivos não estão no disco (importada sem copiar o áudio).
    Missing,
}

impl NoAudio {
    /// Código estável que a UI traduz.
    pub fn code(self) -> &'static str {
        match self {
            NoAudio::Deleted => "deleted",
            NoAudio::None => "none",
            NoAudio::Missing => "missing",
        }
    }
}

/// As trilhas que existem de fato no disco.
#[derive(Debug, Clone)]
pub struct CallAudio {
    pub sys: Option<PathBuf>,
    pub mic: Option<PathBuf>,
    /// Quanto o mic começou depois do sys (s); 0 sem sidecar ou sem as duas trilhas.
    pub mic_offset_s: f64,
    /// Pasta das trilhas (onde mora o cache dos picos).
    pub dir: PathBuf,
}

impl CallAudio {
    pub fn open_mixer(&self) -> Result<Mixer> {
        Mixer::open(self.sys.as_deref(), self.mic.as_deref(), self.mic_offset_s)
    }
}

/// Só consulta o banco e o disco (sem decodificar nada): pode rodar com o banco aberto, rápido.
pub fn resolve(lib: &Library, call_id: i64) -> Result<std::result::Result<CallAudio, NoAudio>> {
    let (mic, sys, dir, deleted): (Option<String>, Option<String>, Option<String>, bool) = lib
        .conn
        .query_row("SELECT mic_path, sys_path, dir, audio_deleted_at IS NOT NULL FROM calls WHERE id = ?1", [call_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?
        .ok_or_else(|| Error::not_found(format!("call {}:{call_id}", lib.id())))?;
    if deleted {
        return Ok(Err(NoAudio::Deleted));
    }
    if mic.is_none() && sys.is_none() {
        return Ok(Err(NoAudio::None));
    }
    let existing = |p: Option<String>| p.map(|p| lib.audio_abs(&p)).filter(|p| p.is_file());
    let (mic, sys) = (existing(mic), existing(sys));
    let Some(first) = sys.as_deref().or(mic.as_deref()) else { return Ok(Err(NoAudio::Missing)) };
    let audio_dir = first.parent().map_or_else(|| lib.root().to_path_buf(), Path::to_path_buf);
    let sidecar_dir = dir.map_or_else(|| audio_dir.clone(), |d| lib.root().join(d));
    let mic_offset_s = if mic.is_some() && sys.is_some() {
        recorder::Sidecar::read(&sidecar_dir).ok().and_then(|s| s.mic_offset_ms()).map_or(0.0, |ms| ms as f64 / 1000.0)
    } else {
        0.0
    };
    Ok(Ok(CallAudio { sys, mic, mic_offset_s, dir: audio_dir }))
}
