//! Busca global (FTS5) em todas as bibliotecas cadastradas, só na versão ativa de cada chamada.
use rusqlite::params;

use crate::app::App;
use crate::library::{self, Library};
use crate::model::SearchHit;
use crate::{Result, text};

pub const MARK_START: char = '\u{2}';
pub const MARK_END: char = '\u{3}';

pub fn search(app: &App, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let Some(q) = text::fts_query(query) else { return Ok(vec![]) };
    let mut hits = Vec::new();
    for row in app.library_rows()? {
        if !row.root.join(library::DB_FILE).is_file() {
            continue;
        }
        let lib = Library::open(row)?;
        hits.extend(search_library(&lib, &q, limit)?);
    }
    // acertos no título primeiro; depois trechos por bm25 (quanto menor, melhor).
    // bm25 de tabelas diferentes não é comparável, por isso os grupos não se misturam.
    hits.sort_by(|a, b| {
        (a.block_id.is_some(), a.rank)
            .partial_cmp(&(b.block_id.is_some(), b.rank))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.started_at.cmp(&a.started_at))
    });
    hits.truncate(limit);
    Ok(hits)
}

fn search_library(lib: &Library, fts: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let mut out = Vec::new();
    let marks = (MARK_START.to_string(), MARK_END.to_string());
    let mut stmt = lib.conn.prepare(
        "SELECT c.id, c.key, c.title, c.started_at, b.id, b.t_start,
                snippet(blocks_fts, 0, ?2, ?3, '…', 14), bm25(blocks_fts)
         FROM blocks_fts
         JOIN blocks b ON b.id = blocks_fts.rowid
         JOIN transcripts t ON t.id = b.transcript_id AND t.is_active = 1
         JOIN calls c ON c.id = t.call_id
         WHERE blocks_fts MATCH ?1
         ORDER BY bm25(blocks_fts) LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![fts, marks.0, marks.1, limit as i64], |r| {
        Ok(SearchHit {
            library_id: lib.id(),
            call_id: r.get(0)?,
            call_key: r.get(1)?,
            call_title: r.get(2)?,
            started_at: r.get(3)?,
            block_id: Some(r.get(4)?),
            t_start: Some(r.get(5)?),
            snippet: r.get(6)?,
            rank: r.get(7)?,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    let mut stmt = lib.conn.prepare(
        "SELECT c.id, c.key, c.title, c.started_at, highlight(calls_fts, 0, ?2, ?3), bm25(calls_fts)
         FROM calls_fts JOIN calls c ON c.id = calls_fts.rowid
         WHERE calls_fts MATCH ?1 ORDER BY bm25(calls_fts) LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![fts, marks.0, marks.1, limit as i64], |r| {
        Ok(SearchHit {
            library_id: lib.id(),
            call_id: r.get(0)?,
            call_key: r.get(1)?,
            call_title: r.get(2)?,
            started_at: r.get(3)?,
            block_id: None,
            t_start: None,
            snippet: r.get(4)?,
            rank: r.get(5)?,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
