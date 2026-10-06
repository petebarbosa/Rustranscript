//! Mini barra (agente B): janela `"bar"` pequena, sem decoração, sempre no topo, fora da barra de
//! tarefas, escondida por padrão; carrega o mesmo front-end em `index.html#/bar`.
//!
//! Criação preguiçosa (`ensure`), para não carregar um 2º webview se o usuário nunca a usa. O título é
//! **fixo** (`BAR_TITLE`): a regra do Hyprland casa por ele. Capability: `capabilities/bar.json`.
//!
//! Esconder é `hide()` (o webview continua vivo, a próxima exibição é instantânea). Por isso o app
//! **não** sai sozinho quando a janela principal fecha: `shell` chama `exit` explicitamente.
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const BAR_LABEL: &str = "bar";
/// Título da janela (contrato com o trecho do Hyprland na UI: `title:^(tary-bar)$`).
pub const BAR_TITLE: &str = "tary-bar";
pub const BAR_URL: &str = "index.html#/bar";
/// Tamanho lógico.
pub const BAR_SIZE: (f64, f64) = (320.0, 56.0);
/// Distância da barra até a borda superior direita do monitor principal (px lógicos).
const MARGIN: f64 = 20.0;

/// Visibilidade conhecida pelo shell (só `show`/`hide` a alteram; a janela não tem decoração, então não
/// há outro jeito de o usuário fechá-la senão pelo gerenciador de janelas, tratado em `shell`).
static VISIBLE: AtomicBool = AtomicBool::new(false);

/// Cria a janela se ainda não existe (escondida) e a devolve.
pub fn ensure(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(w) = app.get_webview_window(BAR_LABEL) {
        return Ok(w);
    }
    let mut builder = WebviewWindowBuilder::new(app, BAR_LABEL, WebviewUrl::App(BAR_URL.into()))
        .title(BAR_TITLE)
        .inner_size(BAR_SIZE.0, BAR_SIZE.1)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .min_inner_size(BAR_SIZE.0, BAR_SIZE.1)
        .max_inner_size(BAR_SIZE.0, BAR_SIZE.1)
        .visible_on_all_workspaces(true)
        .focused(false)
        .visible(false);
    // canto superior direito do monitor principal (ignorado onde o compositor decide a posição)
    if let Ok(Some(m)) = app.primary_monitor() {
        let sf = m.scale_factor();
        let (pos, size) = (m.position(), m.size());
        let x = (pos.x as f64 + size.width as f64) / sf - BAR_SIZE.0 - MARGIN;
        let y = pos.y as f64 / sf + MARGIN;
        builder = builder.position(x.max(0.0), y);
    }
    builder.build()
}

/// Mostra a barra (cria se preciso), sem roubar o foco, em todos os espaços de trabalho.
/// Chame fora da thread principal quando puder criar a janela (menu do tray usa uma thread).
pub fn show(app: &AppHandle) {
    match ensure(app) {
        Ok(w) => {
            let _ = w.show();
            VISIBLE.store(true, Ordering::SeqCst);
        }
        Err(e) => eprintln!("bar: {e}"),
    }
    crate::recording::emit_state(app);
}

pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(BAR_LABEL) {
        let _ = w.hide();
    }
    VISIBLE.store(false, Ordering::SeqCst);
    crate::recording::emit_state(app);
}

pub fn is_visible(_app: &AppHandle) -> bool {
    VISIBLE.load(Ordering::SeqCst)
}
