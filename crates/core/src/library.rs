//! Uma biblioteca = uma pasta com `library.db` + áudio das chamadas.
//! Empresa/projeto ou a inbox ("Não classificadas"), ambas com o mesmo esquema.
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::app::LibraryRow;
use crate::model::*;
use crate::text::{self, MAX_BLOCK_CHARS, MAX_TITLE_CHARS};
use crate::{Error, Result, db, fsx, schema};

pub const DB_FILE: &str = "library.db";
pub const UNASSIGNED_DIR: &str = "_unassigned";

pub struct Library {
    pub row: LibraryRow,
    pub conn: Connection,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClientFilter {
    Any,
    Unassigned,
    Id(i64),
}

impl Library {
    pub fn open(row: LibraryRow) -> Result<Library> {
        std::fs::create_dir_all(&row.root)?;
        let conn = db::open(&row.root.join(DB_FILE), schema::LIBRARY_MIGRATIONS)?;
        Ok(Library { row, conn })
    }

    pub fn id(&self) -> i64 {
        self.row.id
    }

    pub fn root(&self) -> &Path {
        &self.row.root
    }

    pub fn counts(&self) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT count(*), coalesce(sum(client_id IS NULL), 0) FROM calls",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    // ------------------------------------------------------------------ clients

    pub fn clients(&self) -> Result<Vec<ClientInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT cl.id, cl.name, cl.slug, (SELECT count(*) FROM calls c WHERE c.client_id = cl.id)
             FROM clients cl ORDER BY cl.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ClientInfo { id: r.get(0)?, library_id: self.id(), name: r.get(1)?, slug: r.get(2)?, call_count: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_client(&self, name: &str) -> Result<ClientInfo> {
        if self.row.is_inbox() {
            return Err(Error::invalid("unclassified calls have no clients"));
        }
        let name = text::normalize_ws(name);
        if name.is_empty() {
            return Err(Error::invalid("client name is empty"));
        }
        if let Some(c) = self.clients()?.into_iter().find(|c| c.name.to_lowercase() == name.to_lowercase()) {
            return Err(Error::Conflict(format!("client already exists: {}", c.name)));
        }
        let base = match text::slugify(&name) {
            s if s.is_empty() => "client".to_string(),
            s => s,
        };
        let mut slug = base.clone();
        let mut n = 2;
        while self.conn.query_row("SELECT 1 FROM clients WHERE slug = ?1", [&slug], |_| Ok(())).optional()?.is_some() {
            slug = format!("{base}-{n}");
            n += 1;
        }
        self.conn.execute(
            "INSERT INTO clients (name, slug, created_at) VALUES (?1, ?2, ?3)",
            params![name, slug, db::now()],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(ClientInfo { id, library_id: self.id(), name, slug, call_count: 0 })
    }

    pub fn rename_client(&self, id: i64, name: &str) -> Result<()> {
        let name = text::normalize_ws(name);
        if name.is_empty() {
            return Err(Error::invalid("client name is empty"));
        }
        // o slug (nome da pasta) fica: renomear não move áudio
        let n = self.conn.execute("UPDATE clients SET name = ?1 WHERE id = ?2", params![name, id])?;
        if n == 0 { Err(Error::not_found(format!("client {id}"))) } else { Ok(()) }
    }

    /// Por id, nome ou slug (sem diferenciar caixa).
    pub fn find_client(&self, reference: &str) -> Result<ClientInfo> {
        let clients = self.clients()?;
        if let Ok(id) = reference.parse::<i64>()
            && let Some(c) = clients.iter().find(|c| c.id == id)
        {
            return Ok(c.clone());
        }
        let wanted = reference.to_lowercase();
        clients
            .into_iter()
            .find(|c| c.name.to_lowercase() == wanted || c.slug == wanted)
            .ok_or_else(|| Error::not_found(format!("client {reference}")))
    }

    // -------------------------------------------------------------------- calls

    pub fn call_exists(&self, id: i64) -> Result<()> {
        self.conn
            .query_row("SELECT 1 FROM calls WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .ok_or_else(|| Error::not_found(format!("call {}:{id}", self.id())))
    }

    pub fn call_id_by_key(&self, key: &str) -> Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT id FROM calls WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn calls(&self, filter: ClientFilter) -> Result<Vec<CallSummary>> {
        let (cond, arg) = match filter {
            ClientFilter::Any => ("?1 IS NULL", None),
            ClientFilter::Unassigned => ("?1 IS NULL AND client_id IS NULL", None),
            ClientFilter::Id(id) => ("client_id = ?1", Some(id)),
        };
        let ids: Vec<i64> = {
            let mut stmt = self.conn.prepare(&format!("SELECT id FROM calls WHERE {cond} ORDER BY started_at DESC"))?;
            stmt.query_map([arg], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        ids.into_iter().map(|id| self.call_summary(id)).collect()
    }

    pub fn call_summary(&self, id: i64) -> Result<CallSummary> {
        let mut s = self
            .conn
            .query_row(
                "SELECT c.id, c.key, c.title, c.client_id, cl.name, c.started_at, c.duration_s,
                        (c.mic_path IS NOT NULL OR c.sys_path IS NOT NULL) AND c.audio_deleted_at IS NULL,
                        (SELECT count(*) FROM transcripts t WHERE t.call_id = c.id),
                        c.transcription_state, c.transcription_error
                 FROM calls c LEFT JOIN clients cl ON cl.id = c.client_id WHERE c.id = ?1",
                [id],
                |r| {
                    Ok(CallSummary {
                        library_id: self.id(),
                        id: r.get(0)?,
                        key: r.get(1)?,
                        title: r.get(2)?,
                        client_id: r.get(3)?,
                        client_name: r.get(4)?,
                        started_at: r.get(5)?,
                        duration_s: r.get(6)?,
                        has_audio: r.get(7)?,
                        versions: r.get(8)?,
                        transcription_state: r.get(9)?,
                        transcription_error: r.get(10)?,
                        words: 0,
                        preview: String::new(),
                        edited_blocks: 0,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::not_found(format!("call {}:{id}", self.id())))?;
        if let Some(tid) = self.active_transcript_id(id)? {
            let mut stmt = self.conn.prepare("SELECT text, edited_at IS NOT NULL FROM blocks WHERE transcript_id = ?1 AND deleted_at IS NULL ORDER BY seq")?;
            let mut first = Vec::new();
            for row in stmt.query_map([tid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))? {
                let (t, edited) = row?;
                s.words += text::word_count(&t);
                s.edited_blocks += edited as i64;
                if first.len() < 3 {
                    first.push(t);
                }
            }
            s.preview = text::preview(&first.join(" "), 200);
        }
        Ok(s)
    }

    pub fn active_transcript_id(&self, call_id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM transcripts WHERE call_id = ?1 AND is_active = 1", [call_id], |r| r.get(0))
            .optional()?)
    }

    pub fn call_detail(&self, id: i64, transcript_id: Option<i64>) -> Result<CallDetail> {
        let summary = self.call_summary(id)?;
        let transcripts = self.transcripts(id)?;
        // Chamada sem transcrição (gravação recém-feita): detalhe com listas vazias e `transcript_id: None`.
        // Pedir uma versão específica de uma chamada sem versões continua sendo `not_found`.
        let transcript_id = match transcript_id {
            Some(t) if transcripts.iter().any(|x| x.id == t) => Some(t),
            Some(t) => return Err(Error::not_found(format!("transcript {t}"))),
            None => transcripts.iter().find(|t| t.is_active).or(transcripts.last()).map(|t| t.id),
        };
        let (language, expected_speakers, mic_path, sys_path, deleted_at) = self.conn.query_row(
            "SELECT language, expected_speakers, mic_path, sys_path, audio_deleted_at FROM calls WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        Ok(CallDetail {
            summary,
            library_name: self.row.name.clone(),
            language,
            expected_speakers,
            transcript_id,
            transcripts,
            speakers: self.speakers(id)?,
            blocks: match transcript_id {
                Some(t) => self.blocks(t)?,
                None => Vec::new(),
            },
            deleted_blocks: match transcript_id {
                Some(t) => self.deleted_blocks(t)?,
                None => Vec::new(),
            },
            chapters: self.chapters(id)?,
            audio: AudioInfo { mic_path, sys_path, deleted_at },
            cuts: self.cuts(id)?,
        })
    }

    pub fn transcripts(&self, call_id: i64) -> Result<Vec<TranscriptInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, version, model, engine, source_file, created_at, is_active, raw_job_id IS NOT NULL
             FROM transcripts WHERE call_id = ?1 ORDER BY version",
        )?;
        let rows = stmt.query_map([call_id], |r| {
            Ok(TranscriptInfo {
                id: r.get(0)?,
                version: r.get(1)?,
                model: r.get(2)?,
                engine: r.get(3)?,
                source_file: r.get(4)?,
                created_at: r.get(5)?,
                is_active: r.get(6)?,
                has_raw: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn speakers(&self, call_id: i64) -> Result<Vec<SpeakerInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, track, label, name FROM speakers WHERE call_id = ?1 ORDER BY track = 'sys', id",
        )?;
        let rows = stmt.query_map([call_id], |r| {
            Ok(SpeakerInfo { id: r.get(0)?, track: r.get(1)?, label: r.get(2)?, name: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Blocos vivos da versão (os excluídos ficam de fora: tela, textos e contagens partem daqui).
    pub fn blocks(&self, transcript_id: i64) -> Result<Vec<BlockInfo>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BLOCK_COLS} FROM blocks WHERE transcript_id = ?1 AND deleted_at IS NULL ORDER BY seq"
        ))?;
        let rows = stmt.query_map([transcript_id], block_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Blocos excluídos da versão, com o `seq` que tinham (`tary edit restore` usa esse número).
    pub fn deleted_blocks(&self, transcript_id: i64) -> Result<Vec<BlockInfo>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BLOCK_COLS} FROM blocks WHERE transcript_id = ?1 AND deleted_at IS NOT NULL ORDER BY seq"
        ))?;
        let rows = stmt.query_map([transcript_id], block_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Um bloco pelo id, vivo ou excluído (`deleted_at` diz qual).
    pub fn block(&self, block_id: i64) -> Result<BlockInfo> {
        self.conn
            .query_row(&format!("SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1"), [block_id], block_from_row)
            .optional()?
            .ok_or_else(|| Error::not_found(format!("block {block_id}")))
    }

    /// Bloco pelo número de sequência (como aparece em `show`) na versão ativa. Acha também os
    /// excluídos: o `seq` não muda, e é assim que `restore` os encontra.
    pub fn block_id_by_seq(&self, call_id: i64, seq: i64) -> Result<i64> {
        let tid = self.active_transcript_id(call_id)?.ok_or_else(|| Error::not_found("active transcript"))?;
        self.conn
            .query_row("SELECT id FROM blocks WHERE transcript_id = ?1 AND seq = ?2", params![tid, seq], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::not_found(format!("block #{seq}")))
    }

    pub fn chapters(&self, call_id: i64) -> Result<Vec<Chapter>> {
        let mut stmt = self.conn.prepare("SELECT t, title FROM chapters WHERE call_id = ?1 ORDER BY t")?;
        let rows = stmt.query_map([call_id], |r| Ok(Chapter { t: r.get(0)?, title: r.get(1)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn call_of_block(tx: &Connection, block_id: i64) -> Result<i64> {
        tx.query_row(
            "SELECT t.call_id FROM blocks b JOIN transcripts t ON t.id = b.transcript_id WHERE b.id = ?1",
            [block_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::not_found(format!("block {block_id}")))
    }

    // ------------------------------------------------------------------ editing
    // Toda edição roda numa transação; com `dry_run` ela é desfeita e só o resultado volta.

    pub(crate) fn edit<T>(&mut self, dry_run: bool, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let out = f(&tx)?;
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()?;
        }
        Ok(out)
    }

    pub fn set_block_text(&mut self, block_id: i64, new_text: &str, origin: Origin, dry_run: bool) -> Result<BlockInfo> {
        Ok(self.set_block_text_full(block_id, new_text, origin, dry_run)?.0)
    }

    /// Como `set_block_text`, mas devolve também o id da entrada de histórico (`None` se o texto
    /// não mudou) e o texto de antes, lido na mesma transação.
    pub(crate) fn set_block_text_full(
        &mut self,
        block_id: i64,
        new_text: &str,
        origin: Origin,
        dry_run: bool,
    ) -> Result<(BlockInfo, Option<i64>, String)> {
        let new_text = text::normalize_ws(new_text);
        if new_text.is_empty() {
            return Err(Error::invalid("text is empty"));
        }
        if new_text.chars().count() > MAX_BLOCK_CHARS {
            return Err(Error::invalid(format!("text longer than {MAX_BLOCK_CHARS} characters")));
        }
        self.edit(dry_run, |tx| {
            let call_id = Self::call_of_block(tx, block_id)?;
            Self::ensure_alive(tx, block_id)?;
            let old: String = tx.query_row("SELECT text FROM blocks WHERE id = ?1", [block_id], |r| r.get(0))?;
            let edit_id = write_block_text(tx, call_id, block_id, &new_text, origin, None)?;
            Ok((read_block(tx, block_id)?, edit_id, old))
        })
    }

    /// Bloco excluído não se edita: restaura primeiro (o texto e o histórico dele ficam como estão).
    fn ensure_alive(tx: &Connection, block_id: i64) -> Result<()> {
        let deleted: bool = tx.query_row("SELECT deleted_at IS NOT NULL FROM blocks WHERE id = ?1", [block_id], |r| r.get(0))?;
        if deleted { Err(Error::Conflict(format!("block {block_id} is deleted; restore it first"))) } else { Ok(()) }
    }

    pub fn revert_block(&mut self, block_id: i64, origin: Origin, dry_run: bool) -> Result<BlockInfo> {
        let original: String = self
            .conn
            .query_row("SELECT original_text FROM blocks WHERE id = ?1", [block_id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::not_found(format!("block {block_id}")))?;
        self.set_block_text(block_id, &original, origin, dry_run)
    }

    pub fn set_title(&mut self, call_id: i64, title: &str, origin: Origin, dry_run: bool) -> Result<CallSummary> {
        let title = text::normalize_ws(title);
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(Error::invalid(format!("title longer than {MAX_TITLE_CHARS} characters")));
        }
        self.call_exists(call_id)?;
        let id = self.id();
        self.edit(dry_run, |tx| {
            let old: String = tx.query_row("SELECT title FROM calls WHERE id = ?1", [call_id], |r| r.get(0))?;
            if old != title {
                tx.execute("UPDATE calls SET title = ?1 WHERE id = ?2", params![title, call_id])?;
                record(tx, call_id, "call_title", call_id, Some(&old), Some(&title), origin)?;
            }
            let mut s = Library::summary_in(tx, id, call_id)?;
            s.title = title.clone();
            Ok(s)
        })
    }

    fn summary_in(tx: &Connection, library_id: i64, call_id: i64) -> Result<CallSummary> {
        Ok(tx.query_row(
            "SELECT c.key, c.title, c.client_id, c.started_at, c.duration_s, c.transcription_state, c.transcription_error
             FROM calls c WHERE c.id = ?1",
            [call_id],
            |r| {
                Ok(CallSummary {
                    library_id,
                    id: call_id,
                    key: r.get(0)?,
                    title: r.get(1)?,
                    client_id: r.get(2)?,
                    client_name: None,
                    started_at: r.get(3)?,
                    duration_s: r.get(4)?,
                    words: 0,
                    preview: String::new(),
                    edited_blocks: 0,
                    versions: 0,
                    has_audio: false,
                    transcription_state: r.get(5)?,
                    transcription_error: r.get(6)?,
                })
            },
        )?)
    }

    /// Resolve um falante da chamada por id ou rótulo ("Pessoa 1", "Outros", "Eu").
    pub fn find_speaker(&self, call_id: i64, reference: &str) -> Result<SpeakerInfo> {
        let speakers = self.speakers(call_id)?;
        if let Ok(id) = reference.parse::<i64>()
            && let Some(s) = speakers.iter().find(|s| s.id == id)
        {
            return Ok(s.clone());
        }
        let wanted = reference.to_lowercase();
        speakers
            .into_iter()
            .find(|s| s.label.to_lowercase() == wanted || s.name.as_deref().map(str::to_lowercase) == Some(wanted.clone()))
            .ok_or_else(|| Error::not_found(format!("speaker {reference}")))
    }

    /// Renomeia em massa: todos os blocos do falante passam a mostrar o novo nome.
    pub fn rename_speaker(&mut self, speaker_id: i64, name: Option<&str>, origin: Origin, dry_run: bool) -> Result<SpeakerInfo> {
        let name = name.map(text::normalize_ws).filter(|n| !n.is_empty());
        if name.as_ref().is_some_and(|n| n.chars().count() > 80) {
            return Err(Error::invalid("name longer than 80 characters"));
        }
        self.edit(dry_run, |tx| {
            let (call_id, old): (i64, Option<String>) = tx
                .query_row("SELECT call_id, name FROM speakers WHERE id = ?1", [speaker_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| Error::not_found(format!("speaker {speaker_id}")))?;
            if old != name {
                tx.execute("UPDATE speakers SET name = ?1 WHERE id = ?2", params![name, speaker_id])?;
                record(tx, call_id, "speaker_name", speaker_id, old.as_deref(), name.as_deref(), origin)?;
            }
            Ok(tx.query_row("SELECT id, track, label, name FROM speakers WHERE id = ?1", [speaker_id], |r| {
                Ok(SpeakerInfo { id: r.get(0)?, track: r.get(1)?, label: r.get(2)?, name: r.get(3)? })
            })?)
        })
    }

    /// Corrige a atribuição de um bloco a outro falante da mesma chamada.
    pub fn set_block_speaker(&mut self, block_id: i64, speaker_id: i64, origin: Origin, dry_run: bool) -> Result<BlockInfo> {
        self.edit(dry_run, |tx| {
            let call_id = Self::call_of_block(tx, block_id)?;
            Self::ensure_alive(tx, block_id)?;
            let ok = tx
                .query_row("SELECT 1 FROM speakers WHERE id = ?1 AND call_id = ?2", params![speaker_id, call_id], |_| Ok(()))
                .optional()?;
            if ok.is_none() {
                return Err(Error::invalid(format!("speaker {speaker_id} is not part of this call")));
            }
            let old: i64 = tx.query_row("SELECT speaker_id FROM blocks WHERE id = ?1", [block_id], |r| r.get(0))?;
            if old != speaker_id {
                tx.execute("UPDATE blocks SET speaker_id = ?1 WHERE id = ?2", params![speaker_id, block_id])?;
                record(tx, call_id, "block_speaker", block_id, Some(&old.to_string()), Some(&speaker_id.to_string()), origin)?;
            }
            read_block(tx, block_id)
        })
    }

    /// Exclui blocos (exclusão lógica) numa transação só e num lote só do histórico. Já excluído
    /// vem em `unchanged`, sem erro e sem histórico; id inexistente desfaz tudo (`not_found`).
    pub fn delete_blocks(&mut self, block_ids: &[i64], origin: Origin, dry_run: bool) -> Result<BlocksChange> {
        self.set_blocks_deleted(block_ids, true, origin, dry_run)
    }

    /// Desfaz `delete_blocks`: o bloco volta com o mesmo `seq` e texto, e reentra na busca.
    pub fn restore_blocks(&mut self, block_ids: &[i64], origin: Origin, dry_run: bool) -> Result<BlocksChange> {
        self.set_blocks_deleted(block_ids, false, origin, dry_run)
    }

    fn set_blocks_deleted(&mut self, block_ids: &[i64], delete: bool, origin: Origin, dry_run: bool) -> Result<BlocksChange> {
        let mut ids = Vec::with_capacity(block_ids.len());
        for id in block_ids {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        if ids.is_empty() {
            return Err(Error::invalid("no blocks given"));
        }
        self.edit(dry_run, |tx| {
            let batch = new_batch(tx, if delete { "delete" } else { "restore" })?;
            let now = db::now();
            let mut out = BlocksChange { changed: vec![], unchanged: vec![], cuts_added: vec![], cuts_removed: vec![] };
            for id in ids {
                let call_id = Self::call_of_block(tx, id)?;
                let old: Option<String> = tx.query_row("SELECT deleted_at FROM blocks WHERE id = ?1", [id], |r| r.get(0))?;
                if old.is_some() == delete {
                    out.unchanged.push(read_block(tx, id)?);
                    continue;
                }
                let new = delete.then(|| now.clone());
                tx.execute("UPDATE blocks SET deleted_at = ?1 WHERE id = ?2", params![new, id])?;
                record_in(tx, call_id, "block_deleted", id, old.as_deref(), new.as_deref(), origin, Some(batch))?;
                let block = read_block(tx, id)?;
                // excluir o trecho também corta o áudio dele; restaurar devolve (tira os cortes ligados)
                if delete {
                    out.cuts_added.extend(crate::cuts::link_block_cut(tx, call_id, &block, origin, batch)?);
                } else {
                    out.cuts_removed.extend(crate::cuts::unlink_block_cuts(tx, call_id, id, origin, batch)?);
                }
                out.changed.push(block);
            }
            Ok(out)
        })
    }

    pub fn set_active_transcript(&mut self, call_id: i64, transcript_id: i64) -> Result<()> {
        self.edit(false, |tx| {
            let ok = tx
                .query_row("SELECT 1 FROM transcripts WHERE id = ?1 AND call_id = ?2", params![transcript_id, call_id], |_| Ok(()))
                .optional()?;
            if ok.is_none() {
                return Err(Error::not_found(format!("transcript {transcript_id}")));
            }
            tx.execute("UPDATE transcripts SET is_active = 0 WHERE call_id = ?1", [call_id])?;
            tx.execute("UPDATE transcripts SET is_active = 1 WHERE id = ?1", [transcript_id])?;
            Ok(())
        })
    }

    pub fn history(&self, call_id: Option<i64>, limit: i64) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {HISTORY_COLS} FROM edit_history WHERE (?1 IS NULL OR call_id = ?1) ORDER BY id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![call_id, limit], history_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Desfaz a alteração mais recente ainda não desfeita (da chamada, se informada). Se ela faz
    /// parte de um lote (glossário aplicado à chamada), desfaz o lote inteiro, do fim para o
    /// começo, e devolve a entrada mais recente dele (`batch_size` diz quantas foram).
    /// Alterações da importação não entram: desfazê-las é "reverter" o bloco.
    pub fn undo(&mut self, call_id: Option<i64>, dry_run: bool) -> Result<Option<HistoryEntry>> {
        self.edit(dry_run, |tx| {
            let entry = tx
                .query_row(
                    &format!(
                        "SELECT {HISTORY_COLS} FROM edit_history
                         WHERE undone_at IS NULL AND origin != 'import' AND (?1 IS NULL OR call_id = ?1)
                         ORDER BY id DESC LIMIT 1"
                    ),
                    [call_id],
                    history_from_row,
                )
                .optional()?;
            let Some(mut head) = entry else { return Ok(None) };
            let targets = match head.batch_id {
                Some(b) => {
                    let mut stmt = tx.prepare(&format!(
                        "SELECT {HISTORY_COLS} FROM edit_history WHERE batch_id = ?1 AND undone_at IS NULL ORDER BY id DESC"
                    ))?;
                    stmt.query_map([b], history_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?
                }
                None => vec![head.clone()],
            };
            let now = db::now();
            for e in &targets {
                undo_entry(tx, e)?;
                tx.execute("UPDATE edit_history SET undone_at = ?1 WHERE id = ?2", params![now, e.id])?;
            }
            head.undone_at = Some(now);
            Ok(Some(head))
        })
    }

    // -------------------------------------------------------------------- audio

    /// Pasta da chamada relativa à raiz: `<cliente>/<call_…_slug>` (ou só `<call_…>` na inbox).
    pub fn call_dir_for(&self, key: &str, slug: &str, client_id: Option<i64>) -> Result<PathBuf> {
        let folder = if slug.is_empty() { key.to_string() } else { format!("{key}_{slug}") };
        if self.row.is_inbox() {
            return Ok(PathBuf::from(folder));
        }
        let parent = match client_id {
            Some(id) => self.conn.query_row("SELECT slug FROM clients WHERE id = ?1", [id], |r| r.get::<_, String>(0))?,
            None => UNASSIGNED_DIR.to_string(),
        };
        Ok(PathBuf::from(parent).join(folder))
    }

    /// Classifica a chamada num cliente desta biblioteca e move a pasta de áudio junto.
    pub fn set_client(&mut self, call_id: i64, client_id: Option<i64>) -> Result<()> {
        if self.row.is_inbox() && client_id.is_some() {
            return Err(Error::invalid("unclassified calls have no clients"));
        }
        if let Some(id) = client_id {
            self.conn
                .query_row("SELECT 1 FROM clients WHERE id = ?1", [id], |_| Ok(()))
                .optional()?
                .ok_or_else(|| Error::not_found(format!("client {id}")))?;
        }
        let (key, slug, dir): (String, String, Option<String>) = self
            .conn
            .query_row("SELECT key, slug, dir FROM calls WHERE id = ?1", [call_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?
            .ok_or_else(|| Error::not_found(format!("call {call_id}")))?;
        let new_dir = self.call_dir_for(&key, &slug, client_id)?;
        let moved = match dir.as_deref().map(PathBuf::from) {
            Some(old) if old != new_dir && self.root().join(&old).exists() => {
                fsx::move_path(&self.root().join(&old), &self.root().join(&new_dir))?;
                Some(old)
            }
            _ => None,
        };
        let res = self.edit(false, |tx| {
            tx.execute("UPDATE calls SET client_id = ?1 WHERE id = ?2", params![client_id, call_id])?;
            if let Some(old) = &moved {
                rebase_audio_paths(tx, call_id, old, &new_dir)?;
            }
            Ok(())
        });
        match (&res, &moved) {
            (Err(_), Some(old)) => {
                let _ = fsx::move_path(&self.root().join(&new_dir), &self.root().join(old));
            }
            (Ok(()), Some(old)) => {
                if let Some(parent) = self.root().join(old).parent() {
                    fsx::prune_empty_dirs(parent, self.root());
                }
            }
            _ => {}
        }
        res
    }

    pub fn audio_abs(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        db::checkpoint(&self.conn);
    }
}

/// Colunas de `blocks` na ordem de `block_from_row`.
const BLOCK_COLS: &str = "id, seq, t_start, t_end, speaker_id, text, original_text, edited_at IS NOT NULL, deleted_at";

fn block_from_row(r: &rusqlite::Row) -> rusqlite::Result<BlockInfo> {
    Ok(BlockInfo {
        id: r.get(0)?,
        seq: r.get(1)?,
        t_start: r.get(2)?,
        t_end: r.get(3)?,
        speaker_id: r.get(4)?,
        text: r.get(5)?,
        original_text: r.get(6)?,
        edited: r.get(7)?,
        deleted_at: r.get(8)?,
    })
}

/// Colunas de `edit_history` na ordem de `history_from_row`; `batch_size` = entradas do lote.
const HISTORY_COLS: &str = "id, call_id, entity, entity_id, old_value, new_value, origin, at, undone_at, batch_id, batch_kind,
    CASE WHEN batch_id IS NULL THEN NULL
         ELSE (SELECT count(*) FROM edit_history h WHERE h.batch_id = edit_history.batch_id) END";

fn history_from_row(r: &rusqlite::Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: r.get(0)?,
        call_id: r.get(1)?,
        entity: r.get(2)?,
        entity_id: r.get(3)?,
        old_value: r.get(4)?,
        new_value: r.get(5)?,
        origin: r.get(6)?,
        at: r.get(7)?,
        undone_at: r.get(8)?,
        batch_id: r.get(9)?,
        batch_kind: r.get(10)?,
        batch_size: r.get(11)?,
    })
}

/// Reverte o efeito de uma entrada (sem marcar `undone_at`).
fn undo_entry(tx: &Connection, e: &HistoryEntry) -> Result<()> {
    let old = e.old_value.clone();
    match e.entity.as_str() {
        "block_text" => {
            let original: String = tx.query_row("SELECT original_text FROM blocks WHERE id = ?1", [e.entity_id], |r| r.get(0))?;
            apply_block_text(tx, e.entity_id, old.as_deref().unwrap_or(&original), &original)?;
        }
        "block_speaker" => {
            let sid: i64 = old.as_deref().and_then(|s| s.parse().ok()).ok_or_else(|| Error::invalid("bad history"))?;
            tx.execute("UPDATE blocks SET speaker_id = ?1 WHERE id = ?2", params![sid, e.entity_id])?;
        }
        "call_title" => {
            tx.execute("UPDATE calls SET title = ?1 WHERE id = ?2", params![old.unwrap_or_default(), e.entity_id])?;
        }
        "speaker_name" => {
            tx.execute("UPDATE speakers SET name = ?1 WHERE id = ?2", params![old, e.entity_id])?;
        }
        // excluir e restaurar gravam o `deleted_at` de antes em `old_value`: desfazer é voltar a ele
        "block_deleted" => {
            tx.execute("UPDATE blocks SET deleted_at = ?1 WHERE id = ?2", params![old, e.entity_id])?;
        }
        // criar um corte grava o instante da criação em `old_value` ("antes estava removido"), remover grava NULL
        "audio_cut" => crate::cuts::undo_cut(tx, e.entity_id, old)?,
        other => return Err(Error::invalid(format!("cannot undo {other}"))),
    }
    Ok(())
}

pub(crate) fn read_block(tx: &Connection, block_id: i64) -> Result<BlockInfo> {
    Ok(tx.query_row(&format!("SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1"), [block_id], block_from_row)?)
}

pub(crate) fn apply_block_text(tx: &Connection, block_id: i64, new_text: &str, original: &str) -> Result<()> {
    let edited_at = (new_text != original).then(db::now);
    tx.execute("UPDATE blocks SET text = ?1, edited_at = ?2 WHERE id = ?3", params![new_text, edited_at, block_id])?;
    Ok(())
}

/// Lote de alterações desfeitas juntas (ver `HistoryEntry::batch_id`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Batch {
    pub id: i64,
    pub kind: &'static str,
}

/// Abre um lote novo; o id é único dentro da biblioteca.
pub(crate) fn new_batch(tx: &Connection, kind: &'static str) -> Result<Batch> {
    let id: i64 = tx.query_row("SELECT coalesce(max(batch_id), 0) + 1 FROM edit_history", [], |r| r.get(0))?;
    Ok(Batch { id, kind })
}

pub(crate) fn record(
    tx: &Connection,
    call_id: i64,
    entity: &str,
    entity_id: i64,
    old: Option<&str>,
    new: Option<&str>,
    origin: Origin,
) -> Result<i64> {
    record_in(tx, call_id, entity, entity_id, old, new, origin, None)
}

/// Grava uma entrada de histórico (opcionalmente num lote) e devolve o id dela.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_in(
    tx: &Connection,
    call_id: i64,
    entity: &str,
    entity_id: i64,
    old: Option<&str>,
    new: Option<&str>,
    origin: Origin,
    batch: Option<Batch>,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO edit_history (call_id, entity, entity_id, old_value, new_value, origin, at, batch_id, batch_kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![call_id, entity, entity_id, old, new, origin.as_str(), db::now(), batch.map(|b| b.id), batch.map(|b| b.kind)],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Troca o texto de um bloco e registra no histórico; `None` se o texto já era esse. É o único
/// caminho de escrita de texto de bloco (edição manual, glossário, importação).
pub(crate) fn write_block_text(
    tx: &Connection,
    call_id: i64,
    block_id: i64,
    new_text: &str,
    origin: Origin,
    batch: Option<Batch>,
) -> Result<Option<i64>> {
    let (old, original): (String, String) =
        tx.query_row("SELECT text, original_text FROM blocks WHERE id = ?1", [block_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if old == new_text {
        return Ok(None);
    }
    apply_block_text(tx, block_id, new_text, &original)?;
    Ok(Some(record_in(tx, call_id, "block_text", block_id, Some(&old), Some(new_text), origin, batch)?))
}

pub(crate) fn rebase_audio_paths(tx: &Connection, call_id: i64, old: &Path, new: &Path) -> Result<()> {
    let (mic, sys): (Option<String>, Option<String>) =
        tx.query_row("SELECT mic_path, sys_path FROM calls WHERE id = ?1", [call_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let rebase = |p: Option<String>| {
        p.map(|p| match Path::new(&p).strip_prefix(old) {
            Ok(rest) => new.join(rest).to_string_lossy().into_owned(),
            Err(_) => p,
        })
    };
    tx.execute(
        "UPDATE calls SET dir = ?1, mic_path = ?2, sys_path = ?3 WHERE id = ?4",
        params![new.to_string_lossy(), rebase(mic), rebase(sys), call_id],
    )?;
    Ok(())
}
