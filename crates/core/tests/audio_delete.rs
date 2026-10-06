//! Apagar o áudio de uma chamada já transcrita (#24): remove só os FLACs e caches da pasta da chamada,
//! preenche `audio_deleted_at`, recusa com tarefa aberta/gravando, é idempotente e a transcrição
//! passa a explicar o motivo. Áudio sintético (bytes quaisquer: nada aqui decodifica).
use std::path::PathBuf;

use core_lib::storage::{self, AudioDeletion};
use core_lib::transcription::params::JobOptions;
use core_lib::transcription::queue::{self, JobKind};
use core_lib::{App, Result};

struct Env {
    _dir: tempfile::TempDir,
    app: App,
    lib_id: i64,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).unwrap();
    let lib_id = app.inbox_id().unwrap();
    Env { _dir: dir, app, lib_id }
}

impl Env {
    fn root(&self) -> PathBuf {
        self.app.open_library(self.lib_id).unwrap().root().to_path_buf()
    }

    /// Chamada com `mic.flac` (1000 bytes), `sys.flac` (3000), picos (200), `recording.json` e uma anotação
    /// solta; `transcribed` põe uma versão ativa.
    fn call(&self, key: &str, transcribed: bool) -> i64 {
        let lib = self.app.open_library(self.lib_id).unwrap();
        let dir = lib.root().join(key);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, n) in [("mic.flac", 1000), ("sys.flac", 3000), ("peaks.bin", 200), ("recording.json", 50), ("notes.txt", 10)] {
            std::fs::write(dir.join(name), vec![7u8; n]).unwrap();
        }
        lib.conn
            .execute(
                "INSERT INTO calls (key, started_at, created_at, dir, mic_path, sys_path, transcription_state)
                 VALUES (?1, 't', 't', ?1, ?2, ?3, ?4)",
                rusqlite::params![key, format!("{key}/mic.flac"), format!("{key}/sys.flac"), if transcribed { "done" } else { "pending" }],
            )
            .unwrap();
        let id = lib.conn.last_insert_rowid();
        if transcribed {
            lib.conn.execute("INSERT INTO transcripts (call_id, version, created_at, is_active) VALUES (?1, 1, 't', 1)", [id]).unwrap();
        }
        id
    }

    fn delete(&self, call: i64, dry: bool) -> Result<AudioDeletion> {
        storage::delete_audio(&self.app, self.lib_id, call, &[], dry)
    }

    fn deleted_at(&self, call: i64) -> Option<String> {
        self.app
            .open_library(self.lib_id)
            .unwrap()
            .conn
            .query_row("SELECT audio_deleted_at FROM calls WHERE id = ?1", [call], |r| r.get(0))
            .unwrap()
    }

    fn names(&self, key: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.root().join(key)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }
}

const ALL: [&str; 5] = ["mic.flac", "notes.txt", "peaks.bin", "recording.json", "sys.flac"];

#[test]
fn delete_removes_audio_and_caches_and_sets_the_column() {
    let e = env();
    let call = e.call("call_a", true);
    let other = e.call("call_b", true);
    let r = e.delete(call, false).unwrap();
    assert_eq!((r.bytes, r.already_deleted, r.dry_run), (4200, false, false));
    assert_eq!(r.files.iter().map(|f| (f.name.as_str(), f.kind)).collect::<Vec<_>>(), [("mic.flac", "audio"), ("sys.flac", "audio"), ("peaks.bin", "cache")]);
    assert!(r.deleted_at.is_some());
    assert_eq!(e.deleted_at(call), r.deleted_at);
    // recording.json e o que não é do áudio ficam; os caminhos continuam registrando as trilhas
    assert_eq!(e.names("call_a"), ["notes.txt", "recording.json"]);
    let lib = e.app.open_library(e.lib_id).unwrap();
    let d = lib.call_detail(call, None).unwrap();
    assert!(!d.summary.has_audio);
    assert_eq!((d.audio.mic_path.as_deref(), d.audio.deleted_at.clone()), (Some("call_a/mic.flac"), r.deleted_at));
    // outra chamada intacta (arquivos e coluna)
    assert_eq!(e.names("call_b"), ALL);
    assert!(e.deleted_at(other).is_none());
    assert!(lib.call_detail(other, None).unwrap().summary.has_audio);
}

#[test]
fn derived_caches_are_matched_by_name() {
    let e = env();
    let call = e.call("call_a", true);
    let dir = e.root().join("call_a");
    std::fs::write(dir.join("waveform.peaks"), b"x").unwrap();
    std::fs::write(dir.join("peaks.json"), b"x").unwrap();
    std::fs::write(dir.join("speakers.json"), b"x").unwrap(); // não é cache de áudio
    std::fs::create_dir(dir.join("peaks.d")).unwrap(); // subpasta: nunca é tocada
    e.delete(call, false).unwrap();
    assert_eq!(e.names("call_a"), ["notes.txt", "peaks.d", "recording.json", "speakers.json"]);
}

