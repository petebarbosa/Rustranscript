//! Ajuda da CLI no idioma do sistema. Os textos em português são os comentários de documentação
//! do `clap` (fonte da verdade, em `cli.rs`); aqui ficam só as traduções para en-US e es-419,
//! chaveadas pelo caminho do comando (`rstt glossary add`) e, para argumentos, `caminho:id`.
//! O idioma vem de `Lang` (o mesmo de `--lang`, da configuração e do sistema): nada de resolução própria.
use clap::{Arg, ArgAction, Command};

use crate::i18n::Lang;

/// (chave, en-US, es-419). Todo comando e argumento com texto no `clap` precisa estar aqui
/// (`every_help_text_is_translated` confere).
const TABLE: &[(&str, &str, &str)] = &[
    ("rstt", "Local meeting recording and transcription", "Grabación y transcripción de reuniones, local"),
    ("rstt:data_dir", "Data directory (default: ~/.local/share/rustranscript or $RSTT_DATA_DIR)", "Directorio de datos (predeterminado: ~/.local/share/rustranscript o $RSTT_DATA_DIR)"),
    ("rstt:lang", "Message language: pt-BR, en-US, es-419", "Idioma de los mensajes: pt-BR, en-US, es-419"),
    ("rstt:json", "`record`, `status` and `bar`: return the full state as JSON instead of a text line", "`record`, `status` y `bar`: devuelve el estado completo en JSON en vez de una línea de texto"),
    ("rstt gui", "Opens the window (same as running with no arguments)", "Abre la ventana (lo mismo que ejecutar sin argumentos)"),
    ("rstt list", "Lists calls", "Lista llamadas"),
    ("rstt list:library", "Company/project (id or name); without it, all", "Empresa/proyecto (id o nombre); sin esto, todas"),
    ("rstt list:client", "Client (id, name or slug), within --library", "Cliente (id, nombre o slug), dentro de --library"),
    ("rstt list:unassigned", "Only unclassified calls (no client or in the inbox)", "Solo las sin clasificar (sin cliente o en la bandeja)"),
    ("rstt show", "Shows a call (blocks, speakers, versions)", "Muestra una llamada (bloques, hablantes, versiones)"),
    ("rstt show:call", "Key (call_YYYY-MM-DD_HH-MM-SS), old file name or <library>:<id>", "Clave (call_AAAA-MM-DD_HH-MM-SS), nombre del archivo antiguo o <biblioteca>:<id>"),
    ("rstt show:version", "Transcript version (default: the active one)", "Versión de la transcripción (predeterminada: la activa)"),
    ("rstt show:text", "Plain text instead of JSON", "Texto corrido en vez de JSON"),
    ("rstt search", "Searches all calls (ignores accents; word prefix)", "Busca en todas las llamadas (ignora acentos; prefijo de palabra)"),
    ("rstt edit", "Edits texts, titles and speakers", "Edita textos, títulos y hablantes"),
    ("rstt edit block", "Replaces the text of a block (block number as in `show`)", "Cambia el texto de un bloque (número del bloque como en `show`)"),
    ("rstt edit revert", "Returns the block to the transcript's original text", "Devuelve el bloque al texto original de la transcripción"),
    ("rstt edit title", "Call title", "Título de la llamada"),
    ("rstt edit speaker", "Renames a speaker across the whole call (\"Person 2\" → \"Maria\")", "Renombra a un hablante en toda la llamada (\"Persona 2\" → \"María\")"),
    ("rstt edit speaker:speaker", "Label, current name or id", "Etiqueta, nombre actual o id"),
    ("rstt edit speaker:name", "New name (omit with --clear to go back to the label)", "Nuevo nombre (omítelo con --clear para volver a la etiqueta)"),
    ("rstt edit block-speaker", "Assigns a block to another speaker", "Asigna un bloque a otro hablante"),
    ("rstt history", "Change history", "Historial de cambios"),
    ("rstt undo", "Undoes the call's most recent change", "Deshace el cambio más reciente de la llamada"),
    ("rstt import", "Imports transcripts and audio from the old pipeline (originals are not deleted)", "Importa transcripciones y audio del pipeline antiguo (los originales no se borran)"),
    ("rstt import:library", "Destination company/project for new calls (default: Unclassified)", "Empresa/proyecto de destino para llamadas nuevas (predeterminado: Sin clasificar)"),
    ("rstt import:no_audio", "Does not convert WAV → FLAC now", "No convierte WAV → FLAC ahora"),
    ("rstt assign", "Classifies a call: company/project and client", "Clasifica una llamada: empresa/proyecto y cliente"),
    ("rstt assign:library", "Destination company/project (id or name)", "Empresa/proyecto de destino (id o nombre)"),
    ("rstt assign:client", "Client (id, name or slug); omitted = no client", "Cliente (id, nombre o slug); omitido = sin cliente"),
    ("rstt assign:inbox", "Goes back to Unclassified", "Vuelve a Sin clasificar"),
    ("rstt library", "Registered companies/projects", "Empresas/proyectos registrados"),
    ("rstt library add", "Registers (or adopts an existing folder that has a library.db)", "Registra (o adopta una carpeta que ya tiene library.db)"),
    ("rstt library remove", "Only unregisters; the folder stays on disk", "Solo quita el registro; la carpeta queda en el disco"),
    ("rstt client", "Clients of a company/project", "Clientes de una empresa/proyecto"),
    ("rstt glossary", "Glossary: terms (model prompt) and \"wrong → right\" replacements, global or per client", "Glosario: términos (prompt del modelo) y reemplazos \"incorrecto → correcto\", globales o por cliente"),
    (
        "rstt glossary list",
        "Lists the rules: without --library, the global ones; with --library and --client, the ones in effect for the client (client overrides global; hidden ones come with overridden=true); with --library only, the global ones plus those of all its clients",
        "Lista las reglas: sin --library, las globales; con --library y --client, las vigentes para el cliente (el cliente sobrescribe lo global; las ocultas vienen con overridden=true); solo con --library, las globales más las de todos sus clientes",
    ),
    ("rstt glossary list:kind", "Only one kind", "Solo un tipo"),
    ("rstt glossary add", "Creates a rule: with <replacement> it is \"wrong → right\"; without, it is a term", "Crea una regla: con <replacement> es \"incorrecto → correcto\"; sin esto, es un término"),
    ("rstt glossary add:pattern", "Text to look for (or the term)", "Texto a buscar (o el término)"),
    ("rstt glossary add:replacement", "Text that goes in its place", "Texto que entra en su lugar"),
    ("rstt glossary add:library", "Client's company/project (id or name)", "Empresa/proyecto del cliente (id o nombre)"),
    ("rstt glossary add:global", "Global rule (applies to all calls)", "Regla global (vale para todas las llamadas)"),
    ("rstt glossary add:case_sensitive", "Case-sensitive", "Distingue mayúsculas de minúsculas"),
    ("rstt glossary add:term", "Creates a term (the default when there is no replacement)", "Crea un término (lo predeterminado cuando no hay reemplazo)"),
    ("rstt glossary remove", "Removes a rule by id (global and client ids are independent)", "Elimina una regla por id (los ids globales y de cliente son independientes)"),
    ("rstt glossary promote", "Promotes a client rule to global (the client's copy is removed)", "Promueve una regla de cliente a global (la copia del cliente se elimina)"),
    ("rstt glossary apply", "Applies the glossary in effect to a call; everything becomes a single batch, undone by `undo`", "Aplica el glosario vigente a una llamada; todo es un solo lote, que `undo` deshace"),
    ("rstt glossary apply:version", "Transcript version (default: the active one)", "Versión de la transcripción (predeterminada: la activa)"),
    (
        "rstt glossary import",
        "Imports a UTF-8 text file: one entry per line, `#` comments, `wrong -> right` (or → or =>) becomes a replacement and the rest becomes a term",
        "Importa un archivo de texto UTF-8: una entrada por línea, `#` comenta, `incorrecto -> correcto` (o → o =>) es un reemplazo y el resto es un término",
    ),
    ("rstt glossary import:kind", "Only accepts this kind of line", "Solo acepta este tipo de línea"),
    ("rstt glossary suggest", "Shows the rules that would be suggested by an edit <before> → <after> (debugging)", "Muestra las reglas que se sugerirían por una edición <antes> → <después> (depuración)"),
    ("rstt glossary terms", "Terms that would go into the model prompt, within the token budget", "Términos que irían al prompt del modelo, dentro del presupuesto de tokens"),
    ("rstt reclaimable", "Already imported originals that still take up space", "Originales ya importados que aún ocupan espacio"),
    ("rstt settings", "Settings (language, \"Me\" name, data directory)", "Configuración (idioma, nombre del \"Yo\", directorio de datos)"),
    ("rstt settings set", "Keys: language (pt-BR|en-US|es-419), me_name, transcription_language", "Claves: language (pt-BR|en-US|es-419), me_name, transcription_language"),
    ("rstt settings data-dir", "Changes the data directory (takes effect the next time the app opens; data is not moved)", "Cambia el directorio de datos (vale en la próxima apertura; los datos no se mueven)"),
    ("rstt record", "Recording: the app window does the recording; `start`/`toggle` open the app by themselves if it is not open", "Grabación: la ventana de la app es quien graba; `start`/`toggle` abren la app solos si no está abierta"),
    ("rstt record start", "Starts recording (`already_recording` error if already recording)", "Empieza a grabar (error `already_recording` si ya está grabando)"),
    ("rstt record start:library", "Destination company/project (id or name); without it, Unclassified", "Empresa/proyecto de destino (id o nombre); sin esto, Sin clasificar"),
    ("rstt record start:client", "Client (id, name or slug), within --library", "Cliente (id, nombre o slug), dentro de --library"),
    ("rstt record start:expected_speakers", "How many people on the other side (improves voice separation)", "Cuántas personas del otro lado (mejora la separación de voces)"),
    ("rstt record start:language", "Transcription language (pt, en, es...)", "Idioma de la transcripción (pt, en, es...)"),
    ("rstt record start:mic", "Microphone: device name (see `record devices`), `default` or `off`", "Micrófono: nombre del dispositivo (ver `record devices`), `default` u `off`"),
    ("rstt record start:sys", "System audio: monitor name (see `record devices`), `default` or `off`", "Audio del sistema: nombre del monitor (ver `record devices`), `default` u `off`"),
    ("rstt record stop", "Stops recording; conversion and call creation continue in the background", "Detiene la grabación; la conversión y la creación de la llamada siguen en segundo plano"),
    ("rstt record toggle", "Starts if idle, stops if recording (use it on the compositor shortcut)", "Empieza si está inactivo, detiene si está grabando (úsalo en el atajo del compositor)"),
    ("rstt record devices", "Lists microphones and monitors (does not need the app)", "Lista micrófonos y monitores (no necesita la app)"),
    ("rstt status", "Recording state (`{\"state\":\"idle\",\"app_running\":false}` if the app is not open)", "Estado de la grabación (`{\"state\":\"idle\",\"app_running\":false}` si la app no está abierta)"),
    ("rstt bar", "Mini recording bar (needs the app open)", "Mini barra de grabación (necesita la app abierta)"),
    ("rstt transcribe", "Transcribes a call: creates the job in the queue and makes sure the app is up (it does the work)", "Transcribe una llamada: crea la tarea en la cola y asegura que la app esté abierta (ella hace el trabajo)"),
    ("rstt transcribe:call", "Key (call_YYYY-MM-DD_HH-MM-SS), old file name or <library>:<id>", "Clave (call_AAAA-MM-DD_HH-MM-SS), nombre del archivo antiguo o <biblioteca>:<id>"),
    ("rstt transcribe:pending", "Queues all pending calls (without an open job)", "Encola todas las llamadas pendientes (sin tarea abierta)"),
    ("rstt transcribe:kind", "full: everything again; rediarize: only separates the voices again; resegment: only rebuilds the text (without running the model)", "full: todo de nuevo; rediarize: solo separa las voces de nuevo; resegment: solo rearma el texto (sin ejecutar el modelo)"),
    ("rstt transcribe:language", "Spoken language: auto, pt, en or es (default: the call's or the setting's)", "Idioma hablado: auto, pt, en o es (predeterminado: el de la llamada o el de la configuración)"),
    ("rstt transcribe:expected_speakers", "How many people on the other side (improves voice separation)", "Cuántas personas del otro lado (mejora la separación de voces)"),
    ("rstt transcribe:no_bleed_filter", "Does not remove the other side's echo picked up by the microphone", "No elimina el eco del otro lado captado por el micrófono"),
    ("rstt transcribe:dry_run", "Only shows what would be queued", "Solo muestra lo que se encolaría"),
    ("rstt queue", "Transcription queue (default: `list`)", "Cola de transcripción (predeterminado: `list`)"),
    ("rstt queue list", "Running and queued jobs and the most recent finished ones", "Tareas en curso, en cola y las últimas terminadas"),
    ("rstt queue pause", "Pauses the queue (the running job stops and goes back to the queue)", "Pausa la cola (la tarea en curso se detiene y vuelve a la cola)"),
    ("rstt queue resume", "Resumes the queue", "Reanuda la cola"),
    ("rstt queue cancel", "Cancels a queued job (a running one only from the app window)", "Cancela una tarea en cola (la que está en curso solo desde la ventana de la app)"),
    ("rstt queue retry", "Tries again a job that failed or was cancelled", "Reintenta una tarea que falló o fue cancelada"),
    ("rstt setup", "Transcription runtime (isolated Python + models), installed inside the data directory", "Entorno de transcripción (Python aislado + modelos), instalado dentro del directorio de datos"),
    ("rstt setup status", "State of the runtime and models", "Estado del entorno y de los modelos"),
    ("rstt setup install", "Installs the runtime and downloads the missing models (progress on stderr; does not need the app)", "Instala el entorno y descarga los modelos que faltan (progreso en stderr; no necesita la app)"),
];

