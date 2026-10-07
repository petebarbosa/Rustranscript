//! Apagar cliente e empresa/projeto (e a chamada, a primitiva): `keep` x `delete`, recusas antes de mexer
//! em qualquer coisa, raiz adotada com arquivo de fora, dry-run. Dados sintéticos, pasta de dados isolada.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use core_lib::model::LibraryInfo;
use core_lib::recording::{self, Meta, StartRequest};
use core_lib::removal::{self, DeleteMode};
use core_lib::rules::RuleInput;
use core_lib::{App, Library, search};
use recorder::FakeBackend;
use rusqlite::params;

struct Env {
    tmp: tempfile::TempDir,
    app: App,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::open(&tmp.path().join("data")).unwrap();
    Env { tmp, app }
}

struct Company {
    id: i64,
    root: PathBuf,
}

impl Env {
    fn company(&self, name: &str) -> Company {
        let root = self.tmp.path().join(name);
        let row = self.app.add_library(name, &root).unwrap();
        Company { id: row.id, root: row.root }
    }

    fn lib(&self, id: i64) -> Library {
        self.app.open_library(id).unwrap()
    }

    fn inbox(&self) -> Library {
        self.lib(self.app.inbox_id().unwrap())
    }

    fn jobs(&self, lib: i64) -> i64 {
        self.app.db.query_row("SELECT count(*) FROM transcription_jobs WHERE library_id = ?1", [lib], |r| r.get(0)).unwrap()
    }

    fn registered(&self) -> Vec<LibraryInfo> {
        self.app.libraries().unwrap()
    }

