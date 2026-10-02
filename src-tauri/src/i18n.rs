//! Mensagens da CLI. As da interface ficam em `ui/src/locales/*.json`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lang {
    PtBr,
    EnUs,
    Es419,
}

impl Lang {
    pub fn parse(tag: &str) -> Option<Lang> {
        let t = tag.to_lowercase().replace('_', "-");
        if t.starts_with("pt") {
            Some(Lang::PtBr)
        } else if t.starts_with("es") {
            Some(Lang::Es419)
        } else if t.starts_with("en") {
            Some(Lang::EnUs)
        } else {
            None
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Lang::PtBr => "pt-BR",
            Lang::EnUs => "en-US",
            Lang::Es419 => "es-419",
        }
    }

    /// Idioma do sistema; inglês se não reconhecer.
    pub fn system() -> Lang {
        sys_locale::get_locale().as_deref().and_then(Lang::parse).unwrap_or(Lang::EnUs)
    }
}

pub fn error_prefix(lang: Lang, code: &str) -> &'static str {
    use Lang::*;
    match (code, lang) {
        ("not_found", PtBr) => "não encontrado",
        ("not_found", EnUs) => "not found",
        ("not_found", Es419) => "no encontrado",
        ("ambiguous", PtBr) => "referência ambígua",
        ("ambiguous", EnUs) => "ambiguous reference",
        ("ambiguous", Es419) => "referencia ambigua",
        ("invalid", PtBr) => "entrada inválida",
        ("invalid", EnUs) => "invalid input",
        ("invalid", Es419) => "entrada no válida",
        ("conflict", PtBr) => "conflito",
        ("conflict", EnUs) => "conflict",
        ("conflict", Es419) => "conflicto",
        ("schema_too_new", PtBr) => "o banco foi criado por uma versão mais nova da app",
        ("schema_too_new", EnUs) => "the database was created by a newer version of the app",
        ("schema_too_new", Es419) => "la base fue creada por una versión más nueva de la app",
        ("audio", PtBr) => "áudio",
        ("audio", EnUs) => "audio",
        ("audio", Es419) => "audio",
        ("already_recording", PtBr) => "já está gravando",
        ("already_recording", EnUs) => "already recording",
        ("already_recording", Es419) => "ya está grabando",
        ("not_recording", PtBr) => "não está gravando",
        ("not_recording", EnUs) => "not recording",
        ("not_recording", Es419) => "no está grabando",
        ("not_running", PtBr) => "a app não está aberta",
        ("not_running", EnUs) => "the app is not running",
        ("not_running", Es419) => "la app no está abierta",
        ("device_not_found", PtBr) => "dispositivo de áudio não encontrado",
        ("device_not_found", EnUs) => "audio device not found",
        ("device_not_found", Es419) => "dispositivo de audio no encontrado",
        ("device_open_failed", PtBr) => "não foi possível abrir o dispositivo de áudio",
        ("device_open_failed", EnUs) => "could not open the audio device",
        ("device_open_failed", Es419) => "no se pudo abrir el dispositivo de audio",
        ("backend_unavailable", PtBr) => "servidor de áudio indisponível",
        ("backend_unavailable", EnUs) => "audio server unavailable",
        ("backend_unavailable", Es419) => "servidor de audio no disponible",
        ("capture_failed", PtBr) => "falha na captura de áudio",
        ("capture_failed", EnUs) => "audio capture failed",
        ("capture_failed", Es419) => "falló la captura de audio",
        ("invalid_wav", PtBr) => "arquivo WAV inválido",
        ("invalid_wav", EnUs) => "invalid WAV file",
        ("invalid_wav", Es419) => "archivo WAV no válido",
        ("empty_recording", PtBr) => "a gravação não tem áudio",
        ("empty_recording", EnUs) => "the recording has no audio",
        ("empty_recording", Es419) => "la grabación no tiene audio",
        ("bad_request", PtBr) => "requisição inválida",
        ("bad_request", EnUs) => "bad request",
        ("bad_request", Es419) => "solicitud no válida",
        ("not_implemented", PtBr) => "ainda não implementado",
        ("not_implemented", EnUs) => "not implemented yet",
        ("not_implemented", Es419) => "aún no implementado",
        (_, PtBr) => "erro",
        (_, EnUs) => "error",
        (_, Es419) => "error",
    }
}

/// Mensagem traduzida; ausente no idioma → inglês.
pub fn msg(lang: Lang, key: &str) -> &'static str {
    match lookup(lang, key) {
        "" if lang != Lang::EnUs => lookup(Lang::EnUs, key),
        s => s,
    }
}