/// Textos fixos do `clap` (cabeçalhos e a ajuda embutida), nos três idiomas:
/// (uso, comandos, opções, argumentos, `-h`, `-V`, subcomando `help`, argumento do `help`).
fn builtin(lang: Lang) -> [&'static str; 8] {
    match lang {
        Lang::PtBr => ["Uso", "Comandos", "Opções", "Argumentos", "Mostra a ajuda", "Mostra a versão", "Mostra esta mensagem ou a ajuda de um subcomando", "Subcomando"],
        Lang::EnUs => ["Usage", "Commands", "Options", "Arguments", "Print help", "Print version", "Print this message or the help of the given subcommand(s)", "Subcommand"],
        Lang::Es419 => ["Uso", "Comandos", "Opciones", "Argumentos", "Muestra la ayuda", "Muestra la versión", "Muestra este mensaje o la ayuda de un subcomando", "Subcomando"],
    }
}

/// `record toggle` usa os mesmos argumentos de `record start` (`RecordArgs`): um texto só para os dois.
fn canonical(key: &str) -> String {
    key.replace("rstt record toggle:", "rstt record start:")
}

fn translated(lang: Lang, key: &str) -> Option<&'static str> {
    let key = canonical(key);
    let &(_, en, es) = TABLE.iter().find(|(k, ..)| *k == key)?;
    match lang {
        Lang::PtBr => None,
        Lang::EnUs => Some(en),
        Lang::Es419 => Some(es),
    }
}

