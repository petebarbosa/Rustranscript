//! Importação das chamadas do pipeline antigo: `call_*.txt` (+ `_vN`), `call_*.{mic,sys}.wav`,
//! `edits/<stem>.json` e `<chave>.chapters.json` do protótipo. Idempotente pela chave da chamada.
//! Os originais nunca são apagados nem movidos; o áudio é copiado como FLAC.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;

use crate::app::App;
use crate::glossary::Engine;
use crate::library::{self, Library};
use crate::model::{Origin, Rule};
use crate::rules;
use crate::parse::{self, MERGE_CAP_CHARS, MERGE_GAP_S};
use crate::{Error, Result, audio, db, text};

#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub key: String,
    pub started_at: String,
    pub slug: Option<String>,
    pub versions: BTreeMap<u32, SourceVersion>,
    pub mic_wav: Option<PathBuf>,
    pub sys_wav: Option<PathBuf>,
    pub chapters: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SourceVersion {
    pub stem: String,
    pub txt: PathBuf,
    pub edits: Option<PathBuf>,
}

/// Varre arquivos e pastas (sem recursão) e agrupa por chamada.
pub fn scan(paths: &[PathBuf]) -> Result<Vec<Candidate>> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            for entry in std::fs::read_dir(p)? {
                let path = entry?.path();
                if path.is_file() {
                    files.push(path);
                }
            }
        } else if p.is_file() {
            files.push(p.clone());
        } else {
            return Err(Error::not_found(p.display()));
        }
    }
    files.sort();
    let mut by_key: BTreeMap<String, Candidate> = BTreeMap::new();
    for f in files {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let (stem, kind) = if let Some(s) = name.strip_suffix(".mic.wav") {
            (s, "mic")
        } else if let Some(s) = name.strip_suffix(".sys.wav") {
            (s, "sys")
        } else if let Some(s) = name.strip_suffix(".chapters.json") {
            (s, "chapters")
        } else if let Some(s) = name.strip_suffix(".txt") {
            (s, "txt")
        } else {
            continue;
        };
        let Some(info) = parse::parse_stem(stem) else { continue };
        let c = by_key.entry(info.key.clone()).or_insert_with(|| Candidate {
            key: info.key.clone(),
            started_at: info.started_at(),
            ..Default::default()
        });
        if info.slug.is_some() && (c.slug.is_none() || kind == "txt") {
            c.slug = info.slug.clone();
        }
        match kind {
            "mic" => c.mic_wav = Some(f.clone()),
            "sys" => c.sys_wav = Some(f.clone()),
            "chapters" => c.chapters = Some(f.clone()),
            _ => {
                let edits = f.parent().map(|d| d.join("edits").join(format!("{stem}.json"))).filter(|p| p.is_file());
                c.versions.insert(info.version, SourceVersion { stem: stem.to_string(), txt: f.clone(), edits });
            }
        }
    }
    Ok(by_key.into_values().collect())
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Biblioteca de destino para chamadas novas (padrão: inbox).
    pub library_id: Option<i64>,
    pub client_id: Option<i64>,
    pub convert_audio: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportItem {
    pub key: String,
    /// `new`, `updated`, `unchanged` ou `skipped`
    pub status: String,
    pub reason: Option<String>,
    pub library_id: Option<i64>,
    pub call_id: Option<i64>,
    pub versions_added: Vec<u32>,
    pub edits_applied: usize,
    /// Trocas do glossário aplicadas automaticamente às versões novas (origem `import`).
    pub glossary_replacements: usize,
    pub audio_pending: Vec<String>,
    pub audio_converted: Vec<String>,
    pub audio_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub dry_run: bool,
    pub items: Vec<ImportItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Progress {
    Call { index: usize, total: usize, key: String },
    Audio { index: usize, total: usize, key: String, track: String, done: u64, of: u64 },
    Done,
}

struct AudioJob {
    item: usize,
    library_id: i64,
    call_id: i64,
    track: &'static str,
    src: PathBuf,
}

pub fn import(app: &App, candidates: &[Candidate], opts: &ImportOptions, progress: &mut dyn FnMut(Progress)) -> Result<ImportReport> {
    let target_id = match opts.library_id {
        Some(id) => id,
        None => app.inbox_id()?,
    };
    let mut libs: HashMap<i64, Library> = HashMap::new();
    for row in app.library_rows()? {
        if row.is_inbox() || row.root.join(library::DB_FILE).is_file() {
            libs.insert(row.id, Library::open(row)?);
        }
    }
    if !libs.contains_key(&target_id) {
        return Err(Error::not_found(format!("library {target_id}")));
    }
    if let Some(cid) = opts.client_id {
        libs[&target_id].find_client(&cid.to_string())?;
    }

    // glossário global lido uma vez; as regras do cliente de cada chamada entram em `import_call`
    let global_rules = app.global_rules()?;
    let mut items = Vec::new();
    let mut jobs = Vec::new();
    for (index, cand) in candidates.iter().enumerate() {
        progress(Progress::Call { index, total: candidates.len(), key: cand.key.clone() });
        let existing = libs.iter().find_map(|(id, l)| l.call_id_by_key(&cand.key).ok().flatten().map(|c| (*id, c)));
        let lib_id = existing.map(|e| e.0).unwrap_or(target_id);
        let lib = libs.get_mut(&lib_id).unwrap();
        let mut item = import_call(lib, cand, existing.map(|e| e.1), opts, &global_rules)?;
        // Em dry-run a chamada nova é desfeita (sem `call_id`), mas o áudio que seria convertido
        // precisa aparecer na prévia; só vira trabalho de verdade quando há `call_id`.
        if item.status != "skipped" {
            for (track, wav) in [("mic", &cand.mic_wav), ("sys", &cand.sys_wav)] {
                let Some(wav) = wav else { continue };
                let has_flac = match item.call_id {
                    Some(call_id) => {
                        let col = if track == "mic" { "mic_path" } else { "sys_path" };
                        lib.conn
                            .query_row(&format!("SELECT {col} FROM calls WHERE id = ?1"), [call_id], |r| r.get::<_, Option<String>>(0))
                            .optional()?
                            .flatten()
                            .is_some()
                    }
                    None => false,
                };
                if !has_flac {
                    item.audio_pending.push(track.to_string());
                    if let Some(call_id) = item.call_id {
                        jobs.push(AudioJob { item: items.len(), library_id: lib_id, call_id, track, src: wav.clone() });
                    }
                }
            }
        }
        items.push(item);
    }

    if opts.convert_audio && !opts.dry_run {
        let total = jobs.len();
        for (index, job) in jobs.into_iter().enumerate() {
            let lib = libs.get_mut(&job.library_id).unwrap();
            let key = items[job.item].key.clone();
            let result = convert_one(lib, &job, &mut |done, of| {
                progress(Progress::Audio { index, total, key: key.clone(), track: job.track.into(), done, of })
            });
            let item = &mut items[job.item];
            match result {
                Ok(()) => {
                    item.audio_pending.retain(|t| t != job.track);
                    item.audio_converted.push(job.track.to_string());
                    if item.status == "unchanged" {
                        item.status = "updated".into();
                    }
                }
                Err(e) => item.audio_errors.push(format!("{}: {}", job.track, e.detail())),
            }
        }
    }
    progress(Progress::Done);
    Ok(ImportReport { dry_run: opts.dry_run, items })
}

fn import_call(lib: &mut Library, cand: &Candidate, existing: Option<i64>, opts: &ImportOptions, global_rules: &[Rule]) -> Result<ImportItem> {
    let mut item = ImportItem {
        key: cand.key.clone(),
        status: "unchanged".into(),
        reason: None,
        library_id: Some(lib.id()),
        call_id: existing,
        versions_added: vec![],
        edits_applied: 0,
        glossary_replacements: 0,
        audio_pending: vec![],
        audio_converted: vec![],
        audio_errors: vec![],
    };
    // versões com falas reconhecidas
    let mut parsed = Vec::new();
    let mut reasons = Vec::new();
    for (ver, src) in &cand.versions {
        let raw = std::fs::read(&src.txt)?;
        let content = String::from_utf8_lossy(&raw);
        if content.trim().is_empty() {
            reasons.push(format!("empty: {}", file_name(&src.txt)));
            continue;
        }
        let segs = parse::parse(&content);
        if segs.is_empty() {
            reasons.push(format!("no_speech: {}", file_name(&src.txt)));
            continue;
        }
        parsed.push((*ver, src, segs));
    }
    if existing.is_none() && parsed.is_empty() {
        item.status = "skipped".into();
        item.library_id = None;
        item.reason = Some(if reasons.is_empty() { "no_transcript".into() } else { reasons.join("; ") });
        return Ok(item);
    }

    let wav_secs = [&cand.sys_wav, &cand.mic_wav]
        .into_iter()
        .flatten()
        .filter_map(|p| audio::wav_info(p).ok())
        .map(|i| i.duration_s().round() as i64)
        .max()
        .unwrap_or(0);
    let slug = cand.slug.clone().unwrap_or_default();
    let lib_is_inbox = lib.row.is_inbox();
    let client_id = if lib_is_inbox { None } else { opts.client_id };
    let dir = lib.call_dir_for(&cand.key, &slug, client_id)?;

    let lib_id = lib.id();
    let tx = lib.conn.transaction()?;
    let call_id = match existing {
        Some(id) => id,
        None => {
            let seg_secs = parsed.iter().filter_map(|(_, _, s)| s.last().map(|x| x.t as i64)).max().unwrap_or(0);
            tx.execute(
                "INSERT INTO calls (key, client_id, title, slug, started_at, duration_s, language, dir, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pt', ?7, ?8)",
                params![
                    cand.key,
                    client_id,
                    text::humanize(&slug),
                    slug,
                    cand.started_at,
                    wav_secs.max(seg_secs),
                    dir.to_string_lossy(),
                    db::now()
                ],
            )?;
            item.status = "new".into();
            tx.last_insert_rowid()
        }
    };
    item.call_id = Some(call_id);

    // regras `replace` em vigor (globais + cliente da chamada), aplicadas a cada versão nova
    let call_client: Option<i64> = tx.query_row("SELECT client_id FROM calls WHERE id = ?1", [call_id], |r| r.get(0))?;
    let client_rules = match call_client {
        Some(c) if !lib_is_inbox => rules::client_rules_in(&tx, lib_id, c)?,
        _ => vec![],
    };
    let engine = Engine::new(&rules::replace_rules(&rules::merge(global_rules.to_vec(), client_rules)))?;

    for (ver, src, segs) in &parsed {
        let known: Option<i64> = tx
            .query_row("SELECT id FROM transcripts WHERE call_id = ?1 AND version = ?2", params![call_id, ver], |r| r.get(0))
            .optional()?;
        if known.is_some() {
            continue;
        }
        tx.execute(
            "INSERT INTO transcripts (call_id, version, model, engine, source_file, created_at)
             VALUES (?1, ?2, 'large-v3-turbo', 'faster-whisper', ?3, ?4)",
            params![call_id, ver, file_name(&src.txt), db::now()],
        )?;
        let tid = tx.last_insert_rowid();
        let mut by_t = HashMap::new();
        for (seq, b) in parse::merge(segs, MERGE_GAP_S, MERGE_CAP_CHARS).into_iter().enumerate() {
            let sid = speaker_id(&tx, call_id, &b.speaker)?;
            tx.execute(
                "INSERT INTO blocks (transcript_id, seq, t_start, t_end, speaker_id, original_text, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![tid, seq as i64 + 1, b.t_start as f64, b.t_end as f64, sid, b.text],
            )?;
            by_t.insert(b.t_start, tx.last_insert_rowid());
        }
        source(&tx, call_id, "transcript", &src.txt)?;
        // antes das edições do protótipo: o que o usuário editou à mão vence o glossário
        item.glossary_replacements += rules::apply_in_tx(&tx, call_id, tid, &engine, Origin::Import)?.replacements;
        if let Some(edits) = &src.edits {
            item.edits_applied += apply_prototype_edits(&tx, call_id, edits, &by_t)?;
            source(&tx, call_id, "edits", edits)?;
        }
        item.versions_added.push(*ver);
    }
    // versão ativa = a maior
    tx.execute(
        "UPDATE transcripts SET is_active = (version = (SELECT max(version) FROM transcripts WHERE call_id = ?1))
         WHERE call_id = ?1",
        [call_id],
    )?;

    if let Some(ch) = &cand.chapters {
        let has: i64 = tx.query_row("SELECT count(*) FROM chapters WHERE call_id = ?1", [call_id], |r| r.get(0))?;
        if has == 0 {
            for c in read_chapters(ch)? {
                tx.execute("INSERT INTO chapters (call_id, t, title) VALUES (?1, ?2, ?3)", params![call_id, c.0, c.1])?;
            }
            source(&tx, call_id, "chapters", ch)?;
        }
    }
    if item.status == "unchanged" && !item.versions_added.is_empty() {
        item.status = "updated".into();
    }
    if !reasons.is_empty() {
        item.reason = Some(reasons.join("; "));
    }
    if opts.dry_run {
        tx.rollback()?;
        if existing.is_none() {
            item.call_id = None;
        }
    } else {
        tx.commit()?;
    }
    Ok(item)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn source(tx: &Transaction, call_id: i64, kind: &str, path: &Path) -> Result<()> {
    let abs = path.canonicalize()?;
    let size = std::fs::metadata(&abs).map(|m| m.len() as i64).ok();
    tx.execute(
        "INSERT OR IGNORE INTO import_sources (call_id, kind, path, size) VALUES (?1, ?2, ?3, ?4)",
        params![call_id, kind, abs.to_string_lossy(), size],
    )?;
    Ok(())
}

/// "Eu" é o microfone; qualquer outro rótulo vem do áudio do sistema.
fn speaker_id(tx: &Transaction, call_id: i64, label: &str) -> Result<i64> {
    if let Some(id) = tx
        .query_row("SELECT id FROM speakers WHERE call_id = ?1 AND label = ?2", params![call_id, label], |r| r.get(0))
        .optional()?
    {
        return Ok(id);
    }
    let track = if label.eq_ignore_ascii_case("eu") { "mic" } else { "sys" };
    tx.execute("INSERT INTO speakers (call_id, track, label) VALUES (?1, ?2, ?3)", params![call_id, track, label])?;
    Ok(tx.last_insert_rowid())
}

/// `edits/<stem>.json` do protótipo: `{"<segundo inicial do bloco>": "texto editado"}`.
fn apply_prototype_edits(tx: &Transaction, call_id: i64, path: &Path, by_t: &HashMap<u32, i64>) -> Result<usize> {
    let map: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let mut n = 0;
    for (t, new_text) in map {
        let Some(&block_id) = t.parse::<u32>().ok().and_then(|t| by_t.get(&t)) else { continue };
        let new_text = text::normalize_ws(&new_text);
        let (old, original): (String, String) =
            tx.query_row("SELECT text, original_text FROM blocks WHERE id = ?1", [block_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if new_text.is_empty() || new_text == old {
            continue;
        }
        library::apply_block_text(tx, block_id, &new_text, &original)?;
        library::record(tx, call_id, "block_text", block_id, Some(&old), Some(&new_text), Origin::Import)?;
        n += 1;
    }
    Ok(n)
}

/// `[{"t": "00:03:44" | 224, "title": "..."}]`
fn read_chapters(path: &Path) -> Result<Vec<(f64, String)>> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let mut out = Vec::new();
    for v in raw {
        let title = v.get("title").and_then(|t| t.as_str()).map(text::normalize_ws).unwrap_or_default();
        let t = match v.get("t") {
            Some(serde_json::Value::Number(n)) => n.as_f64(),
            Some(serde_json::Value::String(s)) => {
                let parts: Vec<f64> = s.split(':').filter_map(|p| p.parse().ok()).collect();
                Some(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p))
            }
            _ => None,
        };
        if let (Some(t), false) = (t, title.is_empty()) {
            out.push((t, title));
        }
    }
    Ok(out)
}

fn convert_one(lib: &mut Library, job: &AudioJob, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let dir: String = lib.conn.query_row("SELECT dir FROM calls WHERE id = ?1", [job.call_id], |r| r.get(0))?;
    let rel = Path::new(&dir).join(format!("{}.flac", job.track));
    let dst = lib.audio_abs(&rel.to_string_lossy());
    audio::wav_to_flac(&job.src, &dst, progress)?;
    let col = if job.track == "mic" { "mic_path" } else { "sys_path" };
    let tx = lib.conn.transaction()?;
    tx.execute(&format!("UPDATE calls SET {col} = ?1 WHERE id = ?2"), params![rel.to_string_lossy(), job.call_id])?;
    source(&tx, job.call_id, job.track, &job.src)?;
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct ReclaimableFile {
    pub library_id: i64,
    pub call_key: String,
    pub kind: String,
    pub path: String,
    pub size: u64,
}

/// Originais já importados que ainda estão no disco (podem ser apagados pelo usuário).
pub fn reclaimable(app: &App) -> Result<Vec<ReclaimableFile>> {
    let mut out = Vec::new();
    for row in app.library_rows()? {
        if !row.root.join(library::DB_FILE).is_file() {
            continue;
        }
        let lib = Library::open(row)?;
        let mut stmt = lib.conn.prepare(
            "SELECT c.key, s.kind, s.path FROM import_sources s JOIN calls c ON c.id = s.call_id
             WHERE s.kind != 'mic' OR c.mic_path IS NOT NULL ORDER BY c.key, s.kind",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (key, kind, path) = row?;
            if kind == "sys" {
                // só conta o WAV de sistema se o FLAC correspondente já existir
                let ok: bool = lib.conn.query_row("SELECT sys_path IS NOT NULL FROM calls WHERE key = ?1", [&key], |r| r.get(0))?;
                if !ok {
                    continue;
                }
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                out.push(ReclaimableFile { library_id: lib.id(), call_key: key, kind, path, size: meta.len() });
            }
        }
    }
    Ok(out)
}