fn lookup(lang: Lang, key: &str) -> &'static str {
    use Lang::*;
    match (key, lang) {
        ("nothing_to_undo", PtBr) => "nada para desfazer",
        ("nothing_to_undo", EnUs) => "nothing to undo",
        ("nothing_to_undo", Es419) => "nada para deshacer",
        ("dry_run", PtBr) => "simulação: nada foi gravado",
        ("dry_run", EnUs) => "dry run: nothing was written",
        ("dry_run", Es419) => "simulación: no se guardó nada",
        ("call_untitled", PtBr) => "Chamada de",
        ("call_untitled", EnUs) => "Call on",
        ("call_untitled", Es419) => "Llamada del",
        ("unclassified", PtBr) => "Não classificadas",
        ("unclassified", EnUs) => "Unclassified",
        ("unclassified", Es419) => "Sin clasificar",
        ("edited", PtBr) => "editado",
        ("edited", EnUs) => "edited",
        ("edited", Es419) => "editado",
        ("me", PtBr) => "Eu",
        ("me", EnUs) => "Me",
        ("me", Es419) => "Yo",
        ("glossary_nothing_to_apply", PtBr) => "nenhuma regra do glossário mudou algum bloco",
        ("glossary_nothing_to_apply", EnUs) => "no glossary rule changed any block",
        ("glossary_nothing_to_apply", Es419) => "ninguna regla del glosario cambió algún bloque",
        ("glossary_no_suggestions", PtBr) => "nenhuma sugestão de regra para essa edição",
        ("glossary_no_suggestions", EnUs) => "no rule suggestions for this edit",
        ("glossary_no_suggestions", Es419) => "ninguna sugerencia de regla para esta edición",
        ("rec_recording", PtBr) => "gravando",
        ("rec_recording", EnUs) => "recording",
        ("rec_recording", Es419) => "grabando",
        ("rec_stopped", PtBr) => "parado",
        ("rec_stopped", EnUs) => "stopped",
        ("rec_stopped", Es419) => "detenido",
        ("bar_shown", PtBr) => "barra mostrada",
        ("bar_shown", EnUs) => "bar shown",
        ("bar_shown", Es419) => "barra mostrada",
        ("bar_hidden", PtBr) => "barra escondida",
        ("bar_hidden", EnUs) => "bar hidden",
        ("bar_hidden", Es419) => "barra oculta",
        ("tray_record", PtBr) => "Gravar",
        ("tray_record", EnUs) => "Record",
        ("tray_record", Es419) => "Grabar",
        ("tray_stop", PtBr) => "Parar gravação",
        ("tray_stop", EnUs) => "Stop recording",
        ("tray_stop", Es419) => "Detener grabación",
        ("tray_bar_show", PtBr) => "Mostrar barra",
        ("tray_bar_show", EnUs) => "Show bar",
        ("tray_bar_show", Es419) => "Mostrar barra",
        ("tray_bar_hide", PtBr) => "Esconder barra",
        ("tray_bar_hide", EnUs) => "Hide bar",
        ("tray_bar_hide", Es419) => "Ocultar barra",
        ("tray_open", PtBr) => "Abrir janela",
        ("tray_open", EnUs) => "Open window",
        ("tray_open", Es419) => "Abrir ventana",
        ("tray_quit", PtBr) => "Sair",
        ("tray_quit", EnUs) => "Quit",
        ("tray_quit", Es419) => "Salir",
        ("quit_title", PtBr) => "Sair do app?",
        ("quit_title", EnUs) => "Quit the app?",
        ("quit_title", Es419) => "¿Salir de la app?",
        ("quit_recording", PtBr) => "Há uma gravação em andamento. Ao sair, ela é encerrada e fica salva; você poderá recuperá-la na próxima abertura.",
        ("quit_recording", EnUs) => "A recording is in progress. Quitting stops it and keeps it saved; you can recover it the next time the app opens.",
        ("quit_recording", Es419) => "Hay una grabación en curso. Al salir se detiene y queda guardada; podrás recuperarla la próxima vez que abras la app.",
        ("quit_finalizing", PtBr) => "Uma gravação ainda está sendo convertida. Ao sair, a conversão é interrompida; você poderá retomá-la na próxima abertura.",
        ("quit_finalizing", EnUs) => "A recording is still being converted. Quitting interrupts the conversion; you can resume it the next time the app opens.",
        ("quit_finalizing", Es419) => "Una grabación aún se está convirtiendo. Al salir se interrumpe la conversión; podrás retomarla la próxima vez que abras la app.",
        ("quit_confirm", PtBr) => "Sair",
        ("quit_confirm", EnUs) => "Quit",
        ("quit_confirm", Es419) => "Salir",
        ("quit_cancel", PtBr) => "Cancelar",
        ("quit_cancel", EnUs) => "Cancel",
        ("quit_cancel", Es419) => "Cancelar",
        ("close_title", PtBr) => "Gravação em andamento",
        ("close_title", EnUs) => "Recording in progress",
        ("close_title", Es419) => "Grabación en curso",
        ("close_recording", PtBr) => "Há uma gravação em andamento. Você pode esconder a janela e continuar gravando pela barra, ou parar a gravação (ela é convertida e salva) e sair do app.",
        ("close_recording", EnUs) => "A recording is in progress. You can hide the window and keep recording from the bar, or stop the recording (it is converted and saved) and quit the app.",
        ("close_recording", Es419) => "Hay una grabación en curso. Puedes ocultar la ventana y seguir grabando desde la barra, o detener la grabación (se convierte y queda guardada) y salir de la app.",
        ("close_hide", PtBr) => "Esconder e continuar gravando",
        ("close_hide", EnUs) => "Hide and keep recording",
        ("close_hide", Es419) => "Ocultar y seguir grabando",
        ("close_stop", PtBr) => "Parar e sair",
        ("close_stop", EnUs) => "Stop and quit",
        ("close_stop", Es419) => "Detener y salir",
        ("rec_error_title", PtBr) => "Gravação",
        ("rec_error_title", EnUs) => "Recording",
        ("rec_error_title", Es419) => "Grabación",
        _ => "",
    }
}
