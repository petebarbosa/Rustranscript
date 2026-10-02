//! Ganchos do ciclo de vida do app (agente B): fechamento protegido durante a gravação e segunda
//! abertura (instância única).
//!
//! Decisões (contrato 10.4):
//! - **Janela principal fechada, ocioso**: o app sai (como na fase 2). A barra escondida e o tray não
//!   mantêm o processo vivo; para isso `exit` é chamado de forma explícita (uma janela escondida
//!   ainda conta como janela aberta para o Tauri).
//! - **Janela principal fechada, gravando**: não sai; **pergunta** (diálogo nativo, 3 botões): esconder
//!   e continuar gravando (a janela esconde e a barra aparece para o usuário ter o controle), parar e
//!   sair (para pelo fluxo normal, espera a conversão terminar e só então sai) ou cancelar.
//! - **Janela principal fechada, só finalizando**: não sai e não pergunta — a janela só esconde (a
//!   conversão segue). Sair de verdade é o item "Sair" do tray (que confirma). `RunEvent::ExitRequested`
//!   sem código (última janela fechada) é impedido enquanto ocupado.
//! - **Início sem janela**: `ipc::spawn_gui` define `RSTT_HIDDEN`; a janela principal (criada
//!   invisível em `tauri.conf.json`) só é mostrada no `setup` se a variável não existir.
//! - **Segunda abertura**: gravando → mostra a barra; senão traz a janela principal para a frente.
use tauri::{AppHandle, Manager, RunEvent, Window, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};

use crate::recording::{self, RecState};
use crate::{bar, i18n, ipc};

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
            if rec.is_recording() {
                // a janela segue aberta até a escolha; o diálogo não bloqueia a thread de eventos
                let app = app.clone();
                std::thread::spawn(move || confirm_close_while_recording(&app));
            } else if rec.is_busy() {
                hide_main_keep_recording(app);
            } else {
                let app = app.clone();
                std::thread::spawn(move || recording::exit_now(&app));
            }
        }
        _ => {}
    }
}

/// Esconde a janela principal e, gravando, mostra a barra (o controle da gravação sem a janela).
fn hide_main_keep_recording(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if !bar::is_visible(&app) && app.state::<RecState>().is_recording() {
            bar::show(&app);
        }
    });
}

enum CloseChoice {
    Hide,
    Stop,
    Cancel,
}

/// Fechar a janela principal gravando: pergunta (esconder / parar e sair / cancelar). Roda em thread
/// própria; o diálogo nativo é assíncrono (o resultado chega no callback).
fn confirm_close_while_recording(app: &AppHandle) {
    let rec = app.state::<RecState>();
    if !rec.begin_close_prompt() {
        return; // já há um diálogo aberto
    }
    if !rec.is_recording() {
        // a gravação terminou enquanto isso: segue o fluxo sem gravação
        rec.end_close_prompt();
        if rec.is_busy() {
            hide_main_keep_recording(app);
        } else {
            recording::exit_now(app);
        }
        return;
    }
    let l = recording::lang(app);
    let (hide, stop) = (i18n::msg(l, "close_hide"), i18n::msg(l, "close_stop"));
    let h = app.clone();
    app.dialog()
        .message(i18n::msg(l, "close_recording"))
        .title(i18n::msg(l, "close_title"))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(hide.into(), stop.into(), i18n::msg(l, "quit_cancel").into()))
        .show_with_result(move |res| {
            // o plugin devolve o rótulo do botão (`Custom`); `Yes`/`No` cobrem plataformas que não o fazem
            let choice = match res {
                MessageDialogResult::Yes => CloseChoice::Hide,
                MessageDialogResult::No => CloseChoice::Stop,
                MessageDialogResult::Custom(s) if s == hide => CloseChoice::Hide,
                MessageDialogResult::Custom(s) if s == stop => CloseChoice::Stop,
                _ => CloseChoice::Cancel,
            };
            // `show_with_result` roda o callback numa thread do plugin: o trabalho demorado vai para outra
            std::thread::spawn(move || {
                h.state::<RecState>().end_close_prompt();
                match choice {
                    CloseChoice::Hide => hide_main_keep_recording(&h),
                    CloseChoice::Cancel => {}
                    CloseChoice::Stop => stop_and_quit(&h),
                }
            });
        });
}

/// "Parar e sair": para pelo fluxo normal (`do_stop`: a conversão roda em segundo plano), espera a
/// conversão terminar e só então sai. A janela fica aberta mostrando o andamento. Se parar falhar, a
/// gravação continua e o erro é mostrado.
fn stop_and_quit(app: &AppHandle) {
    match recording::do_stop(app) {
        Ok(_) => {}
        // terminou sozinha entre o diálogo e o clique: só falta esperar
        Err(e) if e.code == "not_recording" => {}
        Err(e) => return recording::report_error(app, &e),
    }
    app.state::<RecState>().wait_finalized();
    recording::exit_now(app);
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
