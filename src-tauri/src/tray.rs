//! Ícone de bandeja (agente B). **Linux: só menu** (cliques no ícone não geram evento; sem tooltip):
//! itens Gravar/Parar (um só, alterna), Mostrar/Esconder barra, Abrir janela, Sair. Rótulos traduzidos
//! pelo idioma das configurações (`i18n.rs`); os textos dos itens são trocados (`set_text`) quando o
//! estado ou o idioma muda — o menu em si nunca é refeito (no Linux ele não pode ser removido).
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::i18n::{self, Lang};
use crate::{recording, shell};

const ID_RECORD: &str = "record";
const ID_BAR: &str = "bar";
const ID_OPEN: &str = "open";
const ID_QUIT: &str = "quit";

/// Itens cujo texto muda; guardados no estado do app.
struct TrayItems {
    record: MenuItem<Wry>,
    bar: MenuItem<Wry>,
    open: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

/// Cria o ícone (`TrayIconBuilder` com `app.default_window_icon()`) e o menu inicial.
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let lang = recording::lang(app);
    let item = |id: &str, key: &str| MenuItem::with_id(app, id, i18n::msg(lang, key), true, None::<&str>);
    let items = TrayItems {
        record: item(ID_RECORD, "tray_record")?,
        bar: item(ID_BAR, "tray_bar_show")?,
        open: item(ID_OPEN, "tray_open")?,
        quit: item(ID_QUIT, "tray_quit")?,
    };
    let menu = Menu::with_items(
        app,
        &[&items.record, &items.bar, &PredefinedMenuItem::separator(app)?, &items.open, &items.quit],
    )?;
    let mut builder = TrayIconBuilder::with_id("main").menu(&menu).show_menu_on_left_click(true).on_menu_event(|app, event| {
        // roda na thread principal: o trabalho (abrir dispositivos, criar janela, diálogo) vai para outra
        match event.id().as_ref() {
            ID_RECORD => recording::on_tray_toggle(app),
            ID_BAR => recording::on_tray_bar(app),
            ID_OPEN => shell::show_main(app),
            ID_QUIT => {
                let app = app.clone();
                std::thread::spawn(move || recording::request_quit(&app));
            }
            _ => {}
        }
    });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    app.manage(items);
    refresh(app);
    Ok(())
}

/// Atualiza os rótulos para o estado atual (gravando ⇒ "Parar gravação"; barra visível ⇒ "Esconder
/// barra") e o idioma.
pub fn refresh(app: &AppHandle) {
    let Some(items) = app.try_state::<TrayItems>() else { return };
    let lang: Lang = recording::lang(app);
    let status = recording::status(app);
    let _ = items.record.set_text(i18n::msg(lang, if status.state == "recording" { "tray_stop" } else { "tray_record" }));
    let _ = items.bar.set_text(i18n::msg(lang, if status.bar_visible { "tray_bar_hide" } else { "tray_bar_show" }));
    let _ = items.open.set_text(i18n::msg(lang, "tray_open"));
    let _ = items.quit.set_text(i18n::msg(lang, "tray_quit"));
}
