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
        ("runtime_missing", PtBr) => "o ambiente de transcrição não está instalado (rode `setup install`)",
        ("runtime_missing", EnUs) => "the transcription runtime is not installed (run `setup install`)",
        ("runtime_missing", Es419) => "el entorno de transcripción no está instalado (ejecuta `setup install`)",
        ("runtime_outdated", PtBr) => "o ambiente de transcrição está desatualizado (rode `setup install`)",
        ("runtime_outdated", EnUs) => "the transcription runtime is outdated (run `setup install`)",
        ("runtime_outdated", Es419) => "el entorno de transcripción está desactualizado (ejecuta `setup install`)",
        ("models_missing", PtBr) => "faltam modelos de transcrição (rode `setup install`)",
        ("models_missing", EnUs) => "transcription models are missing (run `setup install`)",
        ("models_missing", Es419) => "faltan modelos de transcripción (ejecuta `setup install`)",
        ("setup_failed", PtBr) => "falha ao instalar o ambiente de transcrição",
        ("setup_failed", EnUs) => "failed to install the transcription runtime",
        ("setup_failed", Es419) => "falló la instalación del entorno de transcripción",
        ("setup_cancelled", PtBr) => "instalação cancelada",
        ("setup_cancelled", EnUs) => "setup cancelled",
        ("setup_cancelled", Es419) => "instalación cancelada",
        ("download_failed", PtBr) => "falha no download",
        ("download_failed", EnUs) => "download failed",
        ("download_failed", Es419) => "falló la descarga",
        ("checksum_mismatch", PtBr) => "o arquivo baixado não confere com a soma de verificação",
        ("checksum_mismatch", EnUs) => "the downloaded file does not match its checksum",
        ("checksum_mismatch", Es419) => "el archivo descargado no coincide con su suma de verificación",
        ("worker_crashed", PtBr) => "o processo de transcrição caiu",
        ("worker_crashed", EnUs) => "the transcription process crashed",
        ("worker_crashed", Es419) => "el proceso de transcripción falló",
        ("worker_protocol", PtBr) => "resposta inesperada do processo de transcrição",
        ("worker_protocol", EnUs) => "unexpected reply from the transcription process",
        ("worker_protocol", Es419) => "respuesta inesperada del proceso de transcripción",
        ("audio_decode", PtBr) => "não foi possível decodificar o áudio",
        ("audio_decode", EnUs) => "could not decode the audio",
        ("audio_decode", Es419) => "no se pudo decodificar el audio",
        ("no_audio", PtBr) => "a chamada não tem áudio",
        ("no_audio", EnUs) => "the call has no audio",
        ("no_audio", Es419) => "la llamada no tiene audio",
        ("oom", PtBr) => "memória insuficiente para transcrever",
        ("oom", EnUs) => "not enough memory to transcribe",
        ("oom", Es419) => "memoria insuficiente para transcribir",
        ("no_raw_data", PtBr) => "a versão não tem dados brutos (foi importada)",
        ("no_raw_data", EnUs) => "the version has no raw data (it was imported)",
        ("no_raw_data", Es419) => "la versión no tiene datos en bruto (fue importada)",
        ("job_failed", PtBr) => "a transcrição falhou",
        ("job_failed", EnUs) => "transcription failed",
        ("job_failed", Es419) => "la transcripción falló",
        (_, PtBr) => "erro",
        (_, EnUs) => "error",
        (_, Es419) => "error",
    }
}

/// Aviso de migração pendente com a app antiga aberta (CLI no stderr, GUI num diálogo).
pub fn legacy_running_message(lang: Lang, blocked: &core_lib::paths::LegacyRunning) -> String {
    msg(lang, "legacy_running").replace("{old}", &blocked.old_data.display().to_string())
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
        ("legacy_running", PtBr) => "Há dados da versão anterior do app em {old} e ela ainda está aberta. Feche-a e abra o Rustranscript de novo para migrar os dados.",
        ("legacy_running", EnUs) => "Data from the previous version of the app is in {old} and it is still running. Close it and open Rustranscript again to migrate the data.",
        ("legacy_running", Es419) => "Hay datos de la versión anterior de la app en {old} y sigue abierta. Ciérrala y abre Rustranscript de nuevo para migrar los datos.",
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
        ("setup_runtime", PtBr) => "instalando o ambiente de transcrição",
        ("setup_runtime", EnUs) => "installing the transcription runtime",
        ("setup_runtime", Es419) => "instalando el entorno de transcripción",
        ("setup_model", PtBr) => "baixando o modelo",
        ("setup_model", EnUs) => "downloading model",
        ("setup_model", Es419) => "descargando el modelo",
        ("setup_done", PtBr) => "instalação concluída",
        ("setup_done", EnUs) => "setup finished",
        ("setup_done", Es419) => "instalación terminada",
        ("queue_cancel_running", PtBr) => "a tarefa está em andamento; cancele pela janela do app",
        ("queue_cancel_running", EnUs) => "the job is running; cancel it from the app window",
        ("queue_cancel_running", Es419) => "la tarea está en curso; cancélala desde la ventana de la app",
        ("gui_not_started", PtBr) => "a app não abriu; a tarefa fica na fila até a app ser aberta",
        ("gui_not_started", EnUs) => "the app did not start; the job stays queued until the app is opened",
        ("gui_not_started", Es419) => "la app no se abrió; la tarea queda en cola hasta que se abra la app",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGS: [Lang; 3] = [Lang::PtBr, Lang::EnUs, Lang::Es419];

    /// Todo código de erro da fase 4 tem texto próprio nas 3 línguas (e não cai no genérico).
    #[test]
    fn transcription_error_codes_are_translated() {
        let codes = [
            "runtime_missing", "runtime_outdated", "models_missing", "setup_failed", "setup_cancelled", "download_failed",
            "checksum_mismatch", "worker_crashed", "worker_protocol", "audio_decode", "no_audio", "oom", "no_raw_data", "job_failed",
        ];
        for lang in LANGS {
            for code in codes {
                let text = error_prefix(lang, code);
                assert!(!matches!(text, "erro" | "error"), "{code} sem tradução em {}", lang.tag());
            }
        }
    }

    #[test]
    fn transcription_messages_exist_in_all_languages() {
        for key in ["setup_runtime", "setup_model", "setup_done", "queue_cancel_running", "gui_not_started"] {
            for lang in LANGS {
                assert!(!lookup(lang, key).is_empty(), "{key} sem tradução em {}", lang.tag());
            }
        }
    }
}
