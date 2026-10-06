//! Onde ficam os dados (config, diretório de dados e socket local).
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "transcriptary";
pub const DATA_DIR_ENV: &str = "TARY_DATA_DIR";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Diretório de dados escolhido pelo usuário (sobrescreve o padrão do SO).
    pub data_dir: Option<PathBuf>,
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", APP_NAME)
}

/// `$XDG_DATA_HOME` (padrão `~/.local/share`): onde o desktop procura `applications/` e `icons/`.
pub fn xdg_data_home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.data_dir().to_path_buf())
}

pub fn config_file() -> PathBuf {
    project_dirs()
        .map(|d| d.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from(".").join(format!("{APP_NAME}.config.json")))
}

pub fn load_config() -> Config {
    std::fs::read_to_string(config_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(cfg: &Config) -> crate::Result<()> {
    let path = config_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::fsx::write_atomic(&path, serde_json::to_string_pretty(cfg)?.as_bytes())
}

/// `TARY_DATA_DIR`, se definida e não vazia.
fn env_data_dir() -> Option<OsString> {
    std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty())
}

/// Ordem: argumento → variável de ambiente → config.json → padrão do SO (`~/.local/share/transcriptary`).
pub fn resolve_data_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = env_data_dir() {
        return PathBuf::from(p);
    }
    if let Some(p) = load_config().data_dir {
        return p;
    }
    project_dirs()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(APP_NAME))
}

/// Diretório só do usuário para o socket local (`$XDG_RUNTIME_DIR/transcriptary`).
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p).join(APP_NAME),
        None => data_dir.join("run"),
    }
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join("ipc.sock")
}
