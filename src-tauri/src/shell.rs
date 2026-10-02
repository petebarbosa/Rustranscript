//! Ganchos do ciclo de vida do app (agente B): fechamento protegido durante a gravação e segunda
//! abertura (instância única).
//!
//! Decisões (contrato 10.4):
//! - **Janela principal fechada, ocioso**: o app sai (como na fase 2). A barra escondida e o tray não
//!   mantêm o processo vivo; para isso `exit` é chamado de forma explícita (uma janela escondida
//!   ainda conta como janela aberta para o Tauri).
//! - **Janela principal fechada, gravando ou finalizando**: não sai e **não pergunta** — a janela só
//!   esconde, a gravação segue, e a barra aparece para o usuário ter o controle. Sair de verdade é o
//!   item "Sair" do tray (que confirma). `RunEvent::ExitRequested` sem código (última janela fechada)
//!   é impedido enquanto ocupado.
//! - **Início sem janela**: `ipc::spawn_gui` define `TRANSCRICOES_HIDDEN`; a janela principal (criada
//!   invisível em `tauri.conf.json`) só é mostrada no `setup` se a variável não existir.
//! - **Segunda abertura**: gravando → mostra a barra; senão traz a janela principal para a frente.
use tauri::{AppHandle, Manager, RunEvent, Window, WindowEvent};

use crate::recording::{self, RecState};
use crate::{bar, ipc};

/// A GUI foi aberta sem janela (pela CLI/atalho)?
pub fn started_hidden() -> bool {
    std::env::var_os(ipc::HIDDEN_ENV).is_some_and(|v| !v.is_empty())
}

/// `CloseRequested` das janelas. Barra: esconde (não destrói). Principal: ver o topo do arquivo.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else { return };
    let app = window.app_handle();
    match window.label() {
        bar::BAR_LABEL => {
            // fechada pelo gerenciador de janelas: só esconde
            api.prevent_close();
            let app = app.clone();
            std::thread::spawn(move || bar::hide(&app));
        }
        "main" => {
            let rec = app.state::<RecState>();
            // o monitor de níveis pertence à tela de gravar: sai junto com a janela
            recording::stop_monitor(&rec);
            api.prevent_close();
            if rec.is_busy() {
                let _ = window.hide();
                let app = app.clone();
                // sem a janela, a barra é o controle da gravação em andamento
                std::thread::spawn(move || {
                    if !bar::is_visible(&app) && app.state::<RecState>().is_recording() {
                        bar::show(&app);
                    }
                });
            } else {
                let app = app.clone();
                std::thread::spawn(move || recording::exit_now(&app));
            }
        }
        _ => {}
    }
}

/// `RunEvent::ExitRequested` (inclui "Sair" da bandeja): ocupado e sem saída confirmada, `prevent_exit`
/// (`code == None` = fechar janelas; `Some(_)` = `AppHandle::exit`, só chamado depois da confirmação).
pub fn on_run_event(app: &AppHandle, event: &RunEvent) {
    match event {
        RunEvent::ExitRequested { code, api, .. } => {
            let rec = app.state::<RecState>();
            if code.is_none() && rec.is_busy() && !rec.is_quitting() {
                api.prevent_exit();
            }
        }
        RunEvent::Exit => recording::shutdown(app),
        _ => {}
    }
}

/// Traz a janela principal para a frente (cria o efeito de "abrir o app").
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        // No X11/Cinnamon, `unminimize` + `set_focus` (sem timestamp de usuário) não tiram a janela
        // do estado minimizado; esconder e mostrar de novo a remapeia como janela normal.
        if w.is_minimized().unwrap_or(false) {
            let _ = w.hide();
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Segunda abertura do app: gravando → mostrar a barra (`bar::show`); senão traz a janela principal
/// para a frente (comportamento da fase 1).
pub fn on_second_instance(app: &AppHandle) {
    if app.state::<RecState>().is_recording() {
        let app = app.clone();
        std::thread::spawn(move || bar::show(&app));
    } else {
        show_main(app);
    }
}
