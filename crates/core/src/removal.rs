//! Apagar de verdade: uma chamada (`Library::delete_call`), um cliente ou uma empresa/projeto inteira.
//! Diferente de `App::remove_library` (só descadastra; pasta e chamadas ficam no disco).
//!
//! Com chamadas, quem apaga escolhe (`DeleteMode`): `Keep` as preserva (empresa → Não classificadas;
//! cliente → "Sem cliente" da mesma empresa) e `Delete` apaga as chamadas, o áudio e todas as linhas.
//!
//! Ordem e queda no meio: primeiro o conteúdo, a linha da entidade por último. Se o processo cair, sobra
//! uma entidade menor e repetir o comando termina o serviço. Tudo que pode recusar (tarefa aberta,
//! gravação em curso, chamada igual na inbox, pasta de destino ocupada, pasta de chamada fora da raiz)
//! é checado ANTES de mexer em qualquer coisa; a CLI e a tela usam `dry_run` para montar a pergunta.
//!
//! Esquema (ver `schema.rs`): `calls` leva em cascata `import_sources`, `transcripts` (e deles `blocks` e
//! `bleed_removals`), `speakers`, `chapters`, `edit_history` e `audio_cuts`. O que NÃO cascateia e é
//! apagado aqui à mão: o bruto da transcrição (`tx_*`, só com `job_id`, sem FK) e `transcription_jobs` do
//! `app.db`. Como `calls`, `clients`, `libraries` e `transcription_jobs` não usam AUTOINCREMENT, os ids
//! são reaproveitados: restos de tarefa/bruto/configuração apontando para um id apagado passariam a valer
//! para outra coisa. Por isso nada disso pode sobrar.
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::library::{DB_FILE, Library, UNASSIGNED_DIR};
use crate::transcription::staging;
use crate::{Error, Result, fsx, recording, storage, transfer};

/// O que fazer com as chamadas da empresa/cliente apagado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    /// Preserva as chamadas: empresa → Não classificadas; cliente → sem cliente, na mesma empresa.
    Keep,
    /// Apaga as chamadas, o áudio e todas as linhas. Irreversível.
    Delete,
}

/// Resultado de `delete_library`/`delete_client`. Em `dry_run` traz só as contagens (nada muda).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Deletion {
    pub dry_run: bool,
    pub library_id: i64,
    /// Preenchido em `delete_client`.
    pub client_id: Option<i64>,
    pub name: String,
    pub mode: Option<DeleteMode>,
    /// Chamadas da entidade (em empresa, todas; em cliente, as dele).
    pub calls: i64,
    /// Áudio e caches no disco dessas chamadas (a mesma conta de `audio list`).
    pub audio_bytes: u64,
    /// Clientes que vão junto (só em empresa; a inbox não tem clientes).
    pub clients: i64,
    /// Entradas de glossário de cliente que vão junto.
    pub glossary_entries: i64,
    /// `dry_run`: por que `Keep` não é possível agora (chamada igual na inbox, pasta de destino ocupada).
    /// `None` = possível.
    pub keep_blocked: Option<String>,
    /// Chamadas preservadas / apagadas (execução de verdade).
    pub moved: i64,
    pub deleted: i64,
    /// Só em `delete_library`: a pasta da raiz foi removida (só se ficou vazia).
    pub folder_removed: bool,
    /// O que ficou no disco e não é do app (arquivos de uma pasta adotada): entradas que sobraram na raiz
    /// da empresa (ou a pasta do cliente), mais `library.db*` que não puderam ser apagados.
    pub leftover: Vec<String>,
}

// -------------------------------------------------------------------------------- uma chamada

