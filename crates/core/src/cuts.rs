//! Cortes de áudio (issue #23): tangentes que saem do texto e do áudio ouvido sem tocar nos FLACs.
//!
//! Um corte é um intervalo `[t_start, t_end)` na linha do tempo original da chamada (a mesma dos blocos), válido
//! para mic e sys. Os cortes ficam em `audio_cuts`; cortes que se sobrepõem ou se tocam NÃO são fundidos na
//! escrita, a união é feita na leitura (`effective_cuts`): cada linha mantém a própria identidade (desfazer, o
//! trecho a que está ligada). Cortes MANUAIS novos só ocupam o que os manuais já salvos não cobrem (a lista
//! fica sem sobreposição); um pedido que já está todo coberto vira `skipped`.
//!
//! Regras, todas dentro de UM lote do histórico (`tary undo` desfaz o lote inteiro):
//! - salvar cortes manuais (`cut_add`): grava os cortes e exclui todo trecho vivo da versão ativa com pelo menos
//!   metade da duração dentro da união de TODOS os cortes (`MIN_COVERAGE`);
//! - excluir trechos (`delete`): cria um corte por trecho, ligado a ele; restaurar (`restore`) remove os cortes
//!   ligados (quem chama é `Library::set_blocks_deleted`);
//! - remover um corte manual (`cut_remove`): devolve os trechos que foram excluídos POR UM corte salvo e agora
//!   ficam com menos da metade coberta; nunca os que o usuário excluiu direto.
//!
//! O motivo de um trecho estar excluído não é coluna: sai do histórico (`deleted_by_cut`), a última entrada
//! 'block_deleted' não desfeita do trecho tem `batch_kind = 'cut_add'`. Como `undo` marca `undone_at`, o motivo
//! acompanha o desfazer sem estado extra.
use rusqlite::{Connection, OptionalExtension, params};

use crate::library::{Library, new_batch, read_block, record_in};
use crate::model::{AudioCut, BlockInfo, CutsChange, Origin};
use crate::{Error, Result, db};

/// Parte mínima da duração de um trecho dentro dos cortes para ele sair junto (a metade conta).
pub const MIN_COVERAGE: f64 = 0.5;
const EPS: f64 = 1e-9;

pub type Span = (f64, f64);

