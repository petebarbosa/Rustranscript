//! Janela Tauri. Os comandos são finos: abrem a biblioteca, chamam o núcleo, devolvem JSON.
use std::path::PathBuf;
use std::sync::Mutex;

use core_lib::import::{self, ImportOptions};
use core_lib::model::*;
use core_lib::rules::{ImportScope, RuleInput, RuleSource};
use core_lib::{App, ClientFilter, Library, Origin, paths, search, storage, transfer};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{Emitter, Manager, State};

use crate::i18n::Lang;
use crate::{ipc, player, recording, shell, shortcut, transcription, tray};

pub(crate) struct AppState {
    pub(crate) data_dir: PathBuf,
    pub(crate) app: Mutex<App>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CmdError {
    pub(crate) code: String,
    pub(crate) detail: String,
}

impl From<core_lib::Error> for CmdError {
    fn from(e: core_lib::Error) -> Self {
        CmdError { code: e.code().into(), detail: e.detail() }
    }
}

impl From<recorder::Error> for CmdError {
    fn from(e: recorder::Error) -> Self {
        core_lib::Error::from(e).into()
    }
}

pub(crate) type R<T> = Result<T, CmdError>;

pub(crate) fn with_app<T>(state: &State<AppState>, f: impl FnOnce(&App) -> core_lib::Result<T>) -> R<T> {
    let app = state.app.lock().unwrap_or_else(|e| e.into_inner());
    Ok(f(&app)?)
}

fn with_lib<T>(state: &State<AppState>, library_id: i64, f: impl FnOnce(&mut Library) -> core_lib::Result<T>) -> R<T> {
    with_app(state, |app| f(&mut app.open_library(library_id)?))
}

fn with_app_lib<T>(state: &State<AppState>, library_id: i64, f: impl FnOnce(&App, &mut Library) -> core_lib::Result<T>) -> R<T> {
    with_app(state, |app| f(app, &mut app.open_library(library_id)?))
}

/// Avisa a tela (como faz a CLI pelo socket) de que o glossário ou blocos mudaram.
fn changed(handle: &tauri::AppHandle, payload: Value) {
    let _ = handle.emit("data-changed", payload);
}

fn bad(detail: impl Into<String>) -> CmdError {
    CmdError { code: "invalid".into(), detail: detail.into() }
}

#[tauri::command(async)]
fn bootstrap(state: State<AppState>) -> R<Value> {
    with_app(&state, |app| {
        Ok(json!({
            "data_dir": app.data_dir,
            "system_language": Lang::system().tag(),
            "settings": app.settings()?,
            "inbox_id": app.inbox_id()?,
            "libraries": app.libraries()?,
        }))
    })
}

#[tauri::command(async)]
fn libraries(state: State<AppState>) -> R<Vec<LibraryInfo>> {
    with_app(&state, |app| app.libraries())
}

#[tauri::command(async)]
fn add_library(state: State<AppState>, name: String, path: String) -> R<i64> {
    with_app(&state, |app| Ok(app.add_library(&name, std::path::Path::new(&path))?.id))
}

#[tauri::command(async)]
fn rename_library(state: State<AppState>, library_id: i64, name: String) -> R<()> {
    with_app(&state, |app| app.rename_library(library_id, &name))
}

#[tauri::command(async)]
fn remove_library(state: State<AppState>, library_id: i64) -> R<()> {
    with_app(&state, |app| app.remove_library(library_id))
}

#[tauri::command(async)]
fn clients(state: State<AppState>, library_id: i64) -> R<Vec<ClientInfo>> {
    with_lib(&state, library_id, |l| l.clients())
}

#[tauri::command(async)]
fn add_client(state: State<AppState>, library_id: i64, name: String) -> R<ClientInfo> {
    with_lib(&state, library_id, |l| l.add_client(&name))
}

#[tauri::command(async)]
fn rename_client(state: State<AppState>, library_id: i64, client_id: i64, name: String) -> R<()> {
    with_lib(&state, library_id, |l| l.rename_client(client_id, &name))
}

/// `library_id` ausente = todas as bibliotecas; `unassigned` = só sem cliente.
#[tauri::command(async)]
fn calls(state: State<AppState>, library_id: Option<i64>, client_id: Option<i64>, unassigned: bool) -> R<Vec<CallSummary>> {
    with_app(&state, |app| {
        let rows = match library_id {
            Some(id) => vec![app.library_row(id)?],
            None => app.library_rows()?,
        };
        let mut out = Vec::new();
        for row in rows {
            if !row.is_inbox() && !row.root.join(core_lib::library::DB_FILE).is_file() {
                continue;
            }
            let lib = Library::open(row)?;
            let filter = match (client_id, unassigned) {
                (Some(c), _) => ClientFilter::Id(c),
                (None, true) => ClientFilter::Unassigned,
                (None, false) => ClientFilter::Any,
            };
            out.extend(lib.calls(filter)?);
        }
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(out)
    })
}

#[tauri::command(async)]
fn call_detail(state: State<AppState>, library_id: i64, call_id: i64, transcript_id: Option<i64>) -> R<CallDetail> {
    with_lib(&state, library_id, |l| l.call_detail(call_id, transcript_id))
}

#[tauri::command(async)]
fn set_block_text(state: State<AppState>, library_id: i64, block_id: i64, text: String) -> R<BlockEdit> {
    with_app_lib(&state, library_id, |app, l| app.edit_block_text(l, block_id, &text, Origin::Ui, false))
}

#[tauri::command(async)]
fn revert_block(state: State<AppState>, library_id: i64, block_id: i64) -> R<BlockInfo> {
    with_lib(&state, library_id, |l| l.revert_block(block_id, Origin::Ui, false))
}

/// Exclusão lógica de blocos (um lote só no histórico); `restore_blocks` é o "Desfazer" do aviso.
#[tauri::command(async)]
fn delete_blocks(state: State<AppState>, library_id: i64, block_ids: Vec<i64>) -> R<BlocksChange> {
    with_lib(&state, library_id, |l| l.delete_blocks(&block_ids, Origin::Ui, false))
}

#[tauri::command(async)]
fn restore_blocks(state: State<AppState>, library_id: i64, block_ids: Vec<i64>) -> R<BlocksChange> {
    with_lib(&state, library_id, |l| l.restore_blocks(&block_ids, Origin::Ui, false))
}

/// Cortes de áudio (#23). `preview_cuts` só calcula (quantos trechos sairiam) e não grava; `add_cuts` grava tudo em
/// um lote; `remove_cut` tira um corte e devolve os trechos que o corte salvo havia excluído.
#[tauri::command(async)]
fn preview_cuts(state: State<AppState>, library_id: i64, call_id: i64, spans: Vec<[f64; 2]>) -> R<CutsChange> {
    with_lib(&state, library_id, |l| l.add_cuts(call_id, &spans.iter().map(|s| (s[0], s[1])).collect::<Vec<_>>(), Origin::Ui, true))
}

#[tauri::command(async)]
fn add_cuts(state: State<AppState>, library_id: i64, call_id: i64, spans: Vec<[f64; 2]>) -> R<CutsChange> {
    with_lib(&state, library_id, |l| l.add_cuts(call_id, &spans.iter().map(|s| (s[0], s[1])).collect::<Vec<_>>(), Origin::Ui, false))
}

#[tauri::command(async)]
fn remove_cut(state: State<AppState>, library_id: i64, call_id: i64, cut_id: i64) -> R<CutsChange> {
    with_lib(&state, library_id, |l| l.remove_cut(call_id, cut_id, Origin::Ui, false))
}

#[tauri::command(async)]
fn set_title(state: State<AppState>, library_id: i64, call_id: i64, title: String) -> R<CallSummary> {
    with_lib(&state, library_id, |l| l.set_title(call_id, &title, Origin::Ui, false))
}

#[tauri::command(async)]
fn rename_speaker(state: State<AppState>, library_id: i64, speaker_id: i64, name: Option<String>) -> R<SpeakerInfo> {
    with_lib(&state, library_id, |l| l.rename_speaker(speaker_id, name.as_deref(), Origin::Ui, false))
}

#[tauri::command(async)]
fn set_block_speaker(state: State<AppState>, library_id: i64, block_id: i64, speaker_id: i64) -> R<BlockInfo> {
    with_lib(&state, library_id, |l| l.set_block_speaker(block_id, speaker_id, Origin::Ui, false))
}

#[tauri::command(async)]
fn set_active_transcript(state: State<AppState>, library_id: i64, call_id: i64, transcript_id: i64) -> R<()> {
    with_lib(&state, library_id, |l| l.set_active_transcript(call_id, transcript_id))
}

#[tauri::command(async)]
fn history(state: State<AppState>, library_id: i64, call_id: i64, limit: i64) -> R<Vec<HistoryEntry>> {
    with_lib(&state, library_id, |l| l.history(Some(call_id), limit))
}

#[tauri::command(async)]
fn undo(state: State<AppState>, library_id: i64, call_id: i64) -> R<Option<HistoryEntry>> {
    with_lib(&state, library_id, |l| l.undo(Some(call_id), false))
}

#[tauri::command(async)]
fn search_all(state: State<AppState>, query: String, limit: usize) -> R<Vec<SearchHit>> {
    with_app(&state, |app| search::search(app, &query, limit.min(200)))
}

#[tauri::command(async)]
fn assign(state: State<AppState>, library_id: i64, call_id: i64, to_library_id: i64, client_id: Option<i64>) -> R<Value> {
    with_app(&state, |app| {
        let (lib, id) = transfer::assign(app, library_id, call_id, to_library_id, client_id)?;
        Ok(json!({"library_id": lib, "call_id": id}))
    })
}

#[tauri::command(async)]
fn set_setting(handle: tauri::AppHandle, state: State<AppState>, key: String, value: Option<String>) -> R<()> {
    // `record_shortcut` não entra: tem comando próprio (`record_set_shortcut`) que valida e registra.
    const KEYS: &[&str] = &[
        "language",
        "me_name",
        "transcription_language",
        "last_library_id",
        "last_client_id",
        "record_library_id",
        "record_client_id",
        "record_mic",
        "record_sys",
        "record_bar_on_start",
    ];
    // fase 4: configurações de transcrição/diarização/vazamento (lista em `transcription::keys::ALL`)
    if !KEYS.contains(&key.as_str()) && !core_lib::transcription::keys::ALL.contains(&key.as_str()) {
        return Err(CmdError { code: "invalid".into(), detail: format!("unknown setting {key}") });
    }
    with_app(&state, |app| app.set_setting(&key, value.as_deref().filter(|v| !v.trim().is_empty())))?;
    // o menu do tray fala o idioma da configuração
    if key == "language" {
        tray::refresh(&handle);
    }
    Ok(())
}

#[tauri::command(async)]
fn reclaimable(state: State<AppState>) -> R<Value> {
    with_app(&state, |app| {
        let files = import::reclaimable(app)?;
        let total: u64 = files.iter().map(|f| f.size).sum();
        Ok(json!({"total_bytes": total, "files": files}))
    })
}

/// Chamadas que ainda têm áudio no disco, com o tamanho de cada uma (tela de armazenamento).
#[tauri::command(async)]
fn audio_list(state: State<AppState>) -> R<Value> {
    with_app(&state, |app| {
        let entries = storage::list_audio(app)?;
        let total: u64 = entries.iter().map(|e| e.bytes).sum();
        Ok(json!({"total_bytes": total, "calls": entries}))
    })
}

/// Apaga o áudio de uma chamada já transcrita (irreversível). `dry_run`: só diz o que seria apagado e o
/// tamanho (a confirmação da tela). Recusa se a chamada está sendo gravada ou convertida.
#[tauri::command(async)]
fn audio_delete(handle: tauri::AppHandle, state: State<AppState>, library_id: i64, call_id: i64, dry_run: bool) -> R<storage::AudioDeletion> {
    let st = recording::status(&handle);
    let busy: Vec<String> = st.recording.map(|r| r.key).into_iter().chain(st.finalizing).collect();
    let r = with_app(&state, |app| storage::delete_audio(app, library_id, call_id, &busy, dry_run))?;
    if !dry_run {
        changed(&handle, json!({"event": "changed", "library_id": library_id, "call_id": call_id}));
    }
    Ok(r)
}

/// Simulação (nada é gravado): o que a importação faria com essas pastas/arquivos.
#[tauri::command(async)]
fn import_preview(state: State<AppState>, paths: Vec<String>, library_id: Option<i64>) -> R<import::ImportReport> {
    with_app(&state, |app| {
        let cands = import::scan(&paths.iter().map(PathBuf::from).collect::<Vec<_>>())?;
        let opts = ImportOptions { library_id, dry_run: true, ..Default::default() };
        import::import(app, &cands, &opts, &mut |_| {})
    })
}

/// Importa em segundo plano; progresso no evento `import-progress`, fim em `import-done`.
#[tauri::command(async)]
fn import_start(
    handle: tauri::AppHandle,
    state: State<AppState>,
    paths: Vec<String>,
    library_id: Option<i64>,
    client_id: Option<i64>,
    convert_audio: bool,
) -> R<()> {
    let data_dir = state.data_dir.clone();
    std::thread::spawn(move || {
        let result = App::open(&data_dir).and_then(|app| {
            let cands = import::scan(&paths.iter().map(PathBuf::from).collect::<Vec<_>>())?;
            let opts = ImportOptions { library_id, client_id, convert_audio, dry_run: false };
            let mut last = (usize::MAX, u64::MAX);
            import::import(&app, &cands, &opts, &mut |p| {
                // limita a ~100 eventos por arquivo de áudio
                if let import::Progress::Audio { index, done, of, .. } = &p {
                    let pct = done * 100 / (*of).max(1);
                    if (*index, pct) == last {
                        return;
                    }
                    last = (*index, pct);
                }
                let _ = handle.emit("import-progress", &p);
            })
        });
        let payload = match result {
            Ok(r) => json!({"ok": true, "report": r}),
            Err(e) => json!({"ok": false, "error": CmdError::from(e)}),
        };
        let _ = handle.emit("import-done", payload);
        let _ = handle.emit("data-changed", json!({"event": "changed"}));
    });
    Ok(())
}

// ------------------------------------------------------------------ glossário

fn parse_scope(scope: &str) -> R<Scope> {
    match scope {
        "global" => Ok(Scope::Global),
        "client" => Ok(Scope::Client),
        other => Err(bad(format!("unknown scope {other}"))),
    }
}

fn parse_kind(kind: &str) -> R<RuleKind> {
    RuleKind::parse(kind).ok_or_else(|| bad(format!("unknown kind {kind}")))
}

fn need<T>(v: Option<T>, what: &str) -> R<T> {
    v.ok_or_else(|| bad(format!("{what} is required")))
}

/// Regras em vigor. Sem `library_id` ou sem `client_id`: só as globais. Com os dois: as do
/// cliente + as globais (estas com `overridden` quando o cliente tem a sua).
#[tauri::command(async)]
fn glossary_list(state: State<AppState>, library_id: Option<i64>, client_id: Option<i64>, kind: Option<String>) -> R<Vec<Rule>> {
    let kind = kind.as_deref().map(parse_kind).transpose()?;
    with_app(&state, |app| {
        let mut rules = match (library_id, client_id) {
            (Some(l), Some(c)) => app.merged_rules(&app.open_library(l)?, Some(c))?,
            _ => app.global_rules()?,
        };
        if let Some(k) = kind {
            rules.retain(|r| r.kind == k);
        }
        Ok(rules)
    })
}

/// `scope = "client"` exige `library_id` e `client_id`. `source_edit_id` (de uma sugestão) é
/// guardado na regra; numa regra global, `library_id` diz de qual biblioteca é essa edição.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn glossary_add(
    handle: tauri::AppHandle,
    state: State<AppState>,
    scope: String,
    library_id: Option<i64>,
    client_id: Option<i64>,
    kind: String,
    pattern: String,
    replacement: Option<String>,
    case_sensitive: bool,
    source_edit_id: Option<i64>,
) -> R<Rule> {
    let input = RuleInput { kind: parse_kind(&kind)?, pattern, replacement, case_sensitive };
    let rule = match parse_scope(&scope)? {
        Scope::Global => {
            let source = library_id.zip(source_edit_id).map(|(library_id, edit_id)| RuleSource { library_id, edit_id });
            with_app(&state, |app| app.add_global_rule(&input, source))?
        }
        Scope::Client => {
            let (library_id, client_id) = (need(library_id, "libraryId")?, need(client_id, "clientId")?);
            with_lib(&state, library_id, |l| l.add_client_rule(client_id, &input, source_edit_id))?
        }
    };
    changed(&handle, json!({"event": "glossary", "library_id": rule.library_id}));
    Ok(rule)
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn glossary_update(
    handle: tauri::AppHandle,
    state: State<AppState>,
    scope: String,
    library_id: Option<i64>,
    id: i64,
    kind: String,
    pattern: String,
    replacement: Option<String>,
    case_sensitive: bool,
) -> R<Rule> {
    let input = RuleInput { kind: parse_kind(&kind)?, pattern, replacement, case_sensitive };
    let rule = match parse_scope(&scope)? {
        Scope::Global => with_app(&state, |app| app.update_global_rule(id, &input))?,
        Scope::Client => with_lib(&state, need(library_id, "libraryId")?, |l| l.update_client_rule(id, &input))?,
    };
    changed(&handle, json!({"event": "glossary", "library_id": rule.library_id}));
    Ok(rule)
}

/// Devolve a regra removida (a UI pode oferecer "desfazer" recriando-a com `glossary_add`).
#[tauri::command(async)]
fn glossary_remove(handle: tauri::AppHandle, state: State<AppState>, scope: String, library_id: Option<i64>, id: i64) -> R<Rule> {
    let rule = match parse_scope(&scope)? {
        Scope::Global => with_app(&state, |app| app.remove_global_rule(id))?,
        Scope::Client => with_lib(&state, need(library_id, "libraryId")?, |l| l.remove_client_rule(id))?,
    };
    changed(&handle, json!({"event": "glossary", "library_id": rule.library_id}));
    Ok(rule)
}

/// Regra de cliente → global (a cópia do cliente some). Devolve a regra global.
#[tauri::command(async)]
fn glossary_promote(handle: tauri::AppHandle, state: State<AppState>, library_id: i64, id: i64) -> R<Rule> {
    let rule = with_app_lib(&state, library_id, |app, l| app.promote_rule(l, id))?;
    changed(&handle, json!({"event": "glossary", "library_id": library_id}));
    Ok(rule)
}

/// Aplica o glossário a uma chamada (versão ativa se `transcript_id` for nulo). Com `dry_run`
/// só devolve as mudanças previstas. Origem `ui`; tudo vira um lote desfeito por `undo`.
#[tauri::command(async)]
fn glossary_apply(
    handle: tauri::AppHandle,
    state: State<AppState>,
    library_id: i64,
    call_id: i64,
    transcript_id: Option<i64>,
    dry_run: bool,
) -> R<ApplyReport> {
    let report = with_app_lib(&state, library_id, |app, l| app.apply_glossary(l, call_id, transcript_id, Origin::Ui, dry_run))?;
    if !dry_run && report.blocks_changed > 0 {
        changed(&handle, json!({"event": "changed", "library_id": library_id, "call_id": call_id}));
    }
    Ok(report)
}

/// Importa um arquivo de termos. `scope = "client"` exige `library_id` e `client_id`.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn glossary_import_file(
    handle: tauri::AppHandle,
    state: State<AppState>,
    path: String,
    scope: String,
    library_id: Option<i64>,
    client_id: Option<i64>,
    kind: Option<String>,
    dry_run: bool,
) -> R<GlossaryImportReport> {
    let kind = kind.as_deref().map(parse_kind).transpose()?;
    let target = match parse_scope(&scope)? {
        Scope::Global => ImportScope::Global,
        Scope::Client => ImportScope::Client { library_id: need(library_id, "libraryId")?, client_id: need(client_id, "clientId")? },
    };
    let report = with_app(&state, |app| app.import_glossary_file(std::path::Path::new(&path), target, kind, dry_run))?;
    if !dry_run && report.added > 0 {
        changed(&handle, json!({"event": "glossary", "library_id": library_id}));
    }
    Ok(report)
}

