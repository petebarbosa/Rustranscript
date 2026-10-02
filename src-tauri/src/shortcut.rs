//! Atalho global de gravação (agente B). Registrado **pelo Rust** (a UI não precisa de permissão do
//! plugin). Só funciona em X11: no Wayland (inclui XWayland) não registra e expõe
//! `shortcut_supported = false`; o usuário usa o `bind` do compositor + `rstt record toggle`.
use core_lib::recording::keys;
use serde::Serialize;
use tauri::{AppHandle, Manager, Wry, plugin::TauriPlugin};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::gui::{AppState, CmdError, R, with_app};
use crate::recording::{RecState, lock};

/// Acelerador padrão (sintaxe do Tauri). Setting `record_shortcut` ausente → este; `"none"` → desligado.
pub const DEFAULT_ACCELERATOR: &str = "Ctrl+Alt+R";

#[derive(Debug, Clone, Serialize)]
pub struct ShortcutInfo {
    /// Configurado (`null` = desligado).
    pub accelerator: Option<String>,
    pub supported: bool,
    /// `true` se o registro no sistema deu certo agora (pode falhar se outra app já usa a combinação).
    pub registered: bool,
    /// Motivo quando `supported && !registered`.
    pub error: Option<String>,
}

/// `"x11"` | `"wayland"` | `"unknown"`. Wayland = `XDG_SESSION_TYPE=wayland` **ou** `WAYLAND_DISPLAY` definida.
pub fn session_type() -> &'static str {
    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    if wayland {
        "wayland"
    } else if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) {
        "x11"
    } else {
        "unknown"
    }
}

/// Regra única para shell e UI: atalho global suportado ⇔ sessão X11.
pub fn supported() -> bool {
    session_type() == "x11"
}

/// O plugin (precisa ser registrado no `Builder`). O handler só reage ao pressionar.
pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                crate::recording::on_shortcut(app);
            }
        })
        .build()
}

/// Acelerador configurado: `None` = desligado (`"none"`); ausente = o padrão.
fn configured(app: &AppHandle) -> Option<String> {
    let state = app.state::<AppState>();
    let value = lock(&state.app).setting(keys::SHORTCUT).ok().flatten();
    match value.as_deref().map(str::trim) {
        None | Some("") => Some(DEFAULT_ACCELERATOR.to_string()),
        Some(v) if v.eq_ignore_ascii_case("none") => None,
        Some(v) => Some(v.to_string()),
    }
}

/// O que está registrado agora (para `record_info`).
pub fn current(app: &AppHandle) -> ShortcutInfo {
    lock(&app.state::<RecState>().shortcut).clone()
}

/// Registra `accelerator` (se houver e o ambiente suportar) e guarda o resultado no estado.
fn apply(app: &AppHandle, accelerator: Option<String>) -> ShortcutInfo {
    let mut info = ShortcutInfo { accelerator: accelerator.clone(), supported: supported(), registered: false, error: None };
    if let (true, Some(a)) = (info.supported, &accelerator) {
        match app.global_shortcut().register(a.as_str()) {
            Ok(()) => info.registered = true,
            Err(e) => info.error = Some(e.to_string()),
        }
    }
    *lock(&app.state::<RecState>().shortcut) = info.clone();
    info
}

/// Registra o atalho das configurações (ou o padrão) no startup. Sem suporte: não faz nada.
pub fn register_from_settings(app: &AppHandle) -> ShortcutInfo {
    apply(app, configured(app))
}

/// Troca o atalho: desregistra o antigo, valida e registra o novo, grava `record_shortcut` em
/// `settings` (`None` → `"none"`). Acelerador inválido → `invalid` (nada muda). Em Wayland só valida e
/// grava (`supported: false`). Falha ao registrar (combinação já usada) → `Ok` com `error`.
pub fn set(app: &AppHandle, accelerator: Option<String>) -> R<ShortcutInfo> {
    let accelerator = accelerator.map(|a| a.trim().to_string()).filter(|a| !a.is_empty() && !a.eq_ignore_ascii_case("none"));
    if let Some(a) = &accelerator {
        a.parse::<Shortcut>().map_err(|e| CmdError { code: "invalid".into(), detail: format!("shortcut {a:?}: {e}") })?;
    }
    if supported() {
        let _ = app.global_shortcut().unregister_all();
    }
    let stored = accelerator.clone().unwrap_or_else(|| "none".into());
    let state = app.state::<AppState>();
    with_app(&state, |a| a.set_setting(keys::SHORTCUT, Some(&stored)))?;
    Ok(apply(app, accelerator))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_accelerator_parses() {
        assert!(DEFAULT_ACCELERATOR.parse::<Shortcut>().is_ok());
        assert!("Ctrl+Shift+F9".parse::<Shortcut>().is_ok());
        assert!("Ctrl+Alt+K".parse::<Shortcut>().is_ok());
        assert!("Ctrl+Alt+".parse::<Shortcut>().is_err());
        assert!("banana".parse::<Shortcut>().is_err());
    }
}
