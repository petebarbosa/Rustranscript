//! CLI no mesmo binário. Saída em JSON (chaves estáveis em inglês) no stdout; erros em JSON no
//! stderr com código de saída 1. Edições valem igual às da tela (entram no histórico, com
//! `origin = cli`) e avisam a app aberta pelo socket local para ela recarregar.
use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use core_lib::import::{self, ImportOptions};
use core_lib::recording::{self as core_rec, Meta, StartRequest};
use core_lib::model::{ClientInfo, Rule, RuleKind};
use core_lib::rules::{ImportScope, RuleInput};
use core_lib::transcription::params::JobOptions;
use core_lib::transcription::queue::{self, JobKind, JobState, PauseReason};
use core_lib::transcription::{keys, models, runtime};
use core_lib::{App, ClientFilter, Error, Library, Origin, glossary, paths, search, transfer};
use recorder::StreamChoice;
use serde_json::{Value, json};

use crate::i18n::{self, Lang};
use crate::ipc::{self, Request};
use crate::transcription;

#[derive(Parser)]
#[command(name = "rstt", version, about = "Gravação e transcrição de reuniões, local")]
pub struct Cli {
    /// Diretório de dados (padrão: ~/.local/share/rustranscript ou $RSTT_DATA_DIR)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Idioma das mensagens: pt-BR, en-US, es-419
    #[arg(long, global = true)]
    lang: Option<String>,
    /// `record`, `status` e `bar`: devolve o estado completo em JSON em vez de uma linha de texto
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Abre a janela (o mesmo que rodar sem argumentos)
    Gui,
    /// Lista chamadas
    List {
        /// Empresa/projeto (id ou nome); sem isso, todas
        #[arg(long)]
        library: Option<String>,
        /// Cliente (id, nome ou slug), dentro de --library
        #[arg(long, requires = "library")]
        client: Option<String>,
        /// Só as não classificadas (sem cliente ou na inbox)
        #[arg(long, conflicts_with = "client")]
        unassigned: bool,
    },
    /// Mostra uma chamada (blocos, falantes, versões)
    Show {
        /// Chave (call_AAAA-MM-DD_HH-MM-SS), nome do arquivo antigo ou <biblioteca>:<id>
        call: String,
        /// Versão da transcrição (padrão: a ativa)
        #[arg(long)]
        version: Option<i64>,
        /// Texto corrido em vez de JSON
        #[arg(long)]
        text: bool,
    },
    /// Busca em todas as chamadas (ignora acentos; prefixo de palavra)
    Search {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Edita textos, títulos e falantes
    Edit {
        #[command(subcommand)]
        what: EditCmd,
    },
    /// Histórico de alterações
    History {
        call: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// Desfaz a alteração mais recente da chamada
    Undo {
        call: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Importa transcrições e áudio do pipeline antigo (originais não são apagados)
    Import {
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Empresa/projeto de destino para chamadas novas (padrão: Não classificadas)
        #[arg(long)]
        library: Option<String>,
        #[arg(long, requires = "library")]
        client: Option<String>,
        /// Não converte WAV → FLAC agora
        #[arg(long)]
        no_audio: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Classifica uma chamada: empresa/projeto e cliente
    Assign {
        call: String,
        /// Empresa/projeto de destino (id ou nome)
        #[arg(long, required_unless_present = "inbox")]
        library: Option<String>,
        /// Cliente (id, nome ou slug); omitido = sem cliente
        #[arg(long, requires = "library")]
        client: Option<String>,
        /// Volta para Não classificadas
        #[arg(long, conflicts_with_all = ["library", "client"])]
        inbox: bool,
    },
    /// Empresas/projetos cadastrados
    Library {
        #[command(subcommand)]
        what: Option<LibraryCmd>,
    },
    /// Clientes de uma empresa/projeto
    Client {
        #[command(subcommand)]
        what: ClientCmd,
    },
    /// Glossário: termos (prompt do modelo) e substituições "errado → certo", globais ou por cliente
    Glossary {
        #[command(subcommand)]
        what: GlossaryCmd,
    },
    /// Originais já importados que ainda ocupam espaço
    Reclaimable,
    /// Configurações (idioma, nome do "Eu", diretório de dados)
    Settings {
        #[command(subcommand)]
        what: Option<SettingsCmd>,
    },
    /// Gravação: a janela da app é quem grava; `start`/`toggle` abrem a app sozinhos se ela não estiver aberta
    Record {
        #[command(subcommand)]
        what: RecordCmd,
    },
    /// Estado da gravação (`{"state":"idle","app_running":false}` se a app não estiver aberta)
    Status,
    /// Mini barra de gravação (precisa da app aberta)
    Bar {
        #[command(subcommand)]
        what: BarCmd,
    },
    /// Transcreve uma chamada: cria a tarefa na fila e garante a app de pé (ela faz o trabalho)
    Transcribe(TranscribeArgs),
    /// Fila de transcrição (padrão: `list`)
    Queue {
        #[command(subcommand)]
        what: Option<QueueCmd>,
    },
    /// Ambiente de transcrição (Python isolado + modelos), instalado dentro do diretório de dados
    Setup {
        #[command(subcommand)]
        what: SetupCmd,
    },
}

#[derive(clap::Args, Debug, Clone, Default)]
pub struct TranscribeArgs {
    /// Chave (call_AAAA-MM-DD_HH-MM-SS), nome do arquivo antigo ou <biblioteca>:<id>
    #[arg(required_unless_present = "pending")]
    call: Option<String>,
    /// Enfileira todas as chamadas pendentes (sem tarefa aberta)
    #[arg(long, conflicts_with_all = ["call", "kind", "language", "expected_speakers", "no_bleed_filter"])]
    pending: bool,
    /// full: tudo de novo; rediarize: só separa as vozes de novo; resegment: só remonta o texto (sem rodar o modelo)
    #[arg(long, value_parser = ["full", "rediarize", "resegment"])]
    kind: Option<String>,
    /// Idioma falado: auto, pt, en ou es (padrão: o da chamada ou da configuração)
    #[arg(long)]
    language: Option<String>,
    /// Quantas pessoas do outro lado (melhora a separação de vozes)
    #[arg(long = "speakers")]
    expected_speakers: Option<i64>,
    /// Não remove o eco do outro lado captado pelo microfone
    #[arg(long)]
    no_bleed_filter: bool,
    /// Só mostra o que seria enfileirado
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum QueueCmd {
    /// Tarefas rodando, enfileiradas e as últimas terminadas
    List,
    /// Pausa a fila (a tarefa em curso para e volta à fila)
    Pause,
    /// Retoma a fila
    Resume,
    /// Cancela uma tarefa enfileirada (a em andamento só pela janela do app)
    Cancel { id: i64 },
    /// Tenta de novo uma tarefa que falhou ou foi cancelada
    Retry { id: i64 },
}

#[derive(Subcommand)]
enum SetupCmd {
    /// Estado do ambiente e dos modelos
    Status,
    /// Instala o ambiente e baixa os modelos que faltam (progresso no stderr; não precisa da app)
    Install,
}

#[derive(clap::Args, Debug, Clone, Default)]
pub struct RecordArgs {
    /// Empresa/projeto de destino (id ou nome); sem isso, Não classificadas
    #[arg(long)]
    library: Option<String>,
    /// Cliente (id, nome ou slug), dentro de --library
    #[arg(long, requires = "library")]
    client: Option<String>,
    #[arg(long)]
    title: Option<String>,
    /// Quantas pessoas do outro lado (melhora a separação de vozes)
    #[arg(long = "speakers")]
    expected_speakers: Option<i64>,
    /// Idioma da transcrição (pt, en, es...)
    #[arg(long)]
    language: Option<String>,
    /// Microfone: nome do dispositivo (ver `record devices`), `default` ou `off`
    #[arg(long)]
    mic: Option<String>,
    /// Áudio do sistema: nome do monitor (ver `record devices`), `default` ou `off`
    #[arg(long)]
    sys: Option<String>,
}

#[derive(Subcommand)]
enum RecordCmd {
    /// Começa a gravar (erro `already_recording` se já estiver gravando)
    Start(RecordArgs),
    /// Para a gravação; a conversão e a criação da chamada continuam em segundo plano
    Stop,
    /// Começa se estiver ocioso, para se estiver gravando (use no atalho do compositor)
    Toggle(RecordArgs),
    /// Lista microfones e monitores (não precisa da app)
    Devices,
}

#[derive(Subcommand)]
enum BarCmd {
    Show,
    Hide,
}

#[derive(Subcommand)]
enum EditCmd {
    /// Troca o texto de um bloco (número do bloco como em `show`)
    Block {
        call: String,
        seq: i64,
        text: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Volta o bloco ao texto original da transcrição
    Revert {
        call: String,
        seq: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Título da chamada
    Title {
        call: String,
        title: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Renomeia um falante em toda a chamada ("Pessoa 2" → "Maria")
    Speaker {
        call: String,
        /// Rótulo, nome atual ou id
        speaker: String,
        /// Novo nome (omita com --clear para voltar ao rótulo)
        name: Option<String>,
        #[arg(long, conflicts_with = "name")]
        clear: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Atribui um bloco a outro falante
    BlockSpeaker {
        call: String,
        seq: i64,
        speaker: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum GlossaryCmd {
    /// Lista as regras: sem --library, as globais; com --library e --client, as em vigor para o
    /// cliente (cliente sobrescreve global; as escondidas vêm com overridden=true); só com
    /// --library, as globais mais as de todos os clientes dela
    List {
        #[arg(long)]
        library: Option<String>,
        #[arg(long, requires = "library")]
        client: Option<String>,
        /// Só um tipo
        #[arg(long, value_parser = ["term", "replace"])]
        kind: Option<String>,
    },
    /// Cria uma regra: com <substituição> é "errado → certo"; sem, é um termo
    Add {
        /// Texto a procurar (ou o termo)
        pattern: String,
        /// Texto que entra no lugar
        #[arg(conflicts_with = "term")]
        replacement: Option<String>,
        /// Empresa/projeto do cliente (id ou nome)
        #[arg(long, requires = "client", conflicts_with = "global")]
        library: Option<String>,
        #[arg(long, requires = "library", conflicts_with = "global")]
        client: Option<String>,
        /// Regra global (vale para todas as chamadas)
        #[arg(long)]
        global: bool,
        /// Diferencia maiúsculas de minúsculas
        #[arg(long)]
        case_sensitive: bool,
        /// Cria um termo (o padrão quando não há substituição)
        #[arg(long)]
        term: bool,
    },
    /// Remove uma regra pelo id (ids globais e de cliente são independentes)
    Remove {
        id: i64,
        #[arg(long, conflicts_with = "global")]
        library: Option<String>,
        #[arg(long)]
        global: bool,
    },
    /// Promove uma regra de cliente a global (a cópia do cliente é removida)
    Promote {
        id: i64,
        #[arg(long)]
        library: String,
    },
    /// Aplica o glossário em vigor a uma chamada; tudo vira um lote só, desfeito por `undo`
    Apply {
        call: String,
        /// Versão da transcrição (padrão: a ativa)
        #[arg(long)]
        version: Option<i64>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Importa um arquivo de texto UTF-8: uma entrada por linha, `#` comenta, `errado -> certo`
    /// (ou → ou =>) vira substituição e o resto vira termo
    Import {
        file: PathBuf,
        #[arg(long, requires = "client", conflicts_with = "global")]
        library: Option<String>,
        #[arg(long, requires = "library", conflicts_with = "global")]
        client: Option<String>,
        #[arg(long)]
        global: bool,
        /// Só aceita esse tipo de linha
        #[arg(long, value_parser = ["term", "replace"])]
        kind: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Mostra as regras que seriam sugeridas por uma edição <antes> → <depois> (depuração)
    Suggest { old: String, new: String },
    /// Termos que iriam para o prompt do modelo, dentro do orçamento de tokens
    Terms {
        #[arg(long)]
        library: Option<String>,
        #[arg(long, requires = "library")]
        client: Option<String>,
    },
}

#[derive(Subcommand)]
enum LibraryCmd {
    List,
    /// Cadastra (ou adota uma pasta já existente com library.db)
    Add { name: String, path: PathBuf },
    Rename { library: String, name: String },
    /// Só descadastra; a pasta fica no disco
    Remove { library: String },
}

#[derive(Subcommand)]
enum ClientCmd {
    List { #[arg(long)] library: String },
    Add { #[arg(long)] library: String, name: String },
    Rename { #[arg(long)] library: String, client: String, name: String },
}

#[derive(Subcommand)]
enum SettingsCmd {
    Get,
    /// Chaves: language (pt-BR|en-US|es-419), me_name, transcription_language
    Set { key: String, value: Option<String> },
    /// Muda o diretório de dados (vale na próxima abertura; os dados não são movidos)
    DataDir { path: Option<PathBuf> },
}

const SETTING_KEYS: &[&str] = &["language", "me_name", "transcription_language"];

/// Idioma da ajuda, decidido antes do `clap` ler a linha: `--lang` (ou `--lang=`) olhado direto nos
/// argumentos, senão o do sistema; o que não for pt-BR/en-US/es-419 vira inglês (`Lang::parse`/`system`).
/// A ajuda não abre o banco, então a configuração `language` do app não entra aqui.
fn help_lang(args: &[std::ffi::OsString]) -> Lang {
    let mut it = args.iter().skip(1).map(|a| a.to_string_lossy());
    while let Some(a) = it.next() {
        let value = match &*a {
            "--" => break,
            "--lang" => it.next().map(|v| v.into_owned()),
            s => s.strip_prefix("--lang=").map(str::to_string),
        };
        if let Some(v) = value {
            return Lang::parse(&v).unwrap_or_else(Lang::system);
        }
    }
    Lang::system()
}

/// `Cli::try_parse_from` com a ajuda (`--help`, subcomandos, erros de uso) em `lang`.
fn parse_localized(args: Vec<std::ffi::OsString>, lang: Lang) -> Result<Cli, clap::Error> {
    let mut cmd = crate::help::localize(Cli::command(), lang);
    let mut matches = cmd.clone().try_get_matches_from(args)?;
    Cli::from_arg_matches_mut(&mut matches).map_err(|e| e.format(&mut cmd))
}

pub fn run(args: Vec<std::ffi::OsString>) -> i32 {
    let cli = match parse_localized(args.clone(), help_lang(&args)) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    if matches!(cli.cmd, Cmd::Gui) {
        crate::gui::run(cli.data_dir);
        return 0;
    }
    if let Err(blocked) = paths::migrate_legacy(cli.data_dir.as_deref()) {
        eprintln!("rstt: {}", crate::i18n::legacy_running_message(cli.lang.as_deref().and_then(Lang::parse).unwrap_or_else(Lang::system), &blocked));
        return 1;
    }
    let data_dir = paths::resolve_data_dir(cli.data_dir.as_deref());
    let mut lang = cli.lang.as_deref().and_then(Lang::parse);
    let result = App::open(&data_dir).and_then(|app| {
        if lang.is_none() {
            lang = app.setting("language").ok().flatten().as_deref().and_then(Lang::parse);
        }
        exec(&app, cli.cmd, lang.unwrap_or_else(Lang::system), cli.json)
    });
    let lang = lang.unwrap_or_else(Lang::system);
    match result {
        Ok(Output::Json(v, changed)) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            if let Some(c) = changed {
                ipc::notify(&data_dir, &c);
            }
            0
        }
        Ok(Output::Text(s)) => {
            print!("{s}");
            0
        }
        Err(e) => {
            let message = format!("{}: {}", i18n::error_prefix(lang, e.code()), e.detail());
            eprintln!("{}", serde_json::to_string_pretty(&json!({"error": {"code": e.code(), "message": message}})).unwrap());
            1
        }
    }
}

enum Output {
    /// JSON + aviso para a app aberta (quando algo mudou)
    Json(Value, Option<Value>),
    Text(String),
}

fn changed(library_id: i64, call_id: Option<i64>) -> Option<Value> {
    Some(json!({"event": "changed", "library_id": library_id, "call_id": call_id}))
}

fn to_json<T: serde::Serialize>(v: T) -> core_lib::Result<Value> {
    Ok(serde_json::to_value(v)?)
}

// ------------------------------------------------------------------ gravação (fase 3)
//
// A CLI nunca captura áudio: fala com a GUI pelo socket (`ipc`). Saída padrão: uma linha de texto
// traduzida (`gravando: <título>` / `parado: <chave>` / `idle` / `recording mm:ss <título>`);
// `--json` devolve o `Status` da GUI (chaves em inglês, estáveis). `record devices` é sempre JSON.

fn ipc_error(e: ipc::IpcError) -> Error {
    match e {
        ipc::IpcError::NotRunning => Error::recording("not_running", "no answer on the app socket"),
        ipc::IpcError::Io(e) => Error::Io(e),
        ipc::IpcError::Protocol(s) => Error::recording("bad_request", s),
    }
}

/// `ok:false` da GUI → o mesmo código de erro (os do núcleo/recorder têm código estático).
fn response_error(code: &str, detail: String) -> Error {
    match code {
        "not_found" => Error::NotFound(detail),
        "invalid" => Error::Invalid(detail),
        "conflict" => Error::Conflict(detail),
        "already_recording" => Error::recording("already_recording", detail),
        "not_recording" => Error::recording("not_recording", detail),
        "not_running" => Error::recording("not_running", detail),
        "device_not_found" => Error::recording("device_not_found", detail),
        "device_open_failed" => Error::recording("device_open_failed", detail),
        "backend_unavailable" => Error::recording("backend_unavailable", detail),
        "capture_failed" => Error::recording("capture_failed", detail),
        "invalid_wav" => Error::recording("invalid_wav", detail),
        "empty_recording" => Error::recording("empty_recording", detail),
        "not_implemented" => Error::recording("not_implemented", detail),
        _ => Error::recording("bad_request", format!("{code}: {detail}")),
    }
}

/// Envia a requisição; `ok:false` vira erro. Devolve o corpo (`Status`) sem o `ok`.
fn call(sock: &std::path::Path, req: &Request) -> core_lib::Result<serde_json::Map<String, Value>> {
    let resp = ipc::request_on(sock, req, ipc::REQUEST_TIMEOUT).map_err(ipc_error)?;
    if resp.ok { Ok(resp.data) } else { Err(response_error(resp.code.as_deref().unwrap_or("error"), resp.detail.unwrap_or_default())) }
}

fn parse_choice(s: &str) -> StreamChoice {
    match s {
        "default" => StreamChoice::Default,
        "off" => StreamChoice::Off,
        name => StreamChoice::Named(name.to_string()),
    }
}

/// Resolve `--library`/`--client` (nome ou id) e monta o pedido. `fill_last`: para `toggle`, o que não
/// foi dito vem do último uso (como no atalho global); `start` usa os padrões (inbox, `default`).
fn start_request(app: &App, a: &RecordArgs, fill_last: bool) -> core_lib::Result<StartRequest> {
    let (library_id, client_id) = match &a.library {
        Some(l) => {
            let row = app.find_library(l)?;
            let client = match &a.client {
                Some(c) => Some(Library::open(row.clone())?.find_client(c)?.id),
                None => None,
            };
            (Some(row.id), client)
        }
        None => (None, None),
    };
    let meta = Meta { library_id, client_id, title: a.title.clone(), expected_speakers: a.expected_speakers, language: a.language.clone() };
    let (mic, sys) = (a.mic.as_deref().map(parse_choice), a.sys.as_deref().map(parse_choice));
    if fill_last {
        Ok(crate::recording::build_request(&core_rec::last_used(app)?, meta, mic, sys))
    } else {
        Ok(StartRequest { meta, mic: mic.unwrap_or_default(), sys: sys.unwrap_or_default() })
    }
}

/// "Chamada de <data>" quando não há título (mesmo texto da tela).
fn display_title(lang: Lang, rec: &Value) -> String {
    match rec["title"].as_str().filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => format!("{} {}", i18n::msg(lang, "call_untitled"), rec["started_at"].as_str().unwrap_or("").replace('T', " ")),
    }
}

/// Linha de texto de `record start|toggle|stop` a partir do `Status` devolvido.
fn record_line(lang: Lang, status: &serde_json::Map<String, Value>) -> String {
    if status["state"] == "recording" {
        format!("{}: {}\n", i18n::msg(lang, "rec_recording"), display_title(lang, &status["recording"]))
    } else {
        let key = status["finalizing"].as_array().and_then(|a| a.last()).and_then(Value::as_str).unwrap_or("");
        format!("{}: {key}\n", i18n::msg(lang, "rec_stopped"))
    }
}

fn status_output(json_out: bool, status: serde_json::Map<String, Value>, line: impl FnOnce(&serde_json::Map<String, Value>) -> String) -> Output {
    if json_out { Output::Json(Value::Object(status), None) } else { Output::Text(line(&status)) }
}

/// `record start|stop|toggle|devices`. `sock` = socket da GUI (parâmetro para os testes).
/// - `start`/`toggle`: resolve `--library/--client`, `ipc::ensure_running_on` (sobe a GUI sem janela se
///   preciso) e envia o pedido. Resposta `ok:false` → erro com o mesmo `code` (exit 1).
/// - `stop`: sem app → erro `not_running`.
/// - `devices`: direto do `recorder::default_backend().list_devices()` (JSON), sem app.
fn exec_record(app: &App, what: RecordCmd, lang: Lang, json_out: bool, sock: &std::path::Path) -> core_lib::Result<Output> {
    match what {
        RecordCmd::Devices => {
            let backend = recorder::default_backend();
            let devices = backend.list_devices()?;
            Ok(Output::Json(json!({"backend": backend.id(), "devices": devices}), None))
        }
        RecordCmd::Start(a) => {
            let req = Request::RecordStart(start_request(app, &a, false)?);
            ipc::ensure_running_on(sock, &app.data_dir, ipc::START_WAIT).map_err(ipc_error)?;
            let status = call(sock, &req)?;
            Ok(status_output(json_out, status, |s| record_line(lang, s)))
        }
        RecordCmd::Toggle(a) => {
            let req = Request::RecordToggle(start_request(app, &a, true)?);
            ipc::ensure_running_on(sock, &app.data_dir, ipc::START_WAIT).map_err(ipc_error)?;
            let status = call(sock, &req)?;
            Ok(status_output(json_out, status, |s| record_line(lang, s)))
        }
        RecordCmd::Stop => {
            let status = call(sock, &Request::RecordStop)?;
            Ok(status_output(json_out, status, |s| record_line(lang, s)))
        }
    }
}

/// `status`: sem app → `Status::not_running()` (exit 0).
fn exec_status(_app: &App, lang: Lang, json_out: bool, sock: &std::path::Path) -> core_lib::Result<Output> {
    let status = match call(sock, &Request::Status) {
        Ok(s) => s,
        Err(Error::Recording("not_running", _)) => match serde_json::to_value(ipc::Status::not_running())? {
            Value::Object(m) => m,
            _ => unreachable!(),
        },
        Err(e) => return Err(e),
    };
    Ok(status_output(json_out, status, |s| {
        if s["state"] == "recording" {
            let rec = &s["recording"];
            format!("recording {} {}\n", fmt_time(rec["elapsed_s"].as_f64().unwrap_or(0.0)), display_title(lang, rec))
        } else {
            "idle\n".to_string()
        }
    }))
}

/// `bar show|hide`: sem app → erro `not_running`.
fn exec_bar(_app: &App, what: BarCmd, lang: Lang, json_out: bool, sock: &std::path::Path) -> core_lib::Result<Output> {
    let (req, key) = match what {
        BarCmd::Show => (Request::BarShow, "bar_shown"),
        BarCmd::Hide => (Request::BarHide, "bar_hidden"),
    };
    call(sock, &req)?;
    if json_out {
        // o `bar_show` só responde `ok`; o estado completo vem de um `status`
        return Ok(Output::Json(Value::Object(call(sock, &Request::Status)?), None));
    }
    Ok(Output::Text(format!("{}\n", i18n::msg(lang, key))))
}

// ------------------------------------------------------------------ transcrição (fase 4)
//
// A CLI só grava em `app.db`: quem transcreve é a thread da fila da app, que descobre as tarefas por polling.
// Por isso `transcribe`/`queue retry` garantem a app de pé (sobe escondida) DEPOIS de gravar a tarefa; se ela não
// subir, a tarefa fica na fila e um aviso vai para o stderr (o comando não falha).

fn ensure_gui(app: &App, lang: Lang, sock: &std::path::Path) {
    if ipc::ensure_running_on(sock, &app.data_dir, ipc::START_WAIT).is_err() {
        eprintln!("warning: {}", i18n::msg(lang, "gui_not_started"));
    }
}

fn parse_job_kind(kind: Option<&str>) -> core_lib::Result<JobKind> {
    match kind {
        None | Some("full") => Ok(JobKind::Full),
        Some("rediarize") => Ok(JobKind::Rediarize),
        Some("resegment") => Ok(JobKind::Resegment),
        Some(other) => Err(Error::invalid(format!("unknown kind {other}"))),
    }
}

fn job_options(a: &TranscribeArgs) -> core_lib::Result<JobOptions> {
    let language = match &a.language {
        Some(l) => Some(core_rec::language_code(l).ok_or_else(|| Error::invalid("language must be auto, pt, en or es"))?.to_string()),
        None => None,
    };
    if a.expected_speakers.is_some_and(|n| n < 1) {
        return Err(Error::invalid("speakers must be 1 or more"));
    }
    Ok(JobOptions { language, expected_speakers: a.expected_speakers, bleed_filter: a.no_bleed_filter.then_some(false), ..Default::default() })
}

/// `transcribe <chamada>` → `JobInfo`; `transcribe --pending` → `JobInfo[]`. `--dry-run` não cria nada.
fn exec_transcribe(app: &App, a: TranscribeArgs, lang: Lang, sock: &std::path::Path) -> core_lib::Result<Output> {
    let dry = |result: Value| Ok(Output::Json(json!({"dry_run": true, "message": i18n::msg(lang, "dry_run"), "result": result}), None));
    if a.pending {
        if a.dry_run {
            let mut found = Vec::new();
            for row in app.library_rows()? {
                if !row.is_inbox() && !row.root.join(core_lib::library::DB_FILE).is_file() {
                    continue;
                }
                for c in Library::open(row)?.calls(ClientFilter::Any)? {
                    if c.transcription_state == core_lib::model::TRANSCRIPTION_PENDING && c.has_audio {
                        found.push(json!({"library_id": c.library_id, "call_id": c.id, "call_key": c.key}));
                    }
                }
            }
            return dry(Value::Array(found));
        }
        let jobs = queue::enqueue_pending(app)?;
        if !jobs.is_empty() {
            ensure_gui(app, lang, sock);
        }
        let changed = (!jobs.is_empty()).then(|| json!({"event": "changed"}));
        return Ok(Output::Json(to_json(jobs)?, changed));
    }
    let (library_id, call_id) = app.find_call(a.call.as_deref().unwrap_or_default())?;
    let kind = parse_job_kind(a.kind.as_deref())?;
    let options = job_options(&a)?;
    if a.dry_run {
        let key = app.open_library(library_id)?.call_summary(call_id)?.key;
        return dry(json!({"library_id": library_id, "call_id": call_id, "call_key": key, "kind": kind, "options": options}));
    }
    let job = queue::enqueue(app, library_id, call_id, kind, &options)?;
    ensure_gui(app, lang, sock);
    Ok(Output::Json(to_json(job)?, changed(library_id, Some(call_id))))
}

/// Há gravação em curso? Pergunta à app aberta pelo socket (`status`), a mesma fonte que a GUI usa para
/// pausar a fila. Sem app, ou sem resposta: não há gravação (nunca sobe a GUI só para listar a fila).
fn recording_now(sock: &std::path::Path) -> bool {
    call(sock, &Request::Status).is_ok_and(|s| s["state"] == "recording")
}

/// Fila como a GUI a mostra: `paused` é o motivo que a GUI mostra (`pause_reason`: com os dois ao mesmo
/// tempo vale `user`, que continua depois que a gravação acaba) e `paused_reasons` lista todos os
/// motivos ativos (`["user", "recording"]`), para não esconder nenhum.
fn queue_json(app: &App, sock: &std::path::Path, recent: usize) -> core_lib::Result<Value> {
    let user = app.setting(keys::QUEUE_PAUSED).ok().flatten().as_deref() == Some("1");
    let recording = recording_now(sock);
    let mut v = to_json(queue::status(app, crate::transcription::pause_reason(user, recording), recent)?)?;
    let reasons: Vec<PauseReason> = [user.then_some(PauseReason::User), recording.then_some(PauseReason::Recording)].into_iter().flatten().collect();
    v["paused_reasons"] = to_json(reasons)?;
    Ok(v)
}

/// `queue [list]` lê o banco; `pause`/`resume`/`cancel` gravam e a app aplica no polling. `cancel` só alcança
/// tarefas enfileiradas: a em andamento é da thread da app (erro `conflict` com a dica).
fn exec_queue(app: &App, what: QueueCmd, lang: Lang, sock: &std::path::Path) -> core_lib::Result<Output> {
    const RECENT: usize = 20;
    let notify = || Some(json!({"event": "changed"}));
    match what {
        QueueCmd::List => Ok(Output::Json(queue_json(app, sock, RECENT)?, None)),
        QueueCmd::Pause | QueueCmd::Resume => {
            let paused = matches!(what, QueueCmd::Pause);
            app.set_setting(keys::QUEUE_PAUSED, Some(if paused { "1" } else { "0" }))?;
            Ok(Output::Json(queue_json(app, sock, RECENT)?, notify()))
        }
        QueueCmd::Cancel { id } => {
            match queue::get(app, id)?.state {
                JobState::Queued => queue::mark_cancelled(app, id)?,
                JobState::Running => return Err(Error::Conflict(i18n::msg(lang, "queue_cancel_running").into())),
                _ => return Err(Error::Conflict(format!("job {id} already finished"))),
            }
            Ok(Output::Json(to_json(queue::get(app, id)?)?, notify()))
        }
        QueueCmd::Retry { id } => {
            let job = queue::retry(app, id)?;
            ensure_gui(app, lang, sock);
            Ok(Output::Json(to_json(job)?, notify()))
        }
    }
}

/// `setup status`: o JSON de `transcription_status` sem a fila. `setup install`: instala no próprio processo.
fn exec_setup(app: &App, what: SetupCmd, lang: Lang) -> core_lib::Result<Output> {
    if matches!(what, SetupCmd::Install) {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        if !matches!(transcription::runtime_status(&app.data_dir)?.state.as_str(), "ready" | "fake") {
            let step = i18n::msg(lang, "setup_runtime");
            runtime::ensure(&app.data_dir, &mut |p| eprintln!("{step} ({}/{}): {}", p.index, p.of, p.step), &cancel)?;
        }
        let step = i18n::msg(lang, "setup_model");
        let mut last: (String, u64) = (String::new(), u64::MAX);
        models::ensure(
            &app.data_dir,
            &[],
            &mut |p| {
                // uma linha por arquivo e a cada 10 %
                let pct = p.bytes_done * 100 / p.bytes_total.max(1);
                if (last.0.as_str(), last.1 / 10) != (p.file.as_str(), pct / 10) {
                    eprintln!("{step} {}/{}: {pct}%", p.model, p.file);
                    last = (p.file.clone(), pct);
                }
            },
            &cancel,
        )?;
        eprintln!("{}", i18n::msg(lang, "setup_done"));
    }
    let fake = transcription::fake_env().is_some();
    Ok(Output::Json(
        json!({
            "runtime": transcription::runtime_status(&app.data_dir)?,
            "models": models::status(&app.data_dir)?,
            "setup": {"running": false, "phase": null},
            "fake_worker": fake,
        }),
        None,
    ))
}

fn find_call(app: &App, reference: &str) -> core_lib::Result<(Library, i64)> {
    let (lib, id) = app.find_call(reference)?;
    Ok((app.open_library(lib)?, id))
}

/// Socket da app; nos testes, um caminho sem servidor dentro do tempdir (nunca consulta a app real do usuário).
fn sock(app: &App) -> std::path::PathBuf {
    if cfg!(test) { app.data_dir.join("sem-app.sock") } else { paths::socket_path(&app.data_dir) }
}

fn exec(app: &App, cmd: Cmd, lang: Lang, json_out: bool) -> core_lib::Result<Output> {
    let json = |v: Value| Ok(Output::Json(v, None));
    let dry = |dry_run: bool, v: Value, lib: i64, call: Option<i64>| {
        if dry_run {
            Ok(Output::Json(json!({"dry_run": true, "message": i18n::msg(lang, "dry_run"), "result": v}), None))
        } else {
            Ok(Output::Json(v, changed(lib, call)))
        }
    };
    match cmd {
        Cmd::Gui => unreachable!(),
        Cmd::List { library, client, unassigned } => {
            let rows = match &library {
                Some(l) => vec![app.find_library(l)?],
                None => app.library_rows()?,
            };
            let mut out = Vec::new();
            for row in rows {
                if !row.is_inbox() && !row.root.join(core_lib::library::DB_FILE).is_file() {
                    continue;
                }
                let lib = Library::open(row)?;
                let filter = match &client {
                    Some(c) => ClientFilter::Id(lib.find_client(c)?.id),
                    None if unassigned => ClientFilter::Unassigned,
                    None => ClientFilter::Any,
                };
                out.extend(lib.calls(filter)?);
            }
            out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
            json(to_json(out)?)
        }
        Cmd::Show { call, version, text } => {
            let (lib, id) = find_call(app, &call)?;
            let tid = match version {
                Some(v) => Some(
                    lib.transcripts(id)?
                        .into_iter()
                        .find(|t| t.version == v)
                        .ok_or_else(|| Error::not_found(format!("version {v}")))?
                        .id,
                ),
                None => None,
            };
            let d = lib.call_detail(id, tid)?;
            if text {
                return Ok(Output::Text(render_text(app, &d, lang)));
            }
            json(to_json(d)?)
        }
        Cmd::Search { query, limit } => {
            let hits = search::search(app, &query.join(" "), limit)?;
            json(to_json(hits)?)
        }
        Cmd::Edit { what } => match what {
            EditCmd::Block { call, seq, text, dry_run } => {
                let (mut lib, id) = find_call(app, &call)?;
                let b = lib.block_id_by_seq(id, seq)?;
                let r = app.edit_block_text(&mut lib, b, &text, Origin::Cli, dry_run)?;
                dry(dry_run, to_json(r)?, lib.id(), Some(id))
            }
            EditCmd::Revert { call, seq, dry_run } => {
                let (mut lib, id) = find_call(app, &call)?;
                let b = lib.block_id_by_seq(id, seq)?;
                let r = lib.revert_block(b, Origin::Cli, dry_run)?;
                dry(dry_run, to_json(r)?, lib.id(), Some(id))
            }
            EditCmd::Title { call, title, dry_run } => {
                let (mut lib, id) = find_call(app, &call)?;
                let r = lib.set_title(id, &title, Origin::Cli, dry_run)?;
                dry(dry_run, to_json(r)?, lib.id(), Some(id))
            }
            EditCmd::Speaker { call, speaker, name, clear, dry_run } => {
                let (mut lib, id) = find_call(app, &call)?;
                if name.is_none() && !clear {
                    return Err(Error::invalid("give a name or --clear"));
                }
                let s = lib.find_speaker(id, &speaker)?;
                let r = lib.rename_speaker(s.id, name.as_deref(), Origin::Cli, dry_run)?;
                dry(dry_run, to_json(r)?, lib.id(), Some(id))
            }
            EditCmd::BlockSpeaker { call, seq, speaker, dry_run } => {
                let (mut lib, id) = find_call(app, &call)?;
                let b = lib.block_id_by_seq(id, seq)?;
                let s = lib.find_speaker(id, &speaker)?;
                let r = lib.set_block_speaker(b, s.id, Origin::Cli, dry_run)?;
                dry(dry_run, to_json(r)?, lib.id(), Some(id))
            }
        },
        Cmd::History { call, limit } => match call {
            Some(c) => {
                let (lib, id) = find_call(app, &c)?;
                json(to_json(lib.history(Some(id), limit)?)?)
            }
            None => {
                let mut all = Vec::new();
                for row in app.library_rows()? {
                    if row.is_inbox() || row.root.join(core_lib::library::DB_FILE).is_file() {
                        let lib = Library::open(row)?;
                        for h in lib.history(None, limit)? {
                            all.push(json!({"library_id": lib.id(), "entry": h}));
                        }
                    }
                }
                all.sort_by(|a, b| b["entry"]["at"].as_str().cmp(&a["entry"]["at"].as_str()));
                all.truncate(limit.max(0) as usize);
                json(Value::Array(all))
            }
        },
        Cmd::Undo { call, dry_run } => {
            let (mut lib, id) = find_call(app, &call)?;
            match lib.undo(Some(id), dry_run)? {
                Some(e) => dry(dry_run, to_json(e)?, lib.id(), Some(id)),
                None => json(json!({"undone": null, "message": i18n::msg(lang, "nothing_to_undo")})),
            }
        }
        Cmd::Import { paths, library, client, no_audio, dry_run } => {
            let (library_id, client_id) = match &library {
                Some(l) => {
                    let row = app.find_library(l)?;
                    let cid = match &client {
                        Some(c) => Some(Library::open(row.clone())?.find_client(c)?.id),
                        None => None,
                    };
                    (Some(row.id), cid)
                }
                None => (None, None),
            };
            let cands = import::scan(&paths)?;
            let opts = ImportOptions { library_id, client_id, convert_audio: !no_audio, dry_run };
            let mut last_pct = u64::MAX;
            let report = import::import(app, &cands, &opts, &mut |p| {
                if let import::Progress::Audio { key, track, done, of, .. } = &p {
                    let pct = done * 100 / (*of).max(1);
                    if pct / 10 != last_pct / 10 {
                        eprintln!("{key} {track}: {pct}%");
                        last_pct = pct;
                    }
                }
            })?;
            for i in &report.items {
                if let Some(reason) = &i.reason {
                    eprintln!("warning: {} {}: {reason}", i.key, i.status);
                }
                for e in &i.audio_errors {
                    eprintln!("warning: {} audio: {e}", i.key);
                }
            }
            let any = report.items.iter().any(|i| i.status != "unchanged" && i.status != "skipped");
            let v = to_json(&report)?;
            Ok(Output::Json(v, (any && !dry_run).then(|| json!({"event": "changed"}))))
        }
        Cmd::Assign { call, library, client, inbox } => {
            let (from, id) = app.find_call(&call)?;
            let (to, cid) = if inbox {
                (app.inbox_id()?, None)
            } else {
                let row = app.find_library(library.as_deref().unwrap())?;
                let cid = match &client {
                    Some(c) => Some(Library::open(row.clone())?.find_client(c)?.id),
                    None => None,
                };
                (row.id, cid)
            };
            let (lib, new_id) = transfer::assign(app, from, id, to, cid)?;
            let summary = app.open_library(lib)?.call_summary(new_id)?;
            Ok(Output::Json(to_json(summary)?, Some(json!({"event": "changed"}))))
        }
        Cmd::Library { what } => match what.unwrap_or(LibraryCmd::List) {
            LibraryCmd::List => json(to_json(app.libraries()?)?),
            LibraryCmd::Add { name, path } => {
                let row = app.add_library(&name, &path)?;
                Ok(Output::Json(json!({"id": row.id, "name": row.name, "path": row.root}), Some(json!({"event": "changed"}))))
            }
            LibraryCmd::Rename { library, name } => {
                let row = app.find_library(&library)?;
                app.rename_library(row.id, &name)?;
                Ok(Output::Json(json!({"id": row.id, "name": name}), Some(json!({"event": "changed"}))))
            }
            LibraryCmd::Remove { library } => {
                let row = app.find_library(&library)?;
                app.remove_library(row.id)?;
                Ok(Output::Json(json!({"removed": row.id, "path": row.root}), Some(json!({"event": "changed"}))))
            }
        },
        Cmd::Client { what } => match what {
            ClientCmd::List { library } => json(to_json(app.open_library(app.find_library(&library)?.id)?.clients()?)?),
            ClientCmd::Add { library, name } => {
                let lib = app.open_library(app.find_library(&library)?.id)?;
                let c = lib.add_client(&name)?;
                Ok(Output::Json(to_json(c)?, changed(lib.id(), None)))
            }
            ClientCmd::Rename { library, client, name } => {
                let lib = app.open_library(app.find_library(&library)?.id)?;
                let c = lib.find_client(&client)?;
                lib.rename_client(c.id, &name)?;
                Ok(Output::Json(json!({"id": c.id, "name": name}), changed(lib.id(), None)))
            }
        },
        Cmd::Glossary { what } => exec_glossary(app, what, lang),
        // fase 3: corpos a cargo do agente B (contrato: RECORDING_CONTRACT.md, "CLI")
        Cmd::Record { what } => exec_record(app, what, lang, json_out, &sock(app)),
        Cmd::Status => exec_status(app, lang, json_out, &sock(app)),
        Cmd::Bar { what } => exec_bar(app, what, lang, json_out, &sock(app)),
        Cmd::Transcribe(a) => exec_transcribe(app, a, lang, &sock(app)),
        Cmd::Queue { what } => exec_queue(app, what.unwrap_or(QueueCmd::List), lang, &sock(app)),
        Cmd::Setup { what } => exec_setup(app, what, lang),
        Cmd::Reclaimable => {
            let files = import::reclaimable(app)?;
            let total: u64 = files.iter().map(|f| f.size).sum();
            json(json!({"total_bytes": total, "files": files}))
        }
        Cmd::Settings { what } => match what.unwrap_or(SettingsCmd::Get) {
            SettingsCmd::Get => {
                let mut s = app.settings()?;
                s.insert("data_dir".into(), json!(app.data_dir));
                json(Value::Object(s))
            }
            SettingsCmd::Set { key, value } => {
                if !SETTING_KEYS.contains(&key.as_str()) {
                    return Err(Error::invalid(format!("unknown setting {key}; use one of {}", SETTING_KEYS.join(", "))));
                }
                if key == "language"
                    && let Some(v) = &value
                    && Lang::parse(v).is_none()
                {
                    return Err(Error::invalid("language must be pt-BR, en-US or es-419"));
                }
                let value = value.map(|v| if key == "language" { Lang::parse(&v).unwrap().tag().to_string() } else { v });
                app.set_setting(&key, value.as_deref())?;
                Ok(Output::Json(json!({key: value}), Some(json!({"event": "settings"}))))
            }
            SettingsCmd::DataDir { path } => {
                let mut cfg = paths::load_config();
                cfg.data_dir = path;
                paths::save_config(&cfg)?;
                json(json!({"data_dir": paths::resolve_data_dir(None), "config_file": paths::config_file()}))
            }
        },
    }
}

/// Escopo de uma regra na linha de comando: `--global` ou `--library` + `--client`.
enum Target {
    Global,
    Client(Box<Library>, ClientInfo),
}

fn target(app: &App, library: &Option<String>, client: &Option<String>, global: bool) -> core_lib::Result<Target> {
    match (global, library, client) {
        (true, None, None) => Ok(Target::Global),
        (false, Some(l), Some(c)) => {
            let lib = app.open_library(app.find_library(l)?.id)?;
            let client = lib.find_client(c)?;
            Ok(Target::Client(Box::new(lib), client))
        }
        _ => Err(Error::invalid("give --global, or --library together with --client")),
    }
}

fn rule_changed(library_id: Option<i64>) -> Option<Value> {
    Some(json!({"event": "glossary", "library_id": library_id}))
}

fn exec_glossary(app: &App, cmd: GlossaryCmd, lang: Lang) -> core_lib::Result<Output> {
    let json_out = |v: Value, changed: Option<Value>| Ok(Output::Json(v, changed));
    let kind_of = |k: &Option<String>| k.as_deref().and_then(RuleKind::parse);
    match cmd {
        GlossaryCmd::List { library, client, kind } => {
            let mut rules: Vec<Rule> = match (&library, &client) {
                (None, _) => app.global_rules()?,
                (Some(l), Some(c)) => {
                    let lib = app.open_library(app.find_library(l)?.id)?;
                    app.merged_rules(&lib, Some(lib.find_client(c)?.id))?
                }
                (Some(l), None) => {
                    let lib = app.open_library(app.find_library(l)?.id)?;
                    let mut all = Vec::new();
                    for c in lib.clients()? {
                        all.extend(lib.client_rules(c.id)?);
                    }
                    all.extend(app.global_rules()?);
                    all
                }
            };
            if let Some(k) = kind_of(&kind) {
                rules.retain(|r| r.kind == k);
            }
            json_out(to_json(rules)?, None)
        }
        GlossaryCmd::Add { pattern, replacement, library, client, global, case_sensitive, term } => {
            let kind = if replacement.is_some() && !term { RuleKind::Replace } else { RuleKind::Term };
            let input = RuleInput { kind, pattern, replacement, case_sensitive };
            match target(app, &library, &client, global)? {
                Target::Global => json_out(to_json(app.add_global_rule(&input, None)?)?, rule_changed(None)),
                Target::Client(lib, c) => json_out(to_json(lib.add_client_rule(c.id, &input, None)?)?, rule_changed(Some(lib.id()))),
            }
        }
        GlossaryCmd::Remove { id, library, global } => match (global, library) {
            (true, None) => json_out(json!({"removed": to_json(app.remove_global_rule(id)?)?}), rule_changed(None)),
            (false, Some(l)) => {
                let lib = app.open_library(app.find_library(&l)?.id)?;
                json_out(json!({"removed": to_json(lib.remove_client_rule(id)?)?}), rule_changed(Some(lib.id())))
            }
            _ => Err(Error::invalid("give --global or --library")),
        },
        GlossaryCmd::Promote { id, library } => {
            let lib = app.open_library(app.find_library(&library)?.id)?;
            json_out(to_json(app.promote_rule(&lib, id)?)?, rule_changed(Some(lib.id())))
        }
        GlossaryCmd::Apply { call, version, dry_run } => {
            let (mut lib, id) = find_call(app, &call)?;
            let tid = match version {
                Some(v) => Some(
                    lib.transcripts(id)?
                        .into_iter()
                        .find(|t| t.version == v)
                        .ok_or_else(|| Error::not_found(format!("version {v}")))?
                        .id,
                ),
                None => None,
            };
            let report = app.apply_glossary(&mut lib, id, tid, Origin::Cli, dry_run)?;
            let none = report.blocks_changed == 0;
            let mut v = to_json(&report)?;
            if dry_run {
                v["message"] = json!(i18n::msg(lang, "dry_run"));
            } else if none {
                v["message"] = json!(i18n::msg(lang, "glossary_nothing_to_apply"));
            }
            json_out(v, (!dry_run && !none).then(|| changed(lib.id(), Some(id))).flatten())
        }
        GlossaryCmd::Import { file, library, client, global, kind, dry_run } => {
            let scope = match target(app, &library, &client, global)? {
                Target::Global => ImportScope::Global,
                Target::Client(lib, c) => ImportScope::Client { library_id: lib.id(), client_id: c.id },
            };
            let library_id = if let ImportScope::Client { library_id, .. } = scope { Some(library_id) } else { None };
            let report = app.import_glossary_file(&file, scope, kind_of(&kind), dry_run)?;
            let mut v = to_json(&report)?;
            if dry_run {
                v["message"] = json!(i18n::msg(lang, "dry_run"));
            }
            json_out(v, (!dry_run && report.added > 0).then(|| rule_changed(library_id)).flatten())
        }
        GlossaryCmd::Suggest { old, new } => {
            let found = glossary::suggest_from_edit(&old, &new);
            if found.is_empty() {
                return json_out(json!({"suggestions": [], "message": i18n::msg(lang, "glossary_no_suggestions")}), None);
            }
            json_out(json!({"suggestions": found}), None)
        }
        GlossaryCmd::Terms { library, client } => {
            let terms = match (&library, &client) {
                (Some(l), Some(c)) => {
                    let lib = app.open_library(app.find_library(l)?.id)?;
                    app.prompt_terms(&lib, Some(lib.find_client(c)?.id))?
                }
                _ => {
                    let inbox = app.open_library(app.inbox_id()?)?;
                    app.prompt_terms(&inbox, None)?
                }
            };
            let tokens: usize = terms.iter().map(|t| glossary::estimate_tokens(t) + 1).sum();
            json_out(json!({"terms": terms, "estimated_tokens": tokens, "budget_tokens": glossary::PROMPT_TOKEN_BUDGET}), None)
        }
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// Texto corrido para ler no terminal: `#seq [mm:ss] Falante: texto`.
fn render_text(app: &App, d: &core_lib::model::CallDetail, lang: Lang) -> String {
    let me = app.setting("me_name").ok().flatten();
    let title = if d.summary.title.is_empty() {
        format!("{} {}", i18n::msg(lang, "call_untitled"), d.summary.started_at.replace('T', " "))
    } else {
        d.summary.title.clone()
    };
    let mut out = format!("{title}\n{} · {}\n\n", d.summary.started_at.replace('T', " "), d.summary.key);
    for b in &d.blocks {
        let spk = d.speakers.iter().find(|s| s.id == b.speaker_id);
        let name = match spk {
            Some(s) if s.name.is_some() => s.name.clone().unwrap(),
            Some(s) if s.track == "mic" => me.clone().unwrap_or_else(|| i18n::msg(lang, "me").to_string()),
            Some(s) => s.label.clone(),
            None => "?".into(),
        };
        let mark = if b.edited { format!(" ({})", i18n::msg(lang, "edited")) } else { String::new() };
        out.push_str(&format!("#{} [{}] {name}{mark}: {}\n", b.seq, fmt_time(b.t_start), b.text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_commands_parse() {
        let p = |args: &[&str]| Cli::try_parse_from(std::iter::once("rstt").chain(args.iter().copied())).map(|c| c.cmd);
        let Ok(Cmd::Record { what: RecordCmd::Start(a) }) =
            p(&["record", "start", "--library", "Empresa", "--client", "Cliente", "--title", "T", "--speakers", "2", "--mic", "off"])
        else {
            panic!("record start")
        };
        assert_eq!((a.library.as_deref(), a.client.as_deref(), a.expected_speakers, a.mic.as_deref()), (Some("Empresa"), Some("Cliente"), Some(2), Some("off")));
        assert!(matches!(p(&["record", "toggle"]), Ok(Cmd::Record { what: RecordCmd::Toggle(_) })));
        assert!(matches!(p(&["record", "stop"]), Ok(Cmd::Record { what: RecordCmd::Stop })));
        assert!(matches!(p(&["record", "devices"]), Ok(Cmd::Record { what: RecordCmd::Devices })));
        assert!(matches!(p(&["status"]), Ok(Cmd::Status)));
        assert!(matches!(p(&["bar", "show"]), Ok(Cmd::Bar { what: BarCmd::Show })));
        assert!(p(&["record", "start", "--client", "C"]).is_err(), "--client exige --library");
    }

    /// Roda um comando como `rstt <args>` e devolve o JSON e o aviso para a app aberta.
    fn run(app: &App, args: &[&str]) -> core_lib::Result<(Value, Option<Value>)> {
        let argv = std::iter::once("rstt").chain(args.iter().copied());
        let cli = Cli::try_parse_from(argv).unwrap();
        match exec(app, cli.cmd, Lang::EnUs, false)? {
            Output::Json(v, c) => Ok((v, c)),
            Output::Text(_) => panic!("texto inesperado"),
        }
    }

    fn setup() -> (tempfile::TempDir, App) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("call_2026-06-01_10-00-00_sintetica.txt"),
            "[00:01:00] Outros: o Gate Wei caiu\n[00:02:00] Eu: ok\n[00:03:00] Outros: o Gate Wei voltou\n",
        )
        .unwrap();
        let app = App::open(&tmp.path().join("data")).unwrap();
        run(&app, &["import", src.to_str().unwrap(), "--no-audio"]).unwrap();
        (tmp, app)
    }

    /// GUI de mentira no socket: guarda as requisições e responde como a GUI real (`Status` achatado).
    struct FakeGui {
        sock: PathBuf,
        seen: std::sync::mpsc::Receiver<Request>,
    }

    fn fake_gui(dir: &std::path::Path) -> FakeGui {
        use core_lib::recording::{Intent, RecordingInfo};
        use recorder::SessionStatus;
        let sock = dir.join("ipc.sock");
        let (tx, seen) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let recording = std::sync::Mutex::new(None::<String>);
        ipc::serve_on(&sock, move |req| {
            tx.lock().unwrap().send(req.clone()).unwrap();
            let mut rec = recording.lock().unwrap();
            let status = |title: Option<&String>, finalizing: Vec<String>| ipc::Status {
                state: if title.is_some() { "recording" } else { "idle" }.into(),
                app_running: true,
                recording: title.map(|t| RecordingInfo {
                    key: "call_2026-10-02_10-00-00".into(),
                    started_at: "2026-10-02T10:00:00".into(),
                    intent: Intent { library_id: 1, client_id: None, title: t.clone(), expected_speakers: None, language: None },
                    session: SessionStatus { elapsed_s: 65.4, mic: None, sys: None, cuts: 0 },
                }),
                finalizing,
                bar_visible: false,
                shortcut: Some("Ctrl+Alt+R".into()),
                shortcut_supported: true,
            };
            match req {
                Request::RecordStart(r) | Request::RecordToggle(r) if rec.is_none() => {
                    *rec = Some(r.meta.title.unwrap_or_default());
                    ipc::Response::ok(status(rec.as_ref(), vec![]))
                }
                Request::RecordStart(_) => ipc::Response::err("already_recording", "busy"),
                Request::RecordStop | Request::RecordToggle(_) if rec.is_some() => {
                    *rec = None;
                    ipc::Response::ok(status(None, vec!["call_2026-10-02_10-00-00".into()]))
                }
                Request::RecordStop | Request::RecordToggle(_) => ipc::Response::err("not_recording", "idle"),
                Request::Status => ipc::Response::ok(status(rec.as_ref(), vec![])),
                _ => ipc::Response::ok(Value::Null),
            }
        })
        .unwrap();
        FakeGui { sock, seen }
    }

    fn text(o: core_lib::Result<Output>) -> String {
        match o.unwrap() {
            Output::Text(s) => s,
            Output::Json(v, _) => panic!("esperava texto, veio {v}"),
        }
    }

    fn cmd(args: &[&str]) -> Cmd {
        Cli::try_parse_from(std::iter::once("rstt").chain(args.iter().copied())).unwrap().cmd
    }

    fn rec_cmd(args: &[&str]) -> RecordCmd {
        match cmd(&[&["record"], args].concat()) {
            Cmd::Record { what } => what,
            _ => panic!(),
        }
    }

    #[test]
    fn record_cli_text_json_and_requests() {
        let (tmp, app) = setup();
        run(&app, &["library", "add", "Empresa", tmp.path().join("Empresa").to_str().unwrap()]).unwrap();
        run(&app, &["client", "add", "--library", "Empresa", "Cliente"]).unwrap();
        let gui = fake_gui(tmp.path());
        let rec = |what: RecordCmd, lang: Lang, json: bool| exec_record(&app, what, lang, json, &gui.sock);

        // start: resolve nome → id, `--mic off`; saída de texto traduzida
        let out = text(rec(rec_cmd(&["start", "--library", "Empresa", "--client", "cliente", "--title", "Reunião", "--speakers", "3", "--mic", "off"]), Lang::PtBr, false));
        assert_eq!(out, "gravando: Reunião\n");
        let Request::RecordStart(req) = gui.seen.recv().unwrap() else { panic!() };
        assert_eq!((req.meta.library_id, req.meta.client_id, req.meta.expected_speakers), (Some(2), Some(1), Some(3)));
        assert_eq!((req.mic, req.sys), (StreamChoice::Off, StreamChoice::Default));

        // já gravando: o código do servidor volta como erro estável
        assert!(rec(rec_cmd(&["start"]), Lang::EnUs, false).is_err_and(|e| e.code() == "already_recording"));
        gui.seen.recv().unwrap();

        // status: texto e JSON (chaves estáveis)
        assert_eq!(text(exec_status(&app, Lang::EnUs, false, &gui.sock)), "recording 01:05 Reunião\n");
        let Output::Json(v, _) = exec_status(&app, Lang::EnUs, true, &gui.sock).unwrap() else { panic!() };
        assert_eq!((v["state"].as_str(), v["app_running"].as_bool(), v["recording"]["title"].as_str()), (Some("recording"), Some(true), Some("Reunião")));
        assert!(v["finalizing"].is_array() && v["shortcut_supported"] == true && v.get("ok").is_none());

        // stop: linha com a chave que ficou em segundo plano
        assert_eq!(text(rec(RecordCmd::Stop, Lang::EnUs, false)), "stopped: call_2026-10-02_10-00-00\n");
        assert!(rec(RecordCmd::Stop, Lang::EnUs, false).is_err_and(|e| e.code() == "not_recording"));

        // toggle sem argumentos herda o último alvo/dispositivos; sem título → "Chamada de <data>"
        core_rec::save_last_used(&app, &core_rec::LastUsed { library_id: Some(2), client_id: Some(1), mic: StreamChoice::Named("m".into()), sys: StreamChoice::Off }).unwrap();
        while gui.seen.try_recv().is_ok() {}
        assert_eq!(text(rec(rec_cmd(&["toggle"]), Lang::PtBr, false)), "gravando: Chamada de 2026-10-02 10:00:00\n");
        let Request::RecordToggle(req) = gui.seen.recv().unwrap() else { panic!() };
        assert_eq!((req.meta.library_id, req.meta.client_id, req.mic, req.sys), (Some(2), Some(1), StreamChoice::Named("m".into()), StreamChoice::Off));
        let Output::Json(v, _) = rec(rec_cmd(&["toggle"]), Lang::EnUs, true).unwrap() else { panic!() };
        assert_eq!((v["state"].as_str(), v["finalizing"][0].as_str()), (Some("idle"), Some("call_2026-10-02_10-00-00")));

        // barra
        assert_eq!(text(exec_bar(&app, BarCmd::Show, Lang::EnUs, false, &gui.sock)), "bar shown\n");
        let Output::Json(v, _) = exec_bar(&app, BarCmd::Hide, Lang::EnUs, true, &gui.sock).unwrap() else { panic!() };
        assert_eq!(v["state"], "idle");
    }

    /// `queue list` pergunta à app (fake) se há gravação: `recording`, `user` e os dois ao mesmo tempo.
    #[test]
    fn queue_list_reports_recording_and_user_pauses() {
        let (tmp, app) = setup();
        let gui = fake_gui(tmp.path());
        let list = || match exec_queue(&app, QueueCmd::List, Lang::EnUs, &gui.sock).unwrap() {
            Output::Json(v, _) => v,
            Output::Text(_) => panic!("texto inesperado"),
        };
        let view = |v: &Value| (v["paused"].clone(), v["paused_reasons"].clone());

        // ocioso: fila andando
        assert_eq!(view(&list()), (Value::Null, json!([])));
        // gravando: pausa distinta da do usuário
        exec_record(&app, rec_cmd(&["start", "--title", "T"]), Lang::EnUs, false, &gui.sock).unwrap();
        assert_eq!(view(&list()), (json!("recording"), json!(["recording"])));
        // os dois: `paused` é o que a GUI mostra (user) e `paused_reasons` não esconde a gravação
        app.set_setting(keys::QUEUE_PAUSED, Some("1")).unwrap();
        assert_eq!(view(&list()), (json!("user"), json!(["user", "recording"])));
        // gravação acabou: só a do usuário
        exec_record(&app, RecordCmd::Stop, Lang::EnUs, false, &gui.sock).unwrap();
        assert_eq!(view(&list()), (json!("user"), json!(["user"])));
        // `queue pause`/`resume` devolvem o mesmo formato
        let Output::Json(v, _) = exec_queue(&app, QueueCmd::Resume, Lang::EnUs, &gui.sock).unwrap() else { panic!() };
        assert_eq!(view(&v), (Value::Null, json!([])));
        // sem app aberta: nunca "recording"
        let none = tmp.path().join("nope.sock");
        let Output::Json(v, _) = exec_queue(&app, QueueCmd::List, Lang::EnUs, &none).unwrap() else { panic!() };
        assert_eq!(view(&v), (Value::Null, json!([])));
    }

    fn help_text(args: &[&str], lang: Lang) -> String {
        let argv = std::iter::once("rstt").chain(args.iter().copied()).map(std::ffi::OsString::from).collect();
        match parse_localized(argv, lang) {
            Err(e) if !e.use_stderr() => e.render().to_string(),
            _ => panic!("esperava a ajuda de {args:?}"),
        }
    }

    #[test]
    fn help_follows_the_language() {
        let want = [
            (Lang::PtBr, "Gravação e transcrição de reuniões, local", "Uso:", "Cria uma regra", "Diferencia maiúsculas de minúsculas"),
            (Lang::EnUs, "Local meeting recording and transcription", "Usage:", "Creates a rule", "Case-sensitive"),
            (Lang::Es419, "Grabación y transcripción de reuniones, local", "Uso:", "Crea una regla", "Distingue mayúsculas de minúsculas"),
        ];
        for (lang, about, usage, add, case) in want {
            let root = help_text(&["--help"], lang);
            assert!(root.contains(about) && root.contains(usage), "{}: {root}", lang.tag());
            let sub = help_text(&["glossary", "add", "--help"], lang);
            assert!(sub.contains(add) && sub.contains(case) && sub.contains(usage), "{}: {sub}", lang.tag());
            // as opções globais valem em todos os subcomandos, no mesmo idioma
            assert!(sub.contains(match lang { Lang::PtBr => "Diretório de dados", Lang::EnUs => "Data directory", Lang::Es419 => "Directorio de datos" }));
        }
        // o que não é pt-BR/en-US/es-419 cai em inglês (mesma `Lang::parse` do app e da CLI)
        for tag in ["de_DE.UTF-8", "C", "fr-FR", ""] {
            assert_eq!(Lang::parse(tag).unwrap_or(Lang::EnUs), Lang::EnUs, "{tag}");
        }
        assert_eq!(Lang::parse("pt_BR.UTF-8"), Some(Lang::PtBr));
        assert_eq!(Lang::parse("es_MX.UTF-8"), Some(Lang::Es419));
        // o comando continua o mesmo: dá para executar com a ajuda localizada
        let cli = parse_localized(["rstt", "queue", "list"].map(Into::into).to_vec(), Lang::Es419).unwrap();
        assert!(matches!(cli.cmd, Cmd::Queue { what: Some(QueueCmd::List) }));
    }

    #[test]
    fn lang_flag_is_read_before_help() {
        let args = |a: &[&str]| a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert_eq!(help_lang(&args(&["rstt", "--lang", "es-419", "--help"])), Lang::Es419);
        assert_eq!(help_lang(&args(&["rstt", "queue", "--lang=pt-BR", "--help"])), Lang::PtBr);
        assert_eq!(help_lang(&args(&["rstt", "--lang", "en", "glossary", "--help"])), Lang::EnUs);
        // depois de `--` já não é opção
        assert_eq!(help_lang(&args(&["rstt", "--", "--lang", "es"])), Lang::system());
        // idioma desconhecido em --lang: o do sistema
        assert_eq!(help_lang(&args(&["rstt", "--lang", "de", "--help"])), Lang::system());
    }

    #[test]
    fn record_cli_without_app() {
        let (tmp, app) = setup();
        let sock = tmp.path().join("nope.sock");
        // status não precisa da app: idle, exit 0
        assert_eq!(text(exec_status(&app, Lang::EnUs, false, &sock)), "idle\n");
        let Output::Json(v, _) = exec_status(&app, Lang::EnUs, true, &sock).unwrap() else { panic!() };
        assert_eq!((v["state"].as_str(), v["app_running"].as_bool()), (Some("idle"), Some(false)));
        assert!(v["recording"].is_null() && v["finalizing"].as_array().unwrap().is_empty());
        // stop e bar exigem a app
        assert!(exec_record(&app, RecordCmd::Stop, Lang::EnUs, false, &sock).is_err_and(|e| e.code() == "not_running"));
        assert!(exec_bar(&app, BarCmd::Show, Lang::EnUs, false, &sock).is_err_and(|e| e.code() == "not_running"));
    }

    #[test]
    fn transcription_commands_parse() {
        let p = |args: &[&str]| Cli::try_parse_from(std::iter::once("rstt").chain(args.iter().copied())).map(|c| c.cmd);
        let Ok(Cmd::Transcribe(a)) = p(&["transcribe", "call_2026-10-01_08-21-52", "--kind", "rediarize", "--language", "pt-BR", "--speakers", "2", "--no-bleed-filter"]) else {
            panic!("transcribe")
        };
        assert_eq!((a.call.as_deref(), a.kind.as_deref(), a.expected_speakers, a.no_bleed_filter, a.pending), (Some("call_2026-10-01_08-21-52"), Some("rediarize"), Some(2), true, false));
        let opts = job_options(&a).unwrap();
        assert_eq!((opts.language.as_deref(), opts.expected_speakers, opts.bleed_filter), (Some("pt"), Some(2), Some(false)));
        assert_eq!(parse_job_kind(a.kind.as_deref()).unwrap(), JobKind::Rediarize);
        assert!(matches!(p(&["transcribe", "--pending"]), Ok(Cmd::Transcribe(a)) if a.pending && a.call.is_none()));
        assert!(p(&["transcribe"]).is_err(), "sem chamada nem --pending");
        assert!(p(&["transcribe", "--pending", "call_x"]).is_err());
        assert!(p(&["transcribe", "--pending", "--kind", "full"]).is_err());
        assert!(p(&["transcribe", "call_x", "--kind", "tudo"]).is_err());
        assert!(job_options(&TranscribeArgs { language: Some("fr".into()), ..Default::default() }).is_err_and(|e| e.code() == "invalid"));
        assert!(job_options(&TranscribeArgs { expected_speakers: Some(0), ..Default::default() }).is_err_and(|e| e.code() == "invalid"));
        assert_eq!(job_options(&TranscribeArgs { language: Some("auto".into()), ..Default::default() }).unwrap().language.as_deref(), Some("auto"));
        assert!(matches!(p(&["queue"]), Ok(Cmd::Queue { what: None })));
        assert!(matches!(p(&["queue", "list"]), Ok(Cmd::Queue { what: Some(QueueCmd::List) })));
        assert!(matches!(p(&["queue", "pause"]), Ok(Cmd::Queue { what: Some(QueueCmd::Pause) })));
        assert!(matches!(p(&["queue", "resume"]), Ok(Cmd::Queue { what: Some(QueueCmd::Resume) })));
        assert!(matches!(p(&["queue", "cancel", "4"]), Ok(Cmd::Queue { what: Some(QueueCmd::Cancel { id: 4 }) })));
        assert!(matches!(p(&["queue", "retry", "4"]), Ok(Cmd::Queue { what: Some(QueueCmd::Retry { id: 4 }) })));
        assert!(p(&["queue", "cancel"]).is_err());
        assert!(matches!(p(&["setup", "status"]), Ok(Cmd::Setup { what: SetupCmd::Status })));
        assert!(matches!(p(&["setup", "install"]), Ok(Cmd::Setup { what: SetupCmd::Install })));
        assert!(p(&["setup"]).is_err());
    }

    /// Caminhos que não dependem do estado do núcleo em construção: nada aqui sobe a app nem toca no socket real.
    #[test]
    fn transcription_cli_without_jobs() {
        let (tmp, app) = setup();
        let call = "call_2026-06-01_10-00-00";
        // fila vazia e pausa do usuário (é o que a CLI grava; a app aplica no polling)
        let (v, notify) = run(&app, &["queue"]).unwrap();
        assert_eq!(v, json!({"paused": null, "paused_reasons": [], "jobs": []}));
        assert!(notify.is_none());
        let (v, notify) = run(&app, &["queue", "pause"]).unwrap();
        assert_eq!(v["paused"], "user");
        assert_eq!(notify.unwrap()["event"], "changed");
        assert_eq!(app.setting(keys::QUEUE_PAUSED).unwrap().as_deref(), Some("1"));
        assert_eq!(run(&app, &["queue", "list"]).unwrap().0["paused"], "user");
        assert_eq!(run(&app, &["queue", "resume"]).unwrap().0["paused"], Value::Null);
        assert_eq!(app.setting(keys::QUEUE_PAUSED).unwrap().as_deref(), Some("0"));
        assert!(run(&app, &["queue", "cancel", "999"]).is_err_and(|e| e.code() == "not_found"));

        // simulação: não cria tarefa, devolve o que seria enfileirado
        let (v, notify) = run(&app, &["transcribe", call, "--dry-run", "--kind", "resegment", "--speakers", "2", "--language", "es"]).unwrap();
        assert!(notify.is_none());
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["result"]["call_key"], call);
        assert_eq!(v["result"]["kind"], "resegment");
        assert_eq!(v["result"]["options"]["expected_speakers"], 2);
        assert_eq!(v["result"]["options"]["language"], "es");
        assert_eq!(run(&app, &["queue"]).unwrap().0["jobs"], json!([]));
        let (v, _) = run(&app, &["transcribe", "--pending", "--dry-run"]).unwrap();
        assert!(v["result"].is_array());
        assert!(run(&app, &["transcribe", "call_inexistente", "--dry-run"]).is_err_and(|e| e.code() == "not_found"));
        // chamada importada só com texto não tem áudio: nunca vira tarefa (e a app nem é acordada)
        let sock = tmp.path().join("nope.sock");
        let args = TranscribeArgs { call: Some(call.into()), ..Default::default() };
        let err = exec_transcribe(&app, args, Lang::EnUs, &sock).err().expect("sem áudio");
        assert!(["no_audio", "not_implemented"].contains(&err.code()), "{}", err.code());
    }

    #[test]
    fn setup_status_json_shape() {
        let (_tmp, app) = setup();
        let (v, _) = run(&app, &["setup", "status"]).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["fake_worker", "models", "runtime", "setup"], "o JSON de transcription_status sem a fila");
        assert_eq!(v["setup"], json!({"running": false, "phase": null}));
        let ids: Vec<_> = v["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["whisper", "segmentation", "embedding"]);
        assert!(v["runtime"]["state"].is_string() && v["runtime"]["runtime_version"].is_u64());
    }

    #[test]
    fn glossary_json_shapes_and_flow() {
        let (tmp, app) = setup();
        let call = "call_2026-06-01_10-00-00";
        let (v, notify) = run(&app, &["glossary", "add", "gate wei", "Gateway", "--global"]).unwrap();
        assert_eq!((v["scope"].as_str(), v["kind"].as_str(), v["replacement"].as_str(), v["overridden"].as_bool()), (Some("global"), Some("replace"), Some("Gateway"), Some(false)));
        assert_eq!(notify.unwrap()["event"], "glossary");
        let (v, _) = run(&app, &["glossary", "add", "Kubernetes", "--global"]).unwrap();
        assert_eq!(v["kind"], "term");
        assert!(run(&app, &["glossary", "add", "gate wei", "x", "--global"]).is_err_and(|e| e.code() == "conflict"));
        assert!(run(&app, &["glossary", "add", "a", "a", "--global"]).is_err_and(|e| e.code() == "invalid"));
        assert!(run(&app, &["glossary", "add", "sem escopo"]).is_err_and(|e| e.code() == "invalid"));

        let (v, _) = run(&app, &["glossary", "list", "--kind", "replace"]).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);

        // simulação: JSON do relatório, nada gravado, nenhum aviso
        let (v, notify) = run(&app, &["glossary", "apply", call, "--dry-run"]).unwrap();
        assert!(notify.is_none());
        assert_eq!((v["dry_run"].as_bool(), v["blocks_changed"].as_u64(), v["replacements"].as_u64()), (Some(true), Some(2), Some(2)));
        assert_eq!(v["changes"][0]["before"], "o Gate Wei caiu");
        assert_eq!(v["changes"][0]["after"], "o Gateway caiu");
        assert_eq!(v["changes"][0]["rules"][0]["count"], 1);
        assert!(v["batch_id"].is_null() && v["message"].is_string());

        let (v, notify) = run(&app, &["glossary", "apply", call]).unwrap();
        assert!(v["batch_id"].is_i64());
        assert_eq!(notify.unwrap()["event"], "changed");
        let (v, notify) = run(&app, &["glossary", "apply", call]).unwrap();
        assert!(notify.is_none() && v["message"].as_str().unwrap().contains("no glossary rule"));

        // um desfazer reverte o lote todo
        let (h, _) = run(&app, &["history", call]).unwrap();
        assert_eq!(h[0]["batch_kind"], "glossary");
        assert_eq!(h[0]["batch_size"], 2);
        let (u, _) = run(&app, &["undo", call]).unwrap();
        assert_eq!(u["batch_size"], 2);
        let (d, _) = run(&app, &["show", call]).unwrap();
        assert_eq!(d["blocks"][0]["text"], "o Gate Wei caiu");

        // edit block devolve o bloco + edit_id + suggestions
        let (v, _) = run(&app, &["edit", "block", call, "1", "o Gateway Service caiu"]).unwrap();
        assert!(v["id"].is_i64() && v["edit_id"].is_i64());
        assert_eq!(v["suggestions"].as_array().unwrap().len(), 0, "'gate wei' já tem regra");
        let (v, _) = run(&app, &["edit", "block", call, "3", "a Gate Wei voltou", "--dry-run"]).unwrap();
        assert_eq!(v["dry_run"], true);
        assert!(v["result"]["suggestions"].is_array());
        let (v, _) = run(&app, &["edit", "block", call, "3", "o Gate Wei retornou"]).unwrap();
        assert_eq!(v["suggestions"][0]["pattern"], "voltou");
        assert_eq!(v["suggestions"][0]["occurrences_in_call"], 0);
        assert_eq!(v["suggestions"][0]["client"], Value::Null);

        let (v, _) = run(&app, &["glossary", "suggest", "o Zenit Service", "o Zenith Service"]).unwrap();
        assert_eq!((v["suggestions"][0]["pattern"].as_str(), v["suggestions"][0]["replacement"].as_str()), (Some("Zenit"), Some("Zenith")));
        let (v, _) = run(&app, &["glossary", "suggest", "igual", "igual"]).unwrap();
        assert!(v["suggestions"].as_array().unwrap().is_empty() && v["message"].is_string());

        let (v, _) = run(&app, &["glossary", "terms"]).unwrap();
        assert_eq!(v["terms"][0], "Kubernetes");
        assert_eq!(v["budget_tokens"], 224);

        // remover (escopo explícito)
        assert!(run(&app, &["glossary", "remove", "1"]).is_err());
        let (v, _) = run(&app, &["glossary", "remove", "1", "--global"]).unwrap();
        assert_eq!(v["removed"]["pattern"], "gate wei");
        drop(tmp);
    }

    #[test]
    fn glossary_client_layer_and_import() {
        let (tmp, app) = setup();
        let company = tmp.path().join("Empresa");
        run(&app, &["library", "add", "Empresa", company.to_str().unwrap()]).unwrap();
        run(&app, &["client", "add", "--library", "Empresa", "Cliente"]).unwrap();
        run(&app, &["glossary", "add", "gate wei", "Global", "--global"]).unwrap();
        let (v, notify) = run(&app, &["glossary", "add", "gate wei", "Cliente X", "--library", "Empresa", "--client", "cliente", "--case-sensitive"]).unwrap();
        assert_eq!((v["scope"].as_str(), v["case_sensitive"].as_bool(), v["client_id"].as_i64()), (Some("client"), Some(true), Some(1)));
        assert_eq!(notify.unwrap()["library_id"], v["library_id"]);
        let (v, _) = run(&app, &["glossary", "list", "--library", "Empresa", "--client", "Cliente"]).unwrap();
        let view: Vec<_> = v.as_array().unwrap().iter().map(|r| (r["scope"].as_str().unwrap(), r["overridden"].as_bool().unwrap())).collect();
        assert_eq!(view, [("client", false), ("global", true)]);
        let (v, _) = run(&app, &["glossary", "list", "--library", "Empresa"]).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);

        let (v, _) = run(&app, &["glossary", "promote", "1", "--library", "Empresa"]).unwrap_or_else(|e| (json!({"err": e.code()}), None));
        assert_eq!(v["err"], "conflict", "global com outra substituição");

        let file = tmp.path().join("termos.txt");
        std::fs::write(&file, "# x\nKafka\nfoo -> bar\n").unwrap();
        let (v, notify) = run(&app, &["glossary", "import", file.to_str().unwrap(), "--dry-run", "--global"]).unwrap();
        assert!(notify.is_none());
        assert_eq!((v["dry_run"].as_bool(), v["added"].as_u64(), v["skipped"].as_u64(), v["invalid"].as_u64()), (Some(true), Some(2), Some(0), Some(0)));
        let (v, notify) = run(&app, &["glossary", "import", file.to_str().unwrap(), "--library", "Empresa", "--client", "Cliente"]).unwrap();
        assert_eq!(v["added"], 2);
        assert!(notify.is_some());
        let (v, _) = run(&app, &["glossary", "remove", "1", "--library", "Empresa"]).unwrap();
        assert_eq!(v["removed"]["scope"], "client");
    }
}
