// Sem console extra no Windows em release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bar;
mod cli;
mod gui;
mod help;
mod i18n;
mod ipc;
mod player;
mod recording;
mod shell;
mod shortcut;
mod transcription;
mod tray;

fn main() {
    // Sem argumentos: abre a janela. Com argumentos: CLI, decidida antes de qualquer coisa do
    // Tauri, para que `rstt list` com a app aberta não seja engolido pela instância única
    // e funcione sem tela.
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() <= 1 {
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) && ipc::spawn_window_detached().is_ok() {
            return;
        }
        gui::run(None);
    } else {
        std::process::exit(cli::run(args));
    }
}