/// Aplica o idioma à ajuda: a árvore de comandos é construída primeiro (isso cria `-h`, `-V`, o
/// subcomando `help` e propaga as opções globais) e só então cada texto é trocado. Em pt-BR ficam
/// os textos do `clap`, só com os cabeçalhos e a ajuda embutida em português.
pub fn localize(mut cmd: Command, lang: Lang) -> Command {
    cmd.build();
    let name = cmd.get_name().to_string();
    walk(cmd, lang, &name)
}

fn walk(cmd: Command, lang: Lang, path: &str) -> Command {
    let b = builtin(lang);
    let usage = *cmd.get_styles().get_usage();
    let mut cmd = cmd
        .help_template(format!("{{before-help}}{{about-with-newline}}\n{usage}{}:{usage:#} {{usage}}\n\n{{all-args}}{{after-help}}", b[0]))
        .subcommand_help_heading(b[1]);
    if let Some(text) = translated(lang, path) {
        cmd = cmd.about(text).long_about(None::<&str>);
    }
    if cmd.get_name() == "help" {
        cmd = cmd.about(b[6]);
    }
    // `-h` e `-V` do clap se reconhecem pela ação (um `--version` do próprio comando tem outra)
    let args: Vec<(String, bool, bool)> =
        cmd.get_arguments().map(|a| (a.get_id().to_string(), a.is_positional(), matches!(a.get_action(), ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong))).collect();
    let is_version = |cmd: &Command, id: &str| cmd.get_arguments().any(|a| a.get_id() == id && matches!(a.get_action(), ArgAction::Version));
    for (id, positional, is_help) in args {
        let key = format!("{path}:{id}");
        // opções globais: o texto vive na raiz
        let root_key = format!("{}:{id}", path.split(' ').next().unwrap_or(path));
        let text = match id.as_str() {
            _ if is_help => Some(b[4]),
            _ if is_version(&cmd, &id) => Some(b[5]),
            "subcommand" if cmd.get_name() == "help" => Some(b[7]),
            _ => translated(lang, &key).or_else(|| translated(lang, &root_key)),
        };
        cmd = cmd.mut_arg(id, |a: Arg| {
            let a = a.help_heading(if positional { b[3] } else { b[2] });
            match text {
                Some(t) => a.help(t).long_help(None::<&str>),
                None => a,
            }
        });
    }
    let subs: Vec<String> = cmd.get_subcommands().map(|s| s.get_name().to_string()).collect();
    for sub in subs {
        let child = format!("{path} {sub}");
        cmd = cmd.mut_subcommand(sub, |s| walk(s, lang, &child));
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn collect(cmd: &Command, path: &str, out: &mut Vec<String>) {
        if cmd.get_about().is_some() && path != "rstt help" {
            out.push(path.to_string());
        }
        // opções globais reaparecem em cada subcomando com o texto da raiz
        for a in cmd.get_arguments().filter(|a| a.get_help().is_some() && !matches!(a.get_action(), ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version) && (path == "rstt" || !a.is_global_set())) {
            out.push(format!("{path}:{}", a.get_id()));
        }
        for s in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            collect(s, &format!("{path} {}", s.get_name()), out);
        }
    }

    /// Todo comando/argumento com texto no `clap` tem tradução em en-US e es-419, e a tabela não
    /// guarda chave que não existe mais.
    #[test]
    fn every_help_text_is_translated() {
        let mut cmd = crate::cli::Cli::command();
        cmd.build();
        let mut keys = vec![];
        collect(&cmd, "rstt", &mut keys);
        for k in &keys {
            assert!(TABLE.iter().any(|(t, ..)| *t == canonical(k)), "sem tradução para {k}");
        }
        for (k, en, es) in TABLE {
            assert!(keys.iter().any(|x| canonical(x) == *k), "chave morta na tabela: {k}");
            assert!(!en.is_empty() && !es.is_empty());
        }
    }
}