#[test]
fn dry_run_changes_nothing() {
    let e = env();
    let call = e.call("call_a", true);
    let r = e.delete(call, true).unwrap();
    assert_eq!((r.bytes, r.files.len(), r.dry_run, r.deleted_at), (4200, 3, true, None));
    assert_eq!(e.names("call_a"), ALL);
    assert!(e.deleted_at(call).is_none());
    // a mesma simulação depois da exclusão real: nada a liberar
    e.delete(call, false).unwrap();
    let again = e.delete(call, true).unwrap();
    assert_eq!((again.bytes, again.files.len(), again.already_deleted), (0, 0, true));
}

#[test]
fn refuses_with_a_queued_or_running_job_and_while_recording() {
    let e = env();
    let call = e.call("call_a", true);
    let job = queue::enqueue(&e.app, e.lib_id, call, JobKind::Full, &JobOptions::default()).unwrap();
    for dry in [true, false] {
        assert_eq!(e.delete(call, dry).unwrap_err().code(), "conflict");
    }
    queue::mark_running(&e.app, job.id).unwrap();
    assert_eq!(e.delete(call, false).unwrap_err().code(), "conflict");
    assert_eq!(e.names("call_a"), ALL);
    assert!(e.deleted_at(call).is_none());
    // terminada a tarefa, pode
    queue::mark_done(&e.app, job.id).unwrap();
    // gravando / convertendo essa chamada
    let busy = ["call_a".to_string()];
    assert_eq!(storage::delete_audio(&e.app, e.lib_id, call, &busy, false).unwrap_err().code(), "conflict");
    assert_eq!(e.names("call_a"), ALL);
    storage::delete_audio(&e.app, e.lib_id, call, &["call_z".to_string()], false).unwrap();
    assert!(e.deleted_at(call).is_some());
}

#[test]
fn refuses_a_call_without_transcript_and_unknown_calls() {
    let e = env();
    let call = e.call("call_a", false);
    assert_eq!(e.delete(call, false).unwrap_err().code(), "not_transcribed");
    assert_eq!(e.names("call_a"), ALL);
    assert_eq!(e.delete(9999, false).unwrap_err().code(), "not_found");
}

#[test]
fn is_idempotent_and_finishes_a_crashed_cleanup() {
    let e = env();
    let call = e.call("call_a", true);
    let first = e.delete(call, false).unwrap();
    let second = e.delete(call, false).unwrap();
    assert!(second.already_deleted && second.files.is_empty() && second.bytes == 0);
    assert_eq!(second.deleted_at, first.deleted_at, "o instante original se mantém");
    // queda entre o banco e os arquivos: a marca está lá e os arquivos sobraram
    let dir = e.root().join("call_a");
    std::fs::write(dir.join("mic.flac"), b"sobra").unwrap();
    let third = e.delete(call, false).unwrap();
    assert_eq!((third.already_deleted, third.bytes, third.deleted_at), (true, 5, first.deleted_at));
    assert_eq!(e.names("call_a"), ["notes.txt", "recording.json"]);
}

#[test]
fn transcription_reports_why_after_the_audio_is_gone() {
    let e = env();
    let call = e.call("call_a", true);
    e.delete(call, false).unwrap();
    for kind in [JobKind::Full, JobKind::Rediarize] {
        let err = queue::enqueue(&e.app, e.lib_id, call, kind, &JobOptions::default()).unwrap_err();
        assert_eq!(err.code(), "audio_deleted", "{kind:?}");
    }
    // `no_audio` continua para quem nunca teve arquivo
    let lib = e.app.open_library(e.lib_id).unwrap();
    lib.conn
        .execute("INSERT INTO calls (key, started_at, created_at, mic_path, transcription_state) VALUES ('call_x', 't', 't', 'call_x/mic.flac', 'pending')", [])
        .unwrap();
    let x = lib.conn.last_insert_rowid();
    assert_eq!(queue::enqueue(&e.app, e.lib_id, x, JobKind::Full, &JobOptions::default()).unwrap_err().code(), "no_audio");
}

#[test]
fn retrying_a_cancelled_job_after_deletion_reports_the_reason() {
    let e = env();
    let call = e.call("call_a", true);
    let job = queue::enqueue(&e.app, e.lib_id, call, JobKind::Full, &JobOptions::default()).unwrap();
    queue::mark_cancelled(&e.app, job.id).unwrap();
    e.delete(call, false).unwrap();
    assert_eq!(queue::retry(&e.app, job.id).unwrap_err().code(), "audio_deleted");
}

#[test]
fn list_shows_only_calls_with_audio_on_disk() {
    let e = env();
    let a = e.call("call_a", true);
    e.call("call_b", false);
    let c = e.call("call_c", true);
    let job = queue::enqueue(&e.app, e.lib_id, c, JobKind::Full, &JobOptions::default()).unwrap();
    let list = storage::list_audio(&e.app).unwrap();
    let view: Vec<_> = list.iter().map(|x| (x.call_key.as_str(), x.bytes, x.blocked)).collect();
    assert_eq!(view, [("call_a", 4200, None), ("call_b", 4200, Some("not_transcribed")), ("call_c", 4200, Some("job_open"))]);
    queue::mark_cancelled(&e.app, job.id).unwrap();
    e.delete(a, false).unwrap();
    let view: Vec<_> = storage::list_audio(&e.app).unwrap().iter().map(|x| x.call_key.clone()).collect();
    assert_eq!(view, ["call_b", "call_c"]);
}
