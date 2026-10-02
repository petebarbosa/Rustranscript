//! Migrações da fase 4 (app v3, library v4) e protocolo de erro dos stubs.
use core_lib::App;
use core_lib::transcription::{models, params::JobOptions, queue};

fn table_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |r| r.get::<_, i64>(0))
        .unwrap()
        == 1
}

#[test]
fn migrations_create_queue_and_raw_tables() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).unwrap();
    assert!(table_exists(&app.db, "transcription_jobs"));
    let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    for t in ["tx_stage", "tx_segments", "tx_turns", "tx_energy", "bleed_removals"] {
        assert!(table_exists(&lib.conn, t), "missing {t}");
    }
    // uma tarefa em aberto por chamada
    let ins = "INSERT INTO transcription_jobs (library_id, call_id, call_key, kind, state, created_at) VALUES (1, 7, 'call_x', 'full', ?1, 't')";
    app.db.execute(ins, ["queued"]).unwrap();
    assert!(app.db.execute(ins, ["running"]).is_err());
    app.db.execute(ins, ["done"]).unwrap(); // terminadas não contam
    // a versão aponta para o bruto de UMA tarefa só
    lib.conn.execute("INSERT INTO calls (key, started_at, created_at) VALUES ('call_a', 't', 't')", []).unwrap();
    let t = "INSERT INTO transcripts (call_id, version, created_at, raw_job_id) VALUES (1, ?1, 't', 9)";
    lib.conn.execute(t, [1]).unwrap();
    assert!(lib.conn.execute(t, [2]).is_err());
}

#[test]
fn api_errors_are_typed_never_panic() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).unwrap();
    let e = queue::enqueue(&app, 1, 1, queue::JobKind::Full, &JobOptions::default()).unwrap_err();
    assert_eq!(e.code(), "not_found");
    assert_eq!(models::model_paths(dir.path()).unwrap_err().code(), "models_missing");
    // coerência das constantes dos modelos
    assert_eq!(models::ALL.len(), 3);
    assert!(models::WHISPER.files.iter().all(|f| f.sha256.len() == 64 && f.url.starts_with("https://")));
}

/// O bug da fase 3: GUI e CLI chamando `App::open` ao mesmo tempo num diretório novo derrubavam uma das duas
/// ("table libraries already exists"). Agora todas abrem e há exatamente uma inbox.
#[test]
fn concurrent_app_open_on_new_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (path, barrier) = (path.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                App::open(&path).map(|app| app.inbox_id().unwrap())
            })
        })
        .collect();
    let ids: Vec<i64> = handles.into_iter().map(|h| h.join().unwrap().expect("App::open must not fail")).collect();
    assert!(ids.windows(2).all(|w| w[0] == w[1]));
    let app = App::open(&path).unwrap();
    let inboxes: i64 = app.db.query_row("SELECT count(*) FROM libraries WHERE kind = 'inbox'", [], |r| r.get(0)).unwrap();
    assert_eq!(inboxes, 1);
}
