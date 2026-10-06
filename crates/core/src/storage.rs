//! Apagar o áudio de uma chamada já transcrita (libera espaço; a transcrição fica).
//!
//! O que sai: `mic.flac`, `sys.flac` e os caches derivados, só dentro da pasta da própria chamada.
//! O que fica: `recording.json`, a linha em `calls` (com `mic_path`/`sys_path` como registro de quais
//! trilhas existiram; a fonte da verdade é `audio_deleted_at`) e tudo da transcrição.
//!
//! Ordem e queda no meio: o banco primeiro, os arquivos depois. Se o processo cair entre os dois, a
//! chamada já está "sem áudio" (nada tenta tocar, cortar ou refazer) e sobram arquivos órfãos que a
//! próxima chamada de `delete_audio` limpa (ela é idempotente: com `audio_deleted_at` já preenchido
//! só remove o que sobrou). A ordem inversa deixaria o banco dizendo "tem áudio" para arquivos que
//! não existem mais.
use std::path::{Component, Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::library::Library;
use crate::transcription::queue::open_available;
use crate::{App, Error, Result, db};

/// Trilhas de áudio da chamada (os FLACs).
const AUDIO_FILES: [&str; 2] = ["mic.flac", "sys.flac"];

/// Cache derivado do áudio (picos da onda do player, #22): qualquer arquivo da pasta da chamada cujo
/// nome comece com `peaks` ou termine em `.peaks`. O nome exato do cache é do #22; este critério
/// cobre os dois jeitos de chamá-lo sem depender do formato.
fn is_derived_cache(name: &str) -> bool {
    name.starts_with("peaks") || name.ends_with(".peaks")
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioFile {
    pub name: String,
    pub bytes: u64,
    /// `audio` (trilha) ou `cache` (derivado, regenerável).
    pub kind: &'static str,
}

/// Resultado de `delete_audio`. Em `dry_run` descreve o que seria feito (nada muda).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioDeletion {
    pub library_id: i64,
    pub call_id: i64,
    pub call_key: String,
    pub dry_run: bool,
    /// A chamada já estava sem áudio antes deste pedido (`files` traz só o que sobrou, em geral nada).
    pub already_deleted: bool,
    /// Arquivos removidos (ou que seriam, em `dry_run`).
    pub files: Vec<AudioFile>,
    /// Soma de `files`: o espaço liberado.
    pub bytes: u64,
    /// `audio_deleted_at` (em `dry_run` de uma chamada ainda com áudio: `None`).
    pub deleted_at: Option<String>,
}

/// Chamada com áudio ocupando espaço (lista da tela de armazenamento e `rstt audio list`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioEntry {
    pub library_id: i64,
    pub call_id: i64,
    pub call_key: String,
    pub title: String,
    pub client_name: Option<String>,
    pub started_at: String,
    pub duration_s: i64,
    /// Tamanho no disco das trilhas e caches.
    pub bytes: u64,
    /// Por que não dá para apagar agora: `not_transcribed` ou `job_open`. `None` = pode.
    pub blocked: Option<&'static str>,
}

/// Pasta da chamada, validada: relativa, dentro da biblioteca e nunca a própria raiz.
fn call_dir(lib: &Library, dir: Option<&str>) -> Result<Option<PathBuf>> {
    let Some(dir) = dir.filter(|d| !d.is_empty()) else { return Ok(None) };
    let rel = Path::new(dir);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(Error::invalid(format!("call directory outside the library: {dir}")));
    }
    Ok(Some(lib.root().join(rel)))
}

/// Arquivos da chamada que a exclusão leva, com tamanho. Só arquivos comuns (links simbólicos e
/// subpastas ficam onde estão).
fn removable(dir: &Path) -> Result<Vec<AudioFile>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = if AUDIO_FILES.contains(&name.as_str()) {
            "audio"
        } else if is_derived_cache(&name) {
            "cache"
        } else {
            continue;
        };
        files.push(AudioFile { bytes: entry.metadata()?.len(), name, kind });
    }
    files.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    Ok(files)
}

fn open_jobs(app: &App, library_id: i64, call_id: i64) -> Result<bool> {
    Ok(app.db.query_row(
        "SELECT EXISTS (SELECT 1 FROM transcription_jobs WHERE library_id = ?1 AND call_id = ?2 AND state IN ('queued', 'running'))",
        params![library_id, call_id],
        |r| r.get(0),
    )?)
}

