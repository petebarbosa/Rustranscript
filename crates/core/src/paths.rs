//! Onde ficam os dados. O nome da app ainda é provisório; trocar `APP_NAME` muda os diretórios.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "transcricoes";
pub const DATA_DIR_ENV: &str = "TRANSCRICOES_DATA_DIR";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Diretório de dados escolhido pelo usuário (sobrescreve o padrão do SO).
    pub data_dir: Option<PathBuf>,
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", APP_NAME)
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

/// Ordem: variável de ambiente → config.json → padrão do SO (`~/.local/share/transcricoes`).
pub fn resolve_data_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(p) = load_config().data_dir {
        return p;
    }
    project_dirs()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(APP_NAME))
}

/// Diretório só do usuário para o socket local (`$XDG_RUNTIME_DIR/transcricoes`).
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p).join(APP_NAME),
        None => data_dir.join("run"),
    }
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join("ipc.sock")
}