/// Sugestões de regra para uma troca de texto (as mesmas que `set_block_text` devolve).
#[tauri::command(async)]
fn glossary_suggestions(state: State<AppState>, library_id: i64, block_id: i64, old_text: String, new_text: String) -> R<Vec<BlockSuggestion>> {
    with_app_lib(&state, library_id, |app, l| app.block_edit_suggestions(l, block_id, &old_text, &new_text))
}

/// Termos que iriam para o prompt do modelo (cliente primeiro), dentro do orçamento de tokens.
#[tauri::command(async)]
fn glossary_prompt_terms(state: State<AppState>, library_id: i64, client_id: Option<i64>) -> R<Value> {
    with_app_lib(&state, library_id, |app, l| {
        let terms = app.prompt_terms(l, client_id)?;
        let tokens: usize = terms.iter().map(|t| core_lib::glossary::estimate_tokens(t) + 1).sum();
        Ok(json!({"terms": terms, "estimated_tokens": tokens, "budget_tokens": core_lib::glossary::PROMPT_TOKEN_BUDGET}))
    })
}

/// Migração pendente com a app antiga aberta: a GUI não tem stderr visível, então mostra a mensagem num diálogo
/// nativo e sai (código 1) quando ele fecha, sem abrir o banco nem criar o diretório de dados novo.
fn legacy_running_dialog(context: tauri::Context<tauri::Wry>, message: String) -> ! {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    eprintln!("rstt: {message}");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            app.dialog().message(message).title("Rustranscript").kind(MessageDialogKind::Error).show(|_| std::process::exit(1));
            Ok(())
        })
        .build(context)
        .expect("error while building tauri application");
    app.run(|_, _| {});
    std::process::exit(1)
}