/// Apaga o áudio de uma chamada. `recording` = chaves de gravações em curso ou sendo convertidas
/// (quem chama pergunta à app; a CLI pelo socket): se a chamada está entre elas, recusa.
///
/// Recusas: `not_found`; `not_transcribed` (sem nenhuma versão: o áudio ainda é a única fonte);
/// `conflict` (gravando/convertendo, ou tarefa de transcrição na fila ou rodando). Em `dry_run` valem as
/// mesmas recusas, para o resultado dizer a verdade.
pub fn delete_audio(app: &App, library_id: i64, call_id: i64, recording: &[String], dry_run: bool) -> Result<AudioDeletion> {
    let lib = open_available(app, library_id)?.ok_or_else(|| Error::not_found(format!("library {library_id} is unavailable")))?;
    let (key, dir, deleted_at, versions): (String, Option<String>, Option<String>, i64) = lib
        .conn
        .query_row(
            "SELECT key, dir, audio_deleted_at, (SELECT count(*) FROM transcripts t WHERE t.call_id = calls.id) FROM calls WHERE id = ?1",
            [call_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| Error::not_found(format!("call {library_id}:{call_id}")))?;
    if recording.contains(&key) {
        return Err(Error::Conflict(format!("call {key} is being recorded")));
    }
    if open_jobs(app, library_id, call_id)? {
        return Err(Error::Conflict(format!("call {key} has a transcription job queued or running")));
    }
    let already_deleted = deleted_at.is_some();
    if !already_deleted && versions == 0 {
        return Err(Error::transcription("not_transcribed", format!("call {key}: the audio is the only source of the text")));
    }
    let dir = call_dir(&lib, dir.as_deref())?;
    let files = match &dir {
        Some(d) => removable(d)?,
        None => Vec::new(),
    };
    let bytes = files.iter().map(|f| f.bytes).sum();
    let mut out = AudioDeletion { library_id, call_id, call_key: key.clone(), dry_run, already_deleted, files, bytes, deleted_at: deleted_at.clone() };
    if dry_run {
        return Ok(out);
    }

    // 1. banco: a chamada passa a "sem áudio" (só na primeira vez; o instante original se mantém)
    if !already_deleted {
        let now = db::now();
        lib.conn.execute("UPDATE calls SET audio_deleted_at = ?1 WHERE id = ?2 AND audio_deleted_at IS NULL", params![now, call_id])?;
        // Uma tarefa pode ter entrado na fila entre a checagem e a gravação (a fila vive em outro banco).
        // O áudio ainda está no disco: desfaz a marca e recusa, nada foi apagado.
        if open_jobs(app, library_id, call_id)? {
            lib.conn.execute("UPDATE calls SET audio_deleted_at = NULL WHERE id = ?1", [call_id])?;
            return Err(Error::Conflict(format!("call {key} has a transcription job queued or running")));
        }
        out.deleted_at = Some(now);
    }
    // 2. arquivos: tenta todos; o primeiro erro (que não seja "já não existe") sobe no fim. A marca do
    // banco fica, e repetir o comando termina a limpeza.
    if let Some(d) = &dir {
        let mut first_err = None;
        for f in &out.files {
            match std::fs::remove_file(d.join(&f.name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if let Some(e) = first_err {
            return Err(e.into());
        }
    }
    Ok(out)
}

/// Chamadas que ainda têm áudio no disco (de todas as bibliotecas disponíveis), da mais pesada para a
/// mais leve. Chamada já sem áudio não entra.
pub fn list_audio(app: &App) -> Result<Vec<AudioEntry>> {
    let mut out = Vec::new();
    for row in app.library_rows()? {
        let Some(lib) = open_available(app, row.id)? else { continue };
        let mut stmt = lib.conn.prepare(
            "SELECT c.id, c.key, c.title, cl.name, c.started_at, c.duration_s, c.dir,
                    (SELECT count(*) FROM transcripts t WHERE t.call_id = c.id)
             FROM calls c LEFT JOIN clients cl ON cl.id = c.client_id
             WHERE (c.mic_path IS NOT NULL OR c.sys_path IS NOT NULL) AND c.audio_deleted_at IS NULL",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, i64>(7)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (call_id, call_key, title, client_name, started_at, duration_s, dir, versions) in rows {
            let bytes = match call_dir(&lib, dir.as_deref()) {
                Ok(Some(d)) => removable(&d)?.iter().map(|f| f.bytes).sum(),
                _ => 0,
            };
            if bytes == 0 {
                continue; // registrada com áudio, mas sem nada no disco: não há o que liberar
            }
            let blocked = if versions == 0 {
                Some("not_transcribed")
            } else if open_jobs(app, row.id, call_id)? {
                Some("job_open")
            } else {
                None
            };
            out.push(AudioEntry { library_id: row.id, call_id, call_key, title, client_name, started_at, duration_s, bytes, blocked });
        }
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.call_key.cmp(&b.call_key)));
    Ok(out)
}