impl Library {
    /// Apaga uma chamada por inteiro: a pasta dela (áudio incluído), o bruto da transcrição, as tarefas
    /// do `app.db` e todas as linhas (versões, blocos, falantes, edições, cortes) e o índice de busca.
    /// Recusa (`conflict`) com tarefa de transcrição aberta; a pasta tem de estar dentro da raiz.
    ///
    /// Ordem: pasta, bruto, tarefas, linha da chamada. Cair no meio deixa a chamada (sem áudio) visível e
    /// repetir termina; o contrário deixaria arquivos sem dono ou tarefa apontando para um id reaproveitado.
    pub fn delete_call(&mut self, app: &App, call_id: i64) -> Result<()> {
        let (key, dir): (String, Option<String>) = self
            .conn
            .query_row("SELECT key, dir FROM calls WHERE id = ?1", [call_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or_else(|| Error::not_found(format!("call {}:{call_id}", self.id())))?;
        check_no_open_job(app, self.id(), call_id, &key)?;
        let folder = call_folder(self, call_id, &key, dir.as_deref())?;

        // 1. pasta da chamada (só a dela)
        if let Some(f) = &folder {
            match std::fs::remove_dir_all(f) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }

        // 2. bruto da transcrição: das tarefas da chamada (qualquer estado) e das versões que o apontam
        let mut jobs: Vec<i64> = {
            let mut st = app.db.prepare("SELECT id FROM transcription_jobs WHERE library_id = ?1 AND call_id = ?2")?;
            st.query_map(params![self.id(), call_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        {
            let mut st = self.conn.prepare("SELECT raw_job_id FROM transcripts WHERE call_id = ?1 AND raw_job_id IS NOT NULL")?;
            jobs.extend(st.query_map([call_id], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        jobs.sort_unstable();
        jobs.dedup();
        for job in jobs {
            staging::delete_rows(&self.conn, job)?;
        }

        // 3. tarefas do app.db e a origem (edição) das regras globais criadas a partir desta chamada
        app.db.execute("DELETE FROM transcription_jobs WHERE library_id = ?1 AND call_id = ?2", params![self.id(), call_id])?;
        let edits: Vec<i64> = {
            let mut st = self.conn.prepare("SELECT id FROM edit_history WHERE call_id = ?1")?;
            st.query_map([call_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        for edit in edits {
            app.db.execute(
                "UPDATE glossary_global SET source_edit_id = NULL, source_library_id = NULL WHERE source_library_id = ?1 AND source_edit_id = ?2",
                params![self.id(), edit],
            )?;
        }

        // 4. linhas: os blocos primeiro e de forma explícita (os gatilhos do FTS tiram o texto da busca), depois
        // a chamada, que leva o resto em cascata
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM blocks WHERE transcript_id IN (SELECT id FROM transcripts WHERE call_id = ?1)", [call_id])?;
        tx.execute("DELETE FROM calls WHERE id = ?1", [call_id])?;
        tx.commit()?;

        if let Some(rel) = dir.as_deref().filter(|d| !d.is_empty()) {
            let abs = self.root().join(rel);
            if let Some(parent) = abs.parent() {
                fsx::prune_empty_dirs(parent, self.root());
            }
        }
        Ok(())
    }
}

fn check_no_open_job(app: &App, library_id: i64, call_id: i64, key: &str) -> Result<()> {
    let open: bool = app.db.query_row(
        "SELECT EXISTS (SELECT 1 FROM transcription_jobs WHERE library_id = ?1 AND call_id = ?2 AND state IN ('queued', 'running'))",
        params![library_id, call_id],
        |r| r.get(0),
    )?;
    if open { Err(Error::Conflict(format!("call {key} has a transcription job queued or running"))) } else { Ok(()) }
}

/// A pasta que `delete_call` vai remover, validada: `None` = nada a remover (sem pasta, ou já não existe).
/// Recusa (`invalid`) se não for uma pasta comum estritamente dentro da raiz (caminho canônico: links
/// simbólicos e `..` não escapam), se o nome não for o da chamada (`<chave>` ou `<chave>_<slug>`) ou se
/// outra chamada usa a mesma pasta ou uma dentro/acima dela.
fn call_folder(lib: &Library, call_id: i64, key: &str, dir: Option<&str>) -> Result<Option<PathBuf>> {
    let Some(abs) = storage::call_dir(lib, dir)? else { return Ok(None) };
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(Error::invalid(format!("call folder is not a plain directory: {}", abs.display())));
    }
    let root = lib.root().canonicalize()?;
    let real = abs.canonicalize()?;
    if real == root || !real.starts_with(&root) {
        return Err(Error::invalid(format!("call folder is outside the library: {}", abs.display())));
    }
    if !real.file_name().is_some_and(|n| n.to_string_lossy().starts_with(key)) {
        return Err(Error::invalid(format!("call folder does not belong to call {key}: {}", abs.display())));
    }
    let rel = dir.unwrap_or_default();
    let shared: i64 = lib.conn.query_row(
        "SELECT count(*) FROM calls WHERE id <> ?1 AND dir IS NOT NULL AND dir <> ''
           AND (dir = ?2 OR substr(dir, 1, length(?2) + 1) = ?2 || '/' OR substr(?2, 1, length(dir) + 1) = dir || '/')",
        params![call_id, rel],
        |r| r.get(0),
    )?;
    if shared > 0 {
        return Err(Error::invalid(format!("call folder is shared with another call: {}", abs.display())));
    }
    Ok(Some(real))
}

// ---------------------------------------------------------------------------- planejamento

struct Plan {
    calls: Vec<i64>,
    audio_bytes: u64,
    keep_blocked: Option<String>,
}

/// Tudo que pode recusar, sem mexer em nada. `inbox`: para onde `Keep` levaria as chamadas de uma empresa
/// (`None` = o cliente, que as deixa "sem cliente" na própria empresa).
fn plan(app: &App, lib: &Library, call_ids: &[i64], inbox: Option<&Library>) -> Result<Plan> {
    let mut calls = Vec::new();
    let mut audio_bytes = 0u64;
    let mut keep_blocked = None;
    for &id in call_ids {
        let (key, slug, dir): (String, String, Option<String>) =
            lib.conn.query_row("SELECT key, slug, dir FROM calls WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        check_no_open_job(app, lib.id(), id, &key)?;
        if let Some(folder) = call_folder(lib, id, &key, dir.as_deref())? {
            audio_bytes += storage::removable(&folder)?.iter().map(|f| f.bytes).sum::<u64>();
        }
        // `Keep`: o destino não pode ter a chamada nem a pasta (`assign`/`set_client` recusariam no meio do caminho)
        if keep_blocked.is_none() {
            keep_blocked = match inbox {
                Some(inbox) => keep_blocker(inbox, &key, &slug, None)?,
                None => keep_blocker(lib, &key, &slug, Some(dir.as_deref()))?,
            };
        }
        calls.push(id);
    }
    Ok(Plan { calls, audio_bytes, keep_blocked })
}

/// `same_library`: `Some(pasta atual)` quando a chamada fica na mesma biblioteca (cliente); `None` quando vai
/// para outra (inbox), onde a chave também não pode existir.
fn keep_blocker(dst: &Library, key: &str, slug: &str, same_library: Option<Option<&str>>) -> Result<Option<String>> {
    if same_library.is_none() && dst.call_id_by_key(key)?.is_some() {
        return Ok(Some(format!("call {key} already exists in the inbox")));
    }
    let new_dir = dst.call_dir_for(key, slug, None)?;
    let old_dir = same_library.flatten().map(Path::new);
    if old_dir != Some(new_dir.as_path()) && dst.root().join(&new_dir).exists() {
        return Ok(Some(format!("destination folder already exists: {}", dst.root().join(&new_dir).display())));
    }
    Ok(None)
}

fn require_mode(mode: Option<DeleteMode>, calls: i64) -> Result<()> {
    if calls > 0 && mode.is_none() {
        return Err(Error::invalid(format!("{calls} call(s) would be affected: choose keep (keep the calls) or delete (delete the calls and their audio)")));
    }
    Ok(())
}

/// Apaga a empresa/projeto aberta? Não pode haver gravação em curso (ou sendo finalizada) para ela/ele.
fn check_not_recording(app: &App, library_id: i64, client_id: Option<i64>) -> Result<()> {
    let busy = recording::busy_targets(app)?
        .into_iter()
        .any(|i| i.library_id == library_id && (client_id.is_none() || i.client_id == client_id));
    if busy { Err(Error::Conflict("a recording to this destination is in progress or being finalized".into())) } else { Ok(()) }
}

/// Configurações em `app.db` que apontam para a biblioteca/cliente. Os ids são reaproveitados, então
/// o apontamento não pode sobrar.
fn clear_settings(app: &App, library_id: i64, client_id: Option<i64>) -> Result<()> {
    let id_of = |key: &str| -> Result<Option<i64>> { Ok(app.setting(key)?.and_then(|v| v.trim().parse().ok())) };
    for (lib_key, client_key) in [(recording::keys::LIBRARY, recording::keys::CLIENT), ("last_library_id", "last_client_id")] {
        if id_of(lib_key)? != Some(library_id) {
            continue;
        }
        match client_id {
            None => {
                app.set_setting(lib_key, None)?;
                app.set_setting(client_key, None)?;
            }
            Some(c) if id_of(client_key)? == Some(c) => app.set_setting(client_key, None)?,
            Some(_) => {}
        }
    }
    Ok(())
}

/// Apaga diretórios vazios de baixo para cima dentro de `dir` (e `dir`, se ficar vazio). Nunca apaga
/// arquivo, nunca segue link simbólico. Devolve se `dir` foi removido.
fn prune_tree(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            prune_tree(&entry.path());
        }
    }
    std::fs::remove_dir(dir).is_ok()
}

fn open_company(app: &App, library_id: i64) -> Result<Library> {
    let row = app.library_row(library_id)?;
    if row.is_inbox() {
        return Err(Error::invalid("the inbox (unclassified calls) cannot be deleted"));
    }
    // `Library::open` criaria a pasta e um `library.db` vazio: com a pasta ausente (disco desmontado) não abre
    if !row.root.join(DB_FILE).is_file() {
        return Err(Error::Conflict(format!(
            "library {library_id} is unavailable (folder or library.db missing); unregister it with `library remove`"
        )));
    }
    Library::open(row)
}

// ------------------------------------------------------------------------------------ cliente

/// Apaga um cliente. `mode` é obrigatório se ele tem chamadas: `Keep` as deixa na mesma empresa, sem
/// cliente (`Library::set_client`, que move as pastas); `Delete` as apaga com o áudio. O glossário do
/// cliente e a linha do cliente saem por último. `dry_run`: só as contagens, com as mesmas recusas.
pub fn delete_client(app: &App, library_id: i64, client_id: i64, mode: Option<DeleteMode>, dry_run: bool) -> Result<Deletion> {
    let mut lib = open_company(app, library_id)?;
    let client = lib
        .clients()?
        .into_iter()
        .find(|c| c.id == client_id)
        .ok_or_else(|| Error::not_found(format!("client {client_id}")))?;
    let call_ids: Vec<i64> = {
        let mut st = lib.conn.prepare("SELECT id FROM calls WHERE client_id = ?1 ORDER BY started_at")?;
        st.query_map([client_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let glossary_entries: i64 = lib.conn.query_row("SELECT count(*) FROM glossary_client WHERE client_id = ?1", [client_id], |r| r.get(0))?;
    check_not_recording(app, library_id, Some(client_id))?;
    let plan = plan(app, &lib, &call_ids, None)?;
    let mut out = Deletion {
        dry_run,
        library_id,
        client_id: Some(client_id),
        name: client.name.clone(),
        mode,
        calls: plan.calls.len() as i64,
        audio_bytes: plan.audio_bytes,
        glossary_entries,
        keep_blocked: plan.keep_blocked.clone(),
        ..Default::default()
    };
    if dry_run {
        return Ok(out);
    }
    require_mode(mode, out.calls)?;
    if mode == Some(DeleteMode::Keep)
        && let Some(why) = plan.keep_blocked
    {
        return Err(Error::Conflict(why));
    }

    // 1. as chamadas
    for &c in &plan.calls {
        match mode {
            Some(DeleteMode::Keep) => {
                lib.set_client(c, None)?;
                out.moved += 1;
            }
            _ => {
                lib.delete_call(app, c)?;
                out.deleted += 1;
            }
        }
    }

    // 2. glossário do cliente e o cliente, por último. Uma chamada que entrou no meio (gravação terminando)
    // desfaz tudo: o cliente continua e repetir o comando recomeça de onde parou.
    check_not_recording(app, library_id, Some(client_id))?;
    let tx = lib.conn.transaction()?;
    let left: i64 = tx.query_row("SELECT count(*) FROM calls WHERE client_id = ?1", [client_id], |r| r.get(0))?;
    if left > 0 {
        return Err(Error::Conflict(format!("client {client_id} got new calls while being deleted; run it again")));
    }
    tx.execute("DELETE FROM glossary_client WHERE client_id = ?1", [client_id])?;
    tx.execute("DELETE FROM clients WHERE id = ?1", [client_id])?;
    tx.commit()?;
    clear_settings(app, library_id, Some(client_id))?;

    // 3. a pasta do cliente, se ficou vazia (arquivos que não são do app ficam e entram em `leftover`)
    let folder = lib.root().join(&client.slug);
    if is_single_component(&client.slug) && folder.is_dir() && !prune_tree(&folder) {
        out.leftover.push(folder.display().to_string());
    }
    Ok(out)
}

fn is_single_component(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!((parts.next(), parts.next()), (Some(std::path::Component::Normal(_)), None))
}

// ------------------------------------------------------------------------------------ empresa

/// Apaga uma empresa/projeto (a inbox nunca: `invalid`). `Keep` leva todas as chamadas para Não
/// classificadas (`transfer::assign`; os clientes e seus glossários somem, a inbox não tem clientes);
/// `Delete` as apaga com o áudio. Depois: descadastra, apaga `library.db` (+ `-wal`/`-shm`) e remove as pastas
/// que ficaram vazias, a raiz só se ficar vazia. NUNCA apaga a raiz em bloco: a pasta pode ter sido adotada
/// (`library add`) e conter arquivos que o app não criou; o que sobra é devolvido em `leftover`.
pub fn delete_library(app: &App, library_id: i64, mode: Option<DeleteMode>, dry_run: bool) -> Result<Deletion> {
    let mut lib = open_company(app, library_id)?;
    let inbox = app.open_library(app.inbox_id()?)?;
    let call_ids: Vec<i64> = {
        let mut st = lib.conn.prepare("SELECT id FROM calls ORDER BY started_at")?;
        st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let clients = lib.clients()?;
    let glossary_entries: i64 =
        lib.conn.query_row("SELECT (SELECT count(*) FROM glossary_client) + (SELECT count(*) FROM glossary_library)", [], |r| r.get(0))?;
    check_not_recording(app, library_id, None)?;
    let plan = plan(app, &lib, &call_ids, Some(&inbox))?;
    let mut out = Deletion {
        dry_run,
        library_id,
        name: lib.row.name.clone(),
        mode,
        calls: plan.calls.len() as i64,
        audio_bytes: plan.audio_bytes,
        clients: clients.len() as i64,
        glossary_entries,
        keep_blocked: plan.keep_blocked.clone(),
        ..Default::default()
    };
    if dry_run {
        return Ok(out);
    }
    require_mode(mode, out.calls)?;
    if mode == Some(DeleteMode::Keep)
        && let Some(why) = plan.keep_blocked
    {
        return Err(Error::Conflict(why));
    }
    let (root, inbox_id) = (lib.root().to_path_buf(), inbox.id());
    drop(inbox);

    // 1. as chamadas
    for &c in &plan.calls {
        match mode {
            Some(DeleteMode::Keep) => {
                transfer::assign(app, library_id, c, inbox_id, None)?;
                out.moved += 1;
            }
            _ => {
                lib.delete_call(app, c)?;
                out.deleted += 1;
            }
        }
    }
    let left: i64 = lib.conn.query_row("SELECT count(*) FROM calls", [], |r| r.get(0))?;
    if left > 0 {
        return Err(Error::Conflict(format!("library {library_id} got new calls while being deleted; run it again")));
    }

    // 2. o que aponta para a biblioteca fora dela (ids são reaproveitados): configurações, origem das regras
    // globais, tarefas que sobraram
    clear_settings(app, library_id, None)?;
    app.db.execute("UPDATE glossary_global SET source_edit_id = NULL, source_library_id = NULL WHERE source_library_id = ?1", [library_id])?;
    app.db.execute("DELETE FROM transcription_jobs WHERE library_id = ?1", [library_id])?;

    // 3. descadastra e solta a conexão antes de apagar o banco
    check_not_recording(app, library_id, None)?;
    app.remove_library(library_id)?;
    drop(lib);
    for name in [DB_FILE.to_string(), format!("{DB_FILE}-wal"), format!("{DB_FILE}-shm")] {
        let path = root.join(&name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => out.leftover.push(path.display().to_string()),
        }
    }

    // 4. pastas vazias: só as do app (pasta de cada cliente e `_unassigned`); nada de varrer a raiz inteira.
    // A raiz sai só se ficou vazia.
    let slugs = clients.iter().map(|c| c.slug.as_str()).chain(std::iter::once(UNASSIGNED_DIR));
    for slug in slugs.filter(|s| is_single_component(s)) {
        let dir = root.join(slug);
        if dir.is_dir() {
            prune_tree(&dir);
        }
    }
    out.folder_removed = std::fs::remove_dir(&root).is_ok();
    if !out.folder_removed
        && let Ok(entries) = std::fs::read_dir(&root)
    {
        let mut rest: Vec<String> = entries.flatten().map(|e| e.path().display().to_string()).collect();
        rest.sort();
        rest.retain(|p| !out.leftover.contains(p));
        out.leftover.extend(rest);
    }
    Ok(out)
}