/// União: ordena e funde intervalos que se sobrepõem ou se tocam; descarta os vazios.
pub fn merge(spans: &[Span]) -> Vec<Span> {
    let mut v: Vec<Span> = spans.iter().copied().filter(|&(s, e)| e > s).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<Span> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Quanto de `[a, b)` cai dentro de `merged` (união já fundida).
pub fn covered_s(merged: &[Span], a: f64, b: f64) -> f64 {
    merged.iter().map(|&(s, e)| (e.min(b) - s.max(a)).max(0.0)).sum()
}

/// O trecho `[t_start, t_end]` tem pelo menos `MIN_COVERAGE` dentro de `merged`? Trecho sem duração conta se o
/// instante dele cai em um corte.
pub fn is_covered(merged: &[Span], t_start: f64, t_end: f64) -> bool {
    if t_end > t_start {
        covered_s(merged, t_start, t_end) >= (t_end - t_start) * MIN_COVERAGE - EPS
    } else {
        merged.iter().any(|&(s, e)| s <= t_start && t_start < e)
    }
}

/// `span` menos a união `merged`: os pedaços que sobram (em ordem).
fn subtract(merged: &[Span], span: Span) -> Vec<Span> {
    let mut out = Vec::new();
    let mut from = span.0;
    for &(s, e) in merged {
        if e <= from || s >= span.1 {
            continue;
        }
        if s > from {
            out.push((from, s));
        }
        from = from.max(e);
    }
    if from < span.1 {
        out.push((from, span.1));
    }
    out
}

fn round_ms(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Por que a chamada não aceita cortes: sem áudio (apagado, ou nunca houve).
fn audio_problem(conn: &Connection, call_id: i64) -> Result<Option<Error>> {
    let (mic, sys, deleted): (Option<String>, Option<String>, Option<String>) = conn
        .query_row("SELECT mic_path, sys_path, audio_deleted_at FROM calls WHERE id = ?1", [call_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?
        .ok_or_else(|| Error::not_found(format!("call {call_id}")))?;
    Ok(if deleted.is_some() {
        Some(Error::transcription("audio_deleted", format!("call {call_id}")))
    } else if mic.is_none() && sys.is_none() {
        Some(Error::transcription("no_audio", format!("call {call_id} has no audio")))
    } else {
        None
    })
}

/// Fim da chamada para limitar cortes: a maior entre a duração gravada (inteira, arredondada) e o fim do último
/// trecho; sem nenhuma das duas, sem limite.
fn call_limit(conn: &Connection, call_id: i64) -> Result<f64> {
    let (dur, last): (i64, f64) = conn.query_row(
        "SELECT c.duration_s, coalesce((SELECT max(b.t_end) FROM blocks b JOIN transcripts t ON t.id = b.transcript_id WHERE t.call_id = c.id), 0)
         FROM calls c WHERE c.id = ?1",
        [call_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let limit = (dur as f64).max(last);
    Ok(if limit > 0.0 { limit } else { f64::INFINITY })
}

const CUT_COLS: &str = "c.id, c.t_start, c.t_end, c.block_id, c.created_at,
    (SELECT b.seq FROM blocks b JOIN transcripts t ON t.id = b.transcript_id AND t.is_active = 1 WHERE b.id = c.block_id)";

fn cut_from_row(r: &rusqlite::Row) -> rusqlite::Result<AudioCut> {
    Ok(AudioCut { id: r.get(0)?, t_start: r.get(1)?, t_end: r.get(2)?, block_id: r.get(3)?, created_at: r.get(4)?, block_seq: r.get(5)? })
}

/// Cortes em uso da chamada, por início.
fn live_cuts(conn: &Connection, call_id: i64) -> Result<Vec<AudioCut>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {CUT_COLS} FROM audio_cuts c WHERE c.call_id = ?1 AND c.removed_at IS NULL ORDER BY c.t_start, c.id"))?;
    let rows = stmt.query_map([call_id], cut_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn spans_of(cuts: &[AudioCut]) -> Vec<Span> {
    cuts.iter().map(|c| (c.t_start, c.t_end)).collect()
}

fn cut_by_id(conn: &Connection, id: i64) -> Result<AudioCut> {
    Ok(conn.query_row(&format!("SELECT {CUT_COLS} FROM audio_cuts c WHERE c.id = ?1"), [id], cut_from_row)?)
}

/// Pedido do usuário → intervalos válidos, dentro de `[0, limit]`, fundidos entre si.
fn normalize(spans: &[Span], limit: f64) -> Result<Vec<Span>> {
    if spans.is_empty() {
        return Err(Error::invalid("no cuts given"));
    }
    let mut out = Vec::with_capacity(spans.len());
    for &(s, e) in spans {
        if !s.is_finite() || !e.is_finite() || e <= s {
            return Err(Error::invalid(format!("invalid cut: start {s} must be before end {e}")));
        }
        let (s, e) = (round_ms(s.max(0.0)), round_ms(e.min(limit)));
        if e > s {
            out.push((s, e));
        }
    }
    if out.is_empty() {
        return Err(Error::invalid(format!("cut outside the call (0 to {limit} s)")));
    }
    Ok(merge(&out))
}

/// Trechos da versão ativa: `(vivos, excluídos)`.
fn active_blocks(conn: &Connection, call_id: i64) -> Result<(Vec<BlockInfo>, Vec<BlockInfo>)> {
    let ids: Vec<(i64, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT b.id, b.deleted_at IS NOT NULL FROM blocks b JOIN transcripts t ON t.id = b.transcript_id AND t.is_active = 1
             WHERE t.call_id = ?1 ORDER BY b.seq",
        )?;
        stmt.query_map([call_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let (mut live, mut dead) = (Vec::new(), Vec::new());
    for (id, deleted) in ids {
        let b = read_block(conn, id)?;
        if deleted { dead.push(b) } else { live.push(b) }
    }
    Ok((live, dead))
}

/// O trecho (excluído agora) saiu por um corte salvo? Olha a última entrada 'block_deleted' não desfeita dele.
fn deleted_by_cut(conn: &Connection, block_id: i64) -> Result<bool> {
    let last: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT new_value, batch_kind FROM edit_history
             WHERE entity = 'block_deleted' AND entity_id = ?1 AND undone_at IS NULL ORDER BY id DESC LIMIT 1",
            [block_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(matches!(last, Some((Some(_), Some(kind))) if kind == "cut_add"))
}

fn insert_cut(tx: &Connection, call_id: i64, span: Span, block_id: Option<i64>, origin: Origin, batch: crate::library::Batch) -> Result<AudioCut> {
    let now = db::now();
    tx.execute(
        "INSERT INTO audio_cuts (call_id, t_start, t_end, block_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![call_id, span.0, span.1, block_id, now],
    )?;
    let id = tx.last_insert_rowid();
    // criar = "antes estava removido": desfazer volta a este valor (ver migração 6)
    record_in(tx, call_id, "audio_cut", id, Some(&now), None, origin, Some(batch))?;
    cut_by_id(tx, id)
}

fn retire_cut(tx: &Connection, call_id: i64, cut: &AudioCut, origin: Origin, batch: crate::library::Batch) -> Result<()> {
    let now = db::now();
    tx.execute("UPDATE audio_cuts SET removed_at = ?1 WHERE id = ?2", params![now, cut.id])?;
    record_in(tx, call_id, "audio_cut", cut.id, None, Some(&now), origin, Some(batch))?;
    Ok(())
}

/// Excluir um trecho corta o áudio dele: um corte ligado ao trecho, no mesmo lote. Sem áudio não há o que cortar.
pub(crate) fn link_block_cut(tx: &Connection, call_id: i64, block: &BlockInfo, origin: Origin, batch: crate::library::Batch) -> Result<Option<AudioCut>> {
    if audio_problem(tx, call_id)?.is_some() {
        return Ok(None);
    }
    let limit = call_limit(tx, call_id)?;
    let span = (block.t_start.max(0.0), block.t_end.min(limit));
    let linked: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM audio_cuts WHERE block_id = ?1 AND removed_at IS NULL)",
        [block.id],
        |r| r.get(0),
    )?;
    if span.1 <= span.0 || linked {
        return Ok(None);
    }
    Ok(Some(insert_cut(tx, call_id, span, Some(block.id), origin, batch)?))
}

/// Restaurar um trecho tira os cortes ligados a ele, no mesmo lote.
pub(crate) fn unlink_block_cuts(tx: &Connection, call_id: i64, block_id: i64, origin: Origin, batch: crate::library::Batch) -> Result<Vec<AudioCut>> {
    let cuts: Vec<AudioCut> = {
        let mut stmt = tx.prepare(&format!("SELECT {CUT_COLS} FROM audio_cuts c WHERE c.block_id = ?1 AND c.removed_at IS NULL ORDER BY c.id"))?;
        stmt.query_map([block_id], cut_from_row)?.collect::<rusqlite::Result<_>>()?
    };
    for c in &cuts {
        retire_cut(tx, call_id, c, origin, batch)?;
    }
    Ok(cuts)
}

/// Desfazer uma entrada 'audio_cut': volta o `removed_at` de antes (ver `undo_entry`).
pub(crate) fn undo_cut(tx: &Connection, cut_id: i64, old: Option<String>) -> Result<()> {
    tx.execute("UPDATE audio_cuts SET removed_at = ?1 WHERE id = ?2", params![old, cut_id])?;
    Ok(())
}

impl Library {
    /// Cortes em uso da chamada (os da lista), por início.
    pub fn cuts(&self, call_id: i64) -> Result<Vec<AudioCut>> {
        self.call_exists(call_id)?;
        live_cuts(&self.conn, call_id)
    }

    /// A união de todos os cortes em uso, em segundos, fundida e ordenada: o que o player pula e o que o
    /// worker zera.
    pub fn effective_cuts(&self, call_id: i64) -> Result<Vec<Span>> {
        Ok(merge(&spans_of(&live_cuts(&self.conn, call_id)?)))
    }

    /// Salva cortes manuais e exclui os trechos que eles cobrem (metade ou mais), num lote só. Com `dry_run`
    /// (ou antes de confirmar na UI) devolve o mesmo resultado sem gravar: `deleted_blocks` é a contagem a mostrar.
    pub fn add_cuts(&mut self, call_id: i64, spans: &[Span], origin: Origin, dry_run: bool) -> Result<CutsChange> {
        self.call_exists(call_id)?;
        self.edit(dry_run, |tx| {
            if let Some(e) = audio_problem(tx, call_id)? {
                return Err(e);
            }
            let wanted = normalize(spans, call_limit(tx, call_id)?)?;
            let manual = merge(&spans_of(&live_cuts(tx, call_id)?.into_iter().filter(|c| c.block_id.is_none()).collect::<Vec<_>>()));
            let (mut fresh, mut skipped) = (Vec::new(), Vec::new());
            for w in wanted {
                let pieces = subtract(&manual, w);
                if pieces.is_empty() {
                    skipped.push([w.0, w.1]);
                }
                fresh.extend(pieces);
            }
            let mut out = CutsChange { call_id, added: vec![], removed: vec![], skipped, deleted_blocks: vec![], restored_blocks: vec![], cuts: vec![] };
            if !fresh.is_empty() {
                let batch = new_batch(tx, "cut_add")?;
                for span in fresh {
                    out.added.push(insert_cut(tx, call_id, span, None, origin, batch)?);
                }
                let all = merge(&spans_of(&live_cuts(tx, call_id)?));
                let now = db::now();
                for b in active_blocks(tx, call_id)?.0 {
                    if is_covered(&all, b.t_start, b.t_end) {
                        tx.execute("UPDATE blocks SET deleted_at = ?1 WHERE id = ?2", params![now, b.id])?;
                        record_in(tx, call_id, "block_deleted", b.id, None, Some(&now), origin, Some(batch))?;
                        out.deleted_blocks.push(read_block(tx, b.id)?);
                    }
                }
            }
            out.cuts = live_cuts(tx, call_id)?;
            Ok(out)
        })
    }

    /// Remove um corte manual e devolve os trechos que ele (sozinho) mantinha excluídos. Corte ligado a um
    /// trecho da versão ativa não sai por aqui: restaure o trecho.
    pub fn remove_cut(&mut self, call_id: i64, cut_id: i64, origin: Origin, dry_run: bool) -> Result<CutsChange> {
        self.call_exists(call_id)?;
        self.edit(dry_run, |tx| {
            let cut = live_cuts(tx, call_id)?.into_iter().find(|c| c.id == cut_id).ok_or_else(|| Error::not_found(format!("cut {cut_id}")))?;
            if let Some(seq) = cut.block_seq {
                return Err(Error::Conflict(format!("cut {cut_id} belongs to passage #{seq}: restore the passage to remove it")));
            }
            let batch = new_batch(tx, "cut_remove")?;
            retire_cut(tx, call_id, &cut, origin, batch)?;
            let rest = live_cuts(tx, call_id)?;
            let all = merge(&spans_of(&rest));
            let mut out = CutsChange { call_id, added: vec![], removed: vec![cut], skipped: vec![], deleted_blocks: vec![], restored_blocks: vec![], cuts: rest };
            for b in active_blocks(tx, call_id)?.1 {
                if !is_covered(&all, b.t_start, b.t_end) && deleted_by_cut(tx, b.id)? {
                    tx.execute("UPDATE blocks SET deleted_at = NULL WHERE id = ?1", [b.id])?;
                    record_in(tx, call_id, "block_deleted", b.id, b.deleted_at.as_deref(), None, origin, Some(batch))?;
                    out.restored_blocks.push(read_block(tx, b.id)?);
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_sorts_joins_overlapping_and_touching_spans() {
        assert_eq!(merge(&[(5.0, 6.0), (1.0, 2.0), (1.5, 3.0), (3.0, 4.0)]), vec![(1.0, 4.0), (5.0, 6.0)]);
        assert!(merge(&[]).is_empty());
    }

    #[test]
    fn coverage_counts_the_union_and_the_half_boundary_is_inclusive() {
        let m = merge(&[(15.0, 25.0)]);
        assert_eq!(covered_s(&m, 10.0, 20.0), 5.0);
        assert!(is_covered(&m, 10.0, 20.0), "exatamente 50 %");
        assert!(is_covered(&m, 20.0, 30.0));
        assert!(!is_covered(&merge(&[(15.5, 24.5)]), 10.0, 20.0), "45 %");
        assert!(!is_covered(&m, 0.0, 10.0));
        // duração zero: vale o instante
        assert!(is_covered(&m, 16.0, 16.0) && !is_covered(&m, 26.0, 26.0));
    }

    #[test]
    fn subtract_trims_a_span_by_the_existing_union() {
        let m = merge(&[(2.0, 4.0), (6.0, 8.0)]);
        assert_eq!(subtract(&m, (1.0, 9.0)), vec![(1.0, 2.0), (4.0, 6.0), (8.0, 9.0)]);
        assert!(subtract(&m, (2.5, 3.5)).is_empty());
        assert_eq!(subtract(&m, (3.0, 5.0)), vec![(4.0, 5.0)]);
    }

    #[test]
    fn normalize_clamps_rounds_and_rejects_bad_spans() {
        assert_eq!(normalize(&[(-3.0, 1.2344), (50.0, 200.0)], 100.0).unwrap(), vec![(0.0, 1.234), (50.0, 100.0)]);
        assert!(normalize(&[(1.0, 1.0)], 100.0).is_err());
        assert!(normalize(&[(200.0, 300.0)], 100.0).is_err());
        assert!(normalize(&[(f64::INFINITY, 1.0)], 100.0).is_err());
        assert!(normalize(&[(1.0, 2.0)], f64::INFINITY).is_ok(), "sem duração conhecida não limita");
    }
}