pub fn run(data_dir: Option<PathBuf>) {
    // o contexto (assets embutidos) é gerado uma vez só, para o app de verdade ou para o diálogo de migração
    let context = tauri::generate_context!();
    if let Err(blocked) = paths::migrate_legacy(data_dir.as_deref()) {
        legacy_running_dialog(context, crate::i18n::legacy_running_message(Lang::system(), &blocked));
    }
    let data_dir = paths::resolve_data_dir(data_dir.as_deref());
    let app = match App::open(&data_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}: {}", e.code(), e.detail());
            std::process::exit(1);
        }
    };
    let data_dir = app.data_dir.clone();
    let app = tauri::Builder::default()
        // precisa ser o primeiro plugin: uma segunda abertura só traz a janela existente para frente
        // (ou a barra, se estiver gravando — ver `shell::on_second_instance`)
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| shell::on_second_instance(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(shortcut::plugin())
        .manage(AppState { data_dir: data_dir.clone(), app: Mutex::new(app) })
        .manage(recording::RecState::new())
        .manage(player::PlayerState::new())
        .manage(transcription::TxState::new())
        .on_window_event(shell::on_window_event)
        .setup(move |app| {
            let handle = app.handle().clone();
            let h = handle.clone();
            if let Err(e) = ipc::listen(&data_dir, move |req| recording::handle_request(&h, req)) {
                eprintln!("ipc: {e}");
            }
            if let Err(e) = tray::setup(&handle) {
                eprintln!("tray: {e}");
            }
            recording::startup(&handle);
            transcription::startup(&handle);
            // a janela principal nasce invisível (`tauri.conf.json`): só aparece se a app não foi
            // aberta pela CLI/atalho para gravar (`ipc::spawn_gui`)
            if !shell::started_hidden()
                && let Some(w) = app.get_webview_window("main")
            {
                let _ = w.show();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            libraries,
            add_library,
            rename_library,
            remove_library,
            clients,
            add_client,
            rename_client,
            calls,
            call_detail,
            set_block_text,
            revert_block,
            delete_blocks,
            restore_blocks,
            preview_cuts,
            add_cuts,
            remove_cut,
            set_title,
            rename_speaker,
            set_block_speaker,
            set_active_transcript,
            history,
            undo,
            search_all,
            assign,
            set_setting,
            reclaimable,
            audio_list,
            audio_delete,
            import_preview,
            import_start,
            glossary_list,
            glossary_add,
            glossary_update,
            glossary_remove,
            glossary_promote,
            glossary_apply,
            glossary_import_file,
            glossary_suggestions,
            glossary_prompt_terms,
            player::player_open,
            player::player_peaks,
            player::player_play,
            player::player_pause,
            player::player_seek,
            player::player_speed,
            player::player_close,
            player::player_set_cuts,
            recording::record_info,
            recording::record_devices,
            recording::record_status,
            recording::record_start,
            recording::record_update,
            recording::record_stop,
            recording::record_toggle,
            recording::record_monitor_start,
            recording::record_monitor_stop,
            recording::record_orphans,
            recording::record_recover,
            recording::record_discard,
            recording::record_set_shortcut,
            recording::bar_show,
            recording::bar_hide,
            recording::show_main_window,
            transcription::transcription_status,
            transcription::transcription_setup_start,
            transcription::transcription_setup_cancel,
            transcription::models_import_local,
            transcription::transcribe_enqueue,
            transcription::transcribe_pending,
            transcription::queue_status,
            transcription::queue_cancel,
            transcription::queue_retry,
            transcription::queue_pause,
            transcription::bleed_removals,
        ])
        .build(context)
        .expect("error while building tauri application");
    app.run(|handle, event| {
        shell::on_run_event(handle, &event);
        transcription::on_run_event(handle, &event);
    });
}