    /// Chamada completa: pasta com `mic.flac` (1000 bytes) e `sys.flac` (3000), versão ativa com um bloco com
    /// `text`, falante, edição no histórico, bruto da transcrição (`tx_segments`) e uma tarefa `done` no `app.db`.
    fn call(&self, lib_id: i64, key: &str, client: Option<i64>, text: &str) -> i64 {
        let lib = self.lib(lib_id);
        let rel = lib.call_dir_for(key, "", client).unwrap();
        let dir = lib.root().join(&rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mic.flac"), vec![1u8; 1000]).unwrap();
        std::fs::write(dir.join("sys.flac"), vec![2u8; 3000]).unwrap();
        let rel_s = rel.to_string_lossy().into_owned();
        lib.conn
            .execute(
                "INSERT INTO calls (key, client_id, started_at, created_at, dir, mic_path, sys_path, title)
                 VALUES (?1, ?2, '2026-01-01T10:00:00', 't', ?3, ?4, ?5, ?6)",
                params![key, client, rel_s, format!("{rel_s}/mic.flac"), format!("{rel_s}/sys.flac"), format!("titulo {key}")],
            )
            .unwrap();
        let call = lib.conn.last_insert_rowid();
        self.app
            .db
            .execute(
                "INSERT INTO transcription_jobs (library_id, call_id, call_key, kind, state, created_at) VALUES (?1, ?2, ?3, 'full', 'done', 't')",
                params![lib_id, call, key],
            )
            .unwrap();
        let job = self.app.db.last_insert_rowid();
        lib.conn
            .execute("INSERT INTO transcripts (call_id, version, created_at, is_active, raw_job_id) VALUES (?1, 1, 't', 1, ?2)", params![call, job])
            .unwrap();
        let transcript = lib.conn.last_insert_rowid();
        lib.conn.execute("INSERT INTO speakers (call_id, track, label) VALUES (?1, 'sys', 'Pessoa 1')", [call]).unwrap();
        let speaker = lib.conn.last_insert_rowid();
        lib.conn
            .execute(
                "INSERT INTO blocks (transcript_id, seq, t_start, t_end, speaker_id, original_text, text) VALUES (?1, 1, 0, 1, ?2, ?3, ?3)",
                params![transcript, speaker, text],
            )
            .unwrap();
        lib.conn
            .execute("INSERT INTO edit_history (call_id, entity, entity_id, new_value, origin, at) VALUES (?1, 'call_title', ?1, 'x', 'ui', 't')", [call])
            .unwrap();
        lib.conn
            .execute("INSERT INTO tx_segments (job_id, track, seq, t_start, t_end, text) VALUES (?1, 'sys', 0, 0, 1, ?2)", params![job, text])
            .unwrap();
        call
    }

    /// Chamada com tarefa de transcrição em aberto (substitui a tarefa `done` de `call`).
    fn open_job(&self, lib_id: i64, call: i64) {
        self.app.db.execute("DELETE FROM transcription_jobs WHERE library_id = ?1 AND call_id = ?2", params![lib_id, call]).unwrap();
        self.app
            .db
            .execute(
                "INSERT INTO transcription_jobs (library_id, call_id, call_key, kind, state, created_at) VALUES (?1, ?2, 'k', 'full', 'queued', 't')",
                params![lib_id, call],
            )
            .unwrap();
    }
}

fn count(lib: &Library, table: &str) -> i64 {
    lib.conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

fn fts(lib: &Library, term: &str) -> i64 {
    lib.conn.query_row("SELECT count(*) FROM blocks_fts WHERE blocks_fts MATCH ?1", [term], |r| r.get(0)).unwrap()
}

/// O índice FTS bate com a tabela de conteúdo (sem entradas órfãs), nos dois índices.
fn fts_consistent(lib: &Library) {
    lib.conn.execute("INSERT INTO blocks_fts(blocks_fts) VALUES ('integrity-check')", []).unwrap();
    lib.conn.execute("INSERT INTO calls_fts(calls_fts) VALUES ('integrity-check')", []).unwrap();
}

fn flac(lib: &Library, key: &str) -> Option<PathBuf> {
    let id = lib.call_id_by_key(key).unwrap()?;
    let (dir, mic): (String, Option<String>) = lib.conn.query_row("SELECT dir, mic_path FROM calls WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(mic.as_deref(), Some(format!("{dir}/mic.flac").as_str()), "mic_path acompanha a pasta");
    Some(lib.root().join(mic.unwrap()))
}

const MODE_KEEP: Option<DeleteMode> = Some(DeleteMode::Keep);
const MODE_DELETE: Option<DeleteMode> = Some(DeleteMode::Delete);

// ------------------------------------------------------------------------------------ cliente

#[test]
fn client_keep_leaves_the_calls_without_client_and_moves_the_folder() {
    let e = env();
    let c = e.company("Empresa");
    let lib = e.lib(c.id);
    let (x, y) = (lib.add_client("Cliente X").unwrap(), lib.add_client("Cliente Y").unwrap());
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    e.call(c.id, "call_2026-01-02_10-00-00", Some(x.id), "bravo");
    e.call(c.id, "call_2026-01-03_10-00-00", Some(y.id), "charlie");
    lib.add_client_rule(x.id, &RuleInput::term("zenit"), None).unwrap();
    lib.add_client_rule(y.id, &RuleInput::term("outro"), None).unwrap();
    drop(lib);

    let r = removal::delete_client(&e.app, c.id, x.id, MODE_KEEP, false).unwrap();
    assert_eq!((r.calls, r.moved, r.deleted, r.glossary_entries), (2, 2, 0, 1));

    let lib = e.lib(c.id);
    assert_eq!(lib.clients().unwrap().len(), 1, "só o Y sobra");
    assert_eq!(count(&lib, "calls"), 3);
    assert_eq!(count(&lib, "glossary_client"), 1, "o glossário do X foi, o do Y fica");
    for key in ["call_2026-01-01_10-00-00", "call_2026-01-02_10-00-00"] {
        let id = lib.call_id_by_key(key).unwrap().unwrap();
        assert_eq!(lib.call_summary(id).unwrap().client_id, None);
        let audio = flac(&lib, key).unwrap();
        assert!(audio.starts_with(c.root.join("_unassigned")) && audio.is_file(), "{audio:?}");
        assert_eq!(std::fs::read(&audio).unwrap().len(), 1000);
    }
    assert!(!c.root.join("cliente-x").exists(), "pasta do cliente apagada (ficou vazia)");
    assert!(c.root.join("cliente-y").is_dir());
    assert_eq!(fts(&lib, "alfa"), 1, "texto continua na busca");
    // as tarefas das chamadas mantidas não são tocadas
    assert_eq!(e.jobs(c.id), 3);
}

#[test]
fn client_delete_removes_calls_audio_rows_search_and_jobs() {
    let e = env();
    let c = e.company("Empresa");
    let lib = e.lib(c.id);
    let (x, y) = (lib.add_client("Cliente X").unwrap(), lib.add_client("Cliente Y").unwrap());
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    e.call(c.id, "call_2026-01-02_10-00-00", Some(x.id), "bravo");
    e.call(c.id, "call_2026-01-03_10-00-00", Some(y.id), "charlie");
    lib.add_client_rule(x.id, &RuleInput::term("zenit"), None).unwrap();
    drop(lib);
    assert_eq!(search::search(&e.app, "alfa", 10).unwrap().len(), 1);

    let r = removal::delete_client(&e.app, c.id, x.id, MODE_DELETE, false).unwrap();
    assert_eq!((r.calls, r.moved, r.deleted), (2, 0, 2));

    let lib = e.lib(c.id);
    assert_eq!(count(&lib, "calls"), 1);
    // nenhuma linha órfã das chamadas apagadas (só sobra a chamada do Y)
    for table in ["transcripts", "blocks", "speakers", "edit_history", "tx_segments"] {
        assert_eq!(count(&lib, table), 1, "{table}");
    }
    assert_eq!(count(&lib, "glossary_client"), 0);
    assert_eq!(lib.clients().unwrap().len(), 1);
    assert_eq!(e.jobs(c.id), 1, "tarefas das chamadas apagadas sumiram");
    // busca: o texto apagado não volta, nem pelo índice, nem pela busca global
    assert_eq!((fts(&lib, "alfa"), fts(&lib, "bravo"), fts(&lib, "charlie")), (0, 0, 1));
    assert!(search::search(&e.app, "alfa", 10).unwrap().is_empty());
    assert!(search::search(&e.app, "titulo", 10).unwrap().len() == 1, "título das apagadas também saiu");
    fts_consistent(&lib);
    // disco
    assert!(!c.root.join("cliente-x").exists());
    assert!(c.root.join("cliente-y/call_2026-01-03_10-00-00/mic.flac").is_file());
}

#[test]
fn deleted_call_ids_are_reused_without_inheriting_old_jobs_or_raw_data() {
    let e = env();
    let c = e.company("Empresa");
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    let id = e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    removal::delete_client(&e.app, c.id, x.id, MODE_DELETE, false).unwrap();
    // `calls.id` não é AUTOINCREMENT: o id volta (e o do job também)
    let again = e.call(c.id, "call_2026-02-01_10-00-00", None, "bravo");
    assert_eq!(again, id);
    assert_eq!(e.jobs(c.id), 1, "só a tarefa da chamada nova");
    let lib = e.lib(c.id);
    assert_eq!(count(&lib, "tx_segments"), 1);
    assert_eq!(lib.conn.query_row("SELECT text FROM tx_segments", [], |r| r.get::<_, String>(0)).unwrap(), "bravo");
}

#[test]
fn client_with_no_calls_needs_no_mode() {
    let e = env();
    let c = e.company("Empresa");
    let lib = e.lib(c.id);
    let x = lib.add_client("Cliente X").unwrap();
    lib.add_client_rule(x.id, &RuleInput::term("zenit"), None).unwrap();
    drop(lib);
    let r = removal::delete_client(&e.app, c.id, x.id, None, false).unwrap();
    assert_eq!((r.calls, r.glossary_entries), (0, 1));
    let lib = e.lib(c.id);
    assert_eq!((lib.clients().unwrap().len(), count(&lib, "glossary_client")), (0, 0));
}

#[test]
fn calls_require_an_explicit_mode_and_nothing_changes_without_it() {
    let e = env();
    let c = e.company("Empresa");
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    let err = removal::delete_client(&e.app, c.id, x.id, None, false).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(err.to_string().contains("keep") && err.to_string().contains("delete"), "{err}");
    let err = removal::delete_library(&e.app, c.id, None, false).unwrap_err();
    assert_eq!(err.code(), "invalid");
    let lib = e.lib(c.id);
    assert_eq!((count(&lib, "calls"), lib.clients().unwrap().len()), (1, 1));
    assert_eq!(e.registered().len(), 2);
}

// ------------------------------------------------------------------------------------ empresa

#[test]
fn company_keep_moves_every_call_to_the_inbox_and_removes_the_library() {
    let e = env();
    let c = e.company("Empresa");
    let lib = e.lib(c.id);
    let x = lib.add_client("Cliente X").unwrap();
    lib.add_client_rule(x.id, &RuleInput::term("zenit"), None).unwrap();
    drop(lib);
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    e.call(c.id, "call_2026-01-02_10-00-00", None, "bravo");
    let inbox_id = e.app.inbox_id().unwrap();

    let r = removal::delete_library(&e.app, c.id, MODE_KEEP, false).unwrap();
    assert_eq!((r.calls, r.moved, r.deleted, r.clients, r.glossary_entries), (2, 2, 0, 1, 1));
    assert!(r.folder_removed && r.leftover.is_empty(), "{r:?}");

    let inbox = e.inbox();
    assert_eq!(count(&inbox, "calls"), 2);
    for key in ["call_2026-01-01_10-00-00", "call_2026-01-02_10-00-00"] {
        let audio = flac(&inbox, key).unwrap();
        assert!(audio.is_file() && audio.starts_with(inbox.root()), "{audio:?}");
    }
    assert_eq!((fts(&inbox, "alfa"), fts(&inbox, "bravo")), (1, 1));
    assert_eq!(search::search(&e.app, "alfa", 10).unwrap().len(), 1);
    assert!(e.registered().iter().all(|l| l.id == inbox_id), "só a inbox sobra");
    assert!(!c.root.exists(), "raiz vazia saiu");
    assert_eq!(e.jobs(c.id), 0);
    assert_eq!(e.jobs(inbox_id), 0, "as tarefas terminadas ficam para trás com a origem (o `assign` as apaga)");
}

#[test]
fn company_delete_removes_everything_and_clears_what_points_at_it() {
    let e = env();
    let c = e.company("Empresa");
    let other = e.company("Outra");
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    e.call(c.id, "call_2026-01-02_10-00-00", None, "bravo");
    e.call(other.id, "call_2026-01-03_10-00-00", None, "charlie");
    // "último destino" apontando para a empresa e o cliente; e uma regra global nascida de uma edição dela
    for (k, v) in [("record_library_id", c.id), ("record_client_id", x.id), ("last_library_id", c.id), ("last_client_id", x.id)] {
        e.app.set_setting(k, Some(&v.to_string())).unwrap();
    }
    e.app
        .db
        .execute("INSERT INTO glossary_global (kind, pattern, created_at, source_edit_id, source_library_id) VALUES ('term', 'zenit', 't', 5, ?1)", [c.id])
        .unwrap();

    let r = removal::delete_library(&e.app, c.id, MODE_DELETE, false).unwrap();
    assert_eq!((r.calls, r.moved, r.deleted, r.audio_bytes), (2, 0, 2, 8000));
    assert!(r.folder_removed);

    assert_eq!(count(&e.inbox(), "calls"), 0, "nada foi para a inbox");
    assert!(!c.root.exists());
    assert!(e.registered().iter().all(|l| l.id != c.id));
    assert_eq!(e.jobs(c.id), 0);
    for k in ["record_library_id", "record_client_id", "last_library_id", "last_client_id"] {
        assert_eq!(e.app.setting(k).unwrap(), None, "{k}");
    }
    let (src_edit, src_lib): (Option<i64>, Option<i64>) =
        e.app.db.query_row("SELECT source_edit_id, source_library_id FROM glossary_global", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((src_edit, src_lib), (None, None));
    // a outra empresa não foi tocada
    let o = e.lib(other.id);
    assert_eq!((count(&o, "calls"), fts(&o, "charlie"), e.jobs(other.id)), (1, 1, 1));
    assert!(search::search(&e.app, "alfa", 10).unwrap().is_empty());
}

#[test]
fn settings_of_other_libraries_are_left_alone() {
    let e = env();
    let (c, other) = (e.company("Empresa"), e.company("Outra"));
    let x = e.lib(other.id).add_client("Cliente X").unwrap();
    e.app.set_setting("record_library_id", Some(&other.id.to_string())).unwrap();
    e.app.set_setting("record_client_id", Some(&x.id.to_string())).unwrap();
    removal::delete_library(&e.app, c.id, None, false).unwrap();
    assert_eq!(e.app.setting("record_library_id").unwrap(), Some(other.id.to_string()));
    assert_eq!(e.app.setting("record_client_id").unwrap(), Some(x.id.to_string()));
}

#[test]
fn inbox_cannot_be_deleted() {
    let e = env();
    let inbox = e.app.inbox_id().unwrap();
    e.call(inbox, "call_2026-01-01_10-00-00", None, "alfa");
    for mode in [None, MODE_KEEP, MODE_DELETE] {
        for dry in [true, false] {
            let err = removal::delete_library(&e.app, inbox, mode, dry).unwrap_err();
            assert_eq!(err.code(), "invalid", "{mode:?} {dry}");
        }
    }
    assert_eq!(removal::delete_client(&e.app, inbox, 1, None, false).unwrap_err().code(), "invalid");
    assert_eq!(count(&e.inbox(), "calls"), 1);
    assert!(e.inbox().root().join("call_2026-01-01_10-00-00/mic.flac").is_file());
}

#[test]
fn adopted_root_keeps_foreign_files_and_stays() {
    let e = env();
    let root = e.tmp.path().join("Adotada");
    std::fs::create_dir_all(root.join("documentos")).unwrap();
    std::fs::write(root.join("documentos/contrato.txt"), "nao e do app").unwrap();
    std::fs::write(root.join("leia-me.txt"), "nem este").unwrap();
    let c = e.app.add_library("Adotada", &root).unwrap();
    let root = c.root.clone();
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    e.call(c.id, "call_2026-01-02_10-00-00", None, "bravo");
    // arquivo de fora até dentro da pasta de um cliente do app
    std::fs::write(root.join("cliente-x/anotacao.txt"), "de fora").unwrap();

    let r = removal::delete_library(&e.app, c.id, MODE_DELETE, false).unwrap();
    assert!(!r.folder_removed);
    assert_eq!(std::fs::read_to_string(root.join("leia-me.txt")).unwrap(), "nem este");
    assert_eq!(std::fs::read_to_string(root.join("documentos/contrato.txt")).unwrap(), "nao e do app");
    assert_eq!(std::fs::read_to_string(root.join("cliente-x/anotacao.txt")).unwrap(), "de fora");
    assert!(!root.join("library.db").exists() && !root.join("library.db-wal").exists() && !root.join("library.db-shm").exists());
    assert!(!root.join("cliente-x/call_2026-01-01_10-00-00").exists(), "a pasta da chamada saiu");
    assert!(!root.join("_unassigned").exists(), "pasta do app que ficou vazia saiu");
    let left: Vec<_> = r.leftover.iter().map(|p| Path::new(p).strip_prefix(&root).unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["cliente-x", "documentos", "leia-me.txt"]);
    assert!(e.registered().iter().all(|l| l.id != c.id));
}

#[test]
fn an_empty_folder_of_the_user_is_foreign_content_too_and_keeps_the_root() {
    let e = env();
    let c = e.company("Empresa");
    e.call(c.id, "call_2026-01-01_10-00-00", None, "alfa");
    // pasta vazia do usuário na raiz: não é do app, a raiz fica
    std::fs::create_dir_all(c.root.join("vazia")).unwrap();
    let r = removal::delete_library(&e.app, c.id, MODE_DELETE, false).unwrap();
    assert!(!r.folder_removed && c.root.join("vazia").is_dir());
}

// ----------------------------------------------------------------------------------- recusas

fn snapshot(e: &Env, c: &Company) -> (i64, i64, i64, usize, bool) {
    let lib = e.lib(c.id);
    (count(&lib, "calls"), count(&lib, "tx_segments"), e.jobs(c.id), lib.clients().unwrap().len(), c.root.join("cliente-x/call_2026-01-01_10-00-00/mic.flac").is_file())
}

#[test]
fn open_job_on_one_call_refuses_everything_and_changes_nothing() {
    let e = env();
    let c = e.company("Empresa");
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    let busy = e.call(c.id, "call_2026-01-02_10-00-00", Some(x.id), "bravo");
    e.call(c.id, "call_2026-01-03_10-00-00", None, "charlie");
    e.open_job(c.id, busy);
    let before = snapshot(&e, &c);

    for mode in [MODE_KEEP, MODE_DELETE] {
        for dry in [false, true] {
            assert_eq!(removal::delete_client(&e.app, c.id, x.id, mode, dry).unwrap_err().code(), "conflict", "client {mode:?} {dry}");
            assert_eq!(removal::delete_library(&e.app, c.id, mode, dry).unwrap_err().code(), "conflict", "library {mode:?} {dry}");
        }
    }
    assert_eq!(snapshot(&e, &c), before);
    assert_eq!(e.registered().len(), 2);
    let inbox = e.inbox();
    assert_eq!(count(&inbox, "calls"), 0, "nenhuma chamada foi para a inbox");
    // a primitiva também recusa
    let mut lib = e.lib(c.id);
    assert_eq!(lib.delete_call(&e.app, busy).unwrap_err().code(), "conflict");
    assert_eq!(count(&lib, "calls"), 3);
}

#[test]
fn keep_is_refused_when_the_inbox_already_has_the_call_and_dry_run_says_so() {
    let e = env();
    let c = e.company("Empresa");
    e.call(c.id, "call_2026-01-01_10-00-00", None, "alfa");
    let inbox_id = e.app.inbox_id().unwrap();
    e.call(inbox_id, "call_2026-01-01_10-00-00", None, "outra com a mesma chave");
    let before = (count(&e.lib(c.id), "calls"), count(&e.inbox(), "calls"));

    let dry = removal::delete_library(&e.app, c.id, None, true).unwrap();
    assert!(dry.keep_blocked.as_deref().unwrap().contains("already exists"), "{dry:?}");
    assert_eq!(removal::delete_library(&e.app, c.id, MODE_KEEP, false).unwrap_err().code(), "conflict");
    assert_eq!((count(&e.lib(c.id), "calls"), count(&e.inbox(), "calls")), before);
    assert_eq!(e.registered().len(), 2);
    // `delete` não tem esse problema
    removal::delete_library(&e.app, c.id, MODE_DELETE, false).unwrap();
    assert_eq!(count(&e.inbox(), "calls"), 1, "a da inbox continua");
}

#[test]
fn client_keep_is_refused_when_the_unassigned_folder_is_taken() {
    let e = env();
    let c = e.company("Empresa");
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    std::fs::create_dir_all(c.root.join("_unassigned/call_2026-01-01_10-00-00")).unwrap();
    assert!(removal::delete_client(&e.app, c.id, x.id, None, true).unwrap().keep_blocked.is_some());
    assert_eq!(removal::delete_client(&e.app, c.id, x.id, MODE_KEEP, false).unwrap_err().code(), "conflict");
    assert_eq!(e.lib(c.id).clients().unwrap().len(), 1);
}

#[test]
fn a_recording_to_the_library_or_client_blocks_the_deletion() {
    let e = env();
    let c = e.company("Empresa");
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    let y = e.lib(c.id).add_client("Cliente Y").unwrap();
    let rec = recording::start(
        &e.app,
        Arc::new(FakeBackend::new()),
        StartRequest { meta: Meta { library_id: Some(c.id), client_id: Some(x.id), ..Meta::default() }, ..Default::default() },
    )
    .unwrap();
    assert_eq!(removal::delete_client(&e.app, c.id, x.id, None, false).unwrap_err().code(), "conflict");
    assert_eq!(removal::delete_library(&e.app, c.id, None, false).unwrap_err().code(), "conflict");
    // outro cliente da mesma empresa não é afetado
    removal::delete_client(&e.app, c.id, y.id, None, false).unwrap();
    assert_eq!(e.lib(c.id).clients().unwrap().len(), 1);
    // depois de parar (e finalizar), a trava cai
    let stopped = rec.stop().unwrap();
    drop(stopped);
    removal::delete_client(&e.app, c.id, x.id, None, false).unwrap();
}

#[test]
fn unavailable_library_is_refused_without_recreating_its_folder() {
    let e = env();
    let c = e.company("Empresa");
    std::fs::remove_dir_all(&c.root).unwrap(); // pendrive desmontado
    assert_eq!(removal::delete_library(&e.app, c.id, None, false).unwrap_err().code(), "conflict");
    assert!(!c.root.exists());
    assert!(e.registered().iter().any(|l| l.id == c.id), "continua cadastrada (use `library remove`)");
}

// ------------------------------------------------------------------------------------ dry-run

#[test]
fn dry_run_returns_the_counts_and_changes_nothing() {
    let e = env();
    let c = e.company("Empresa");
    let lib = e.lib(c.id);
    let (x, y) = (lib.add_client("Cliente X").unwrap(), lib.add_client("Cliente Y").unwrap());
    lib.add_client_rule(x.id, &RuleInput::term("zenit"), None).unwrap();
    lib.add_client_rule(x.id, &RuleInput::term("polar"), None).unwrap();
    lib.add_client_rule(y.id, &RuleInput::term("outro"), None).unwrap();
    drop(lib);
    e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    e.call(c.id, "call_2026-01-02_10-00-00", Some(y.id), "bravo");
    e.call(c.id, "call_2026-01-03_10-00-00", Some(y.id), "charlie");
    // uma chamada já sem áudio não conta bytes
    e.lib(c.id).conn.execute("UPDATE calls SET audio_deleted_at = 't' WHERE key = 'call_2026-01-03_10-00-00'", []).unwrap();
    std::fs::remove_file(c.root.join("cliente-y/call_2026-01-03_10-00-00/mic.flac")).unwrap();
    std::fs::remove_file(c.root.join("cliente-y/call_2026-01-03_10-00-00/sys.flac")).unwrap();
    let before = snapshot(&e, &c);

    let r = removal::delete_client(&e.app, c.id, x.id, None, true).unwrap();
    assert!(r.dry_run);
    assert_eq!((r.calls, r.audio_bytes, r.clients, r.glossary_entries, r.moved, r.deleted), (1, 4000, 0, 2, 0, 0));
    assert_eq!((r.name.as_str(), r.client_id, r.keep_blocked.as_deref()), ("Cliente X", Some(x.id), None));
    let r = removal::delete_client(&e.app, c.id, y.id, MODE_DELETE, true).unwrap();
    assert_eq!((r.calls, r.audio_bytes, r.glossary_entries), (2, 4000, 1));

    let r = removal::delete_library(&e.app, c.id, None, true).unwrap();
    assert!(r.dry_run);
    assert_eq!((r.calls, r.audio_bytes, r.clients, r.glossary_entries, r.moved, r.deleted, r.folder_removed), (3, 8000, 2, 3, 0, 0, false));
    assert_eq!(r.name, "Empresa");
    assert!(r.keep_blocked.is_none());

    assert_eq!(snapshot(&e, &c), before);
    assert_eq!(e.registered().len(), 2);
    assert!(c.root.join("library.db").is_file());
    assert_eq!(count(&e.inbox(), "calls"), 0);
}

// ------------------------------------------------------------------------------ a primitiva

#[test]
fn delete_call_refuses_folders_outside_the_library() {
    let e = env();
    let c = e.company("Empresa");
    let outside = e.tmp.path().join("fora");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precioso.txt"), "x").unwrap();
    let mut lib = e.lib(c.id);
    fn ins(lib: &Library, key: &str, dir: &str) -> i64 {
        lib.conn.execute("INSERT INTO calls (key, started_at, created_at, dir) VALUES (?1, 't', 't', ?2)", params![key, dir]).unwrap();
        lib.conn.last_insert_rowid()
    }
    // `..` e caminho absoluto
    let a = ins(&lib, "call_a", "../fora");
    let b = ins(&lib, "call_b", outside.to_str().unwrap());
    // link simbólico dentro da raiz apontando para fora
    std::os::unix::fs::symlink(&outside, c.root.join("call_c")).unwrap();
    let l = ins(&lib, "call_c", "call_c");
    // a própria raiz / pasta que não é da chamada
    let r = ins(&lib, "call_d", ".");
    std::fs::create_dir_all(c.root.join("cliente")).unwrap();
    let n = ins(&lib, "call_e", "cliente");
    for id in [a, b, l, r, n] {
        assert_eq!(lib.delete_call(&e.app, id).unwrap_err().code(), "invalid", "call {id}");
    }
    assert_eq!(count(&lib, "calls"), 5, "nenhuma linha saiu");
    assert!(outside.join("precioso.txt").is_file() && c.root.join("cliente").is_dir() && c.root.join("library.db").is_file());
    // dois registros na mesma pasta: nenhum dos dois pode apagá-la
    std::fs::create_dir_all(c.root.join("call_f")).unwrap();
    let f1 = ins(&lib, "call_f", "call_f");
    ins(&lib, "call_f2", "call_f");
    assert_eq!(lib.delete_call(&e.app, f1).unwrap_err().code(), "invalid");
    assert!(c.root.join("call_f").is_dir());
}

#[test]
fn delete_call_works_with_a_missing_folder_and_prunes_the_empty_parent() {
    let e = env();
    let c = e.company("Empresa");
    let x = e.lib(c.id).add_client("Cliente X").unwrap();
    let id = e.call(c.id, "call_2026-01-01_10-00-00", Some(x.id), "alfa");
    let gone = e.call(c.id, "call_2026-01-02_10-00-00", Some(x.id), "bravo");
    std::fs::remove_dir_all(c.root.join("cliente-x/call_2026-01-02_10-00-00")).unwrap();
    let mut lib = e.lib(c.id);
    lib.delete_call(&e.app, gone).unwrap();
    assert!(c.root.join("cliente-x").is_dir(), "ainda tem a outra chamada");
    lib.delete_call(&e.app, id).unwrap();
    assert!(!c.root.join("cliente-x").exists(), "pasta do cliente vazia saiu");
    assert!(c.root.is_dir() && c.root.join("library.db").is_file(), "a raiz nunca sai por aqui");
    assert_eq!(lib.delete_call(&e.app, id).unwrap_err().code(), "not_found");
    fts_consistent(&lib);
}
