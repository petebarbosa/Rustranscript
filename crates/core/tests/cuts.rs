//! Cortes de áudio (#23) no banco: salvar cortes manuais (regra dos 50 %), excluir/restaurar trechos com cortes
//! ligados, remover cortes, desfazer de cada tipo de lote, migração da versão 5 e cópia ao mover a chamada.
//! Dados sintéticos; as trilhas só existem como caminhos (nada aqui lê áudio).
use core_lib::{App, Error, Library, Origin, db, schema, transfer};
use rusqlite::params;

/// Trechos: 1=[0,10) 2=[10,20) 3=[20,30) 4=[30,40) 5=[40,50), chamada de 100 s com as duas trilhas registradas.
fn fixture() -> (tempfile::TempDir, App, Library, i64, Vec<i64>) {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::open(&tmp.path().join("data")).unwrap();
    let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    lib.conn
        .execute_batch(
            "INSERT INTO calls (id, key, started_at, duration_s, mic_path, sys_path, created_at)
                VALUES (1, 'call_2026-07-01_09-00-00', '2026-07-01T09:00:00', 100, 'c/mic.flac', 'c/sys.flac', 't');
             INSERT INTO transcripts (id, call_id, version, created_at, is_active) VALUES (1, 1, 1, 't', 1);
             INSERT INTO speakers (id, call_id, track, label) VALUES (1, 1, 'sys', 'Pessoa 1');",
        )
        .unwrap();
    for (i, text) in ["abertura", "planilha", "orcamento", "proximos passos", "encerramento"].iter().enumerate() {
        lib.conn
            .execute(
                "INSERT INTO blocks (transcript_id, seq, t_start, t_end, speaker_id, original_text, text) VALUES (1, ?1, ?2, ?3, 1, ?4, ?4)",
                params![i as i64 + 1, i as f64 * 10.0, i as f64 * 10.0 + 10.0, text],
            )
            .unwrap();
    }
    let ids = (1..=5).map(|s| lib.block_id_by_seq(1, s).unwrap()).collect();
    (tmp, app, lib, 1, ids)
}

fn live(lib: &Library, call: i64) -> Vec<i64> {
    lib.call_detail(call, None).unwrap().blocks.iter().map(|b| b.seq).collect()
}

fn spans(lib: &Library, call: i64) -> Vec<(f64, f64)> {
    lib.cuts(call).unwrap().iter().map(|c| (c.t_start, c.t_end)).collect()
}

fn code<T>(r: Result<T, Error>) -> &'static str {
    r.err().expect("expected an error").code()
}

#[test]
fn saving_cuts_deletes_passages_with_at_least_half_covered() {
    let (_t, _app, mut lib, call, _) = fixture();
    // [15, 25): os trechos 2 e 3 têm exatamente 50 % dentro: saem (a metade conta). Os outros ficam.
    let r = lib.add_cuts(call, &[(15.0, 25.0)], Origin::Cli, false).unwrap();
    assert_eq!(r.added.len(), 1);
    assert_eq!(r.deleted_blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(live(&lib, call), [1, 4, 5]);
    assert_eq!(spans(&lib, call), [(15.0, 25.0)]);
    assert!(r.restored_blocks.is_empty() && r.skipped.is_empty());
}

#[test]
fn just_under_half_covered_keeps_the_passage() {
    let (_t, _app, mut lib, call, _) = fixture();
    // 15,001..24,999: cada um dos dois trechos fica com 4,999 de 10 (49,99 %)
    let r = lib.add_cuts(call, &[(15.001, 24.999)], Origin::Ui, false).unwrap();
    assert!(r.deleted_blocks.is_empty());
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
}

#[test]
fn the_union_of_all_cuts_decides_not_each_cut_alone() {
    let (_t, _app, mut lib, call, _) = fixture();
    // dois cortes de 3 s cada no trecho 2 (30 % cada): sozinhos não tiram nada, juntos somam 60 %
    lib.add_cuts(call, &[(10.0, 13.0)], Origin::Cli, false).unwrap();
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    let r = lib.add_cuts(call, &[(14.0, 17.0)], Origin::Cli, false).unwrap();
    assert_eq!(r.deleted_blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), [2]);
}

#[test]
fn dry_run_reports_the_same_count_and_changes_nothing() {
    let (_t, _app, mut lib, call, _) = fixture();
    let sim = lib.add_cuts(call, &[(15.0, 45.0)], Origin::Cli, true).unwrap();
    assert_eq!(sim.deleted_blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), [2, 3, 4, 5], "o 5 tem 5 de 10 dentro: a metade conta");
    assert_eq!(sim.added.len(), 1);
    assert_eq!(sim.cuts.len(), 1, "como ficaria");
    assert!(lib.cuts(call).unwrap().is_empty());
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    assert!(lib.history(Some(call), 10).unwrap().is_empty());
    let real = lib.add_cuts(call, &[(15.0, 45.0)], Origin::Cli, false).unwrap();
    assert_eq!(real.deleted_blocks.len(), sim.deleted_blocks.len());
}

#[test]
fn cuts_are_clamped_to_the_call_and_bad_input_is_refused() {
    let (_t, _app, mut lib, call, _) = fixture();
    lib.add_cuts(call, &[(-5.0, 2.0)], Origin::Cli, false).unwrap();
    lib.add_cuts(call, &[(95.0, 400.0)], Origin::Cli, false).unwrap();
    assert_eq!(spans(&lib, call), [(0.0, 2.0), (95.0, 100.0)]);
    assert_eq!(code(lib.add_cuts(call, &[(150.0, 160.0)], Origin::Cli, false)), "invalid", "fora da chamada");
    assert_eq!(code(lib.add_cuts(call, &[(30.0, 30.0)], Origin::Cli, false)), "invalid");
    assert_eq!(code(lib.add_cuts(call, &[(30.0, 20.0)], Origin::Cli, false)), "invalid");
    assert_eq!(code(lib.add_cuts(call, &[(f64::NAN, 20.0)], Origin::Cli, false)), "invalid");
    assert_eq!(code(lib.add_cuts(call, &[], Origin::Cli, false)), "invalid");
    assert_eq!(code(lib.add_cuts(999, &[(1.0, 2.0)], Origin::Cli, false)), "not_found");
}

#[test]
fn overlapping_requests_are_unioned_on_read_and_never_duplicated() {
    let (_t, _app, mut lib, call, _) = fixture();
    lib.add_cuts(call, &[(60.0, 70.0)], Origin::Cli, false).unwrap();
    // metade de dentro, metade de fora: só entra o que falta (a lista fica sem sobreposição)
    let r = lib.add_cuts(call, &[(65.0, 80.0)], Origin::Cli, false).unwrap();
    assert_eq!(r.added.iter().map(|c| (c.t_start, c.t_end)).collect::<Vec<_>>(), [(70.0, 80.0)]);
    // todo dentro: nada novo, nada no histórico
    let hist = lib.history(Some(call), 100).unwrap().len();
    let r = lib.add_cuts(call, &[(62.0, 68.0)], Origin::Cli, false).unwrap();
    assert!(r.added.is_empty() && r.skipped == [[62.0, 68.0]]);
    assert_eq!(lib.history(Some(call), 100).unwrap().len(), hist);
    // pedidos que se sobrepõem entre si viram um corte só
    let r = lib.add_cuts(call, &[(10.0, 12.0), (11.0, 14.0), (14.0, 15.0)], Origin::Cli, false).unwrap();
    assert_eq!(r.added.iter().map(|c| (c.t_start, c.t_end)).collect::<Vec<_>>(), [(10.0, 15.0)]);
    // a união lida pelo player e pelo worker é fundida, inclusive cortes que se tocam
    lib.add_cuts(call, &[(15.0, 16.0)], Origin::Cli, false).unwrap();
    assert_eq!(lib.effective_cuts(call).unwrap(), [(10.0, 16.0), (60.0, 80.0)]);
}

#[test]
fn saving_cuts_is_one_batch_with_the_deleted_passages_and_undo_reverts_it_whole() {
    let (_t, _app, mut lib, call, _) = fixture();
    lib.add_cuts(call, &[(15.0, 25.0), (35.0, 45.0)], Origin::Ui, false).unwrap();
    assert_eq!(live(&lib, call), [1]);
    let h = lib.history(Some(call), 100).unwrap();
    // dois cortes + trechos 2, 3, 4, 5 (o 4 e o 5 têm 5 de 10 dentro: 50 %)
    assert_eq!(h.len(), 2 + 4);
    assert!(h.iter().all(|e| e.batch_kind.as_deref() == Some("cut_add") && e.batch_id == h[0].batch_id && e.batch_size == Some(6)));
    assert_eq!(h.iter().filter(|e| e.entity == "audio_cut").count(), 2);
    assert_eq!(h.iter().filter(|e| e.entity == "block_deleted").count(), 4);
    let head = lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!(head.batch_size, Some(6));
    assert!(lib.cuts(call).unwrap().is_empty());
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    assert!(lib.effective_cuts(call).unwrap().is_empty());
    assert!(lib.undo(Some(call), false).unwrap().is_none(), "o lote inteiro saiu de uma vez");
}

#[test]
fn deleting_passages_creates_one_linked_cut_each_in_the_same_batch() {
    let (_t, _app, mut lib, call, ids) = fixture();
    let r = lib.delete_blocks(&[ids[1], ids[3]], Origin::Ui, false).unwrap();
    assert_eq!(r.changed.len(), 2);
    assert_eq!(r.cuts_added.len(), 2);
    assert_eq!(spans(&lib, call), [(10.0, 20.0), (30.0, 40.0)]);
    let cuts = lib.cuts(call).unwrap();
    assert_eq!(cuts.iter().map(|c| c.block_id).collect::<Vec<_>>(), [Some(ids[1]), Some(ids[3])]);
    assert_eq!(cuts.iter().map(|c| c.block_seq).collect::<Vec<_>>(), [Some(2), Some(4)], "ligado ao trecho da versão ativa");
    let h = lib.history(Some(call), 100).unwrap();
    assert_eq!(h.len(), 4);
    assert!(h.iter().all(|e| e.batch_kind.as_deref() == Some("delete") && e.batch_id == h[0].batch_id));
    // já excluído: não cria outro corte
    let again = lib.delete_blocks(&[ids[1]], Origin::Ui, false).unwrap();
    assert!(again.cuts_added.is_empty());
    assert_eq!(lib.cuts(call).unwrap().len(), 2);
    // simulação não grava
    let sim = lib.delete_blocks(&[ids[0]], Origin::Cli, true).unwrap();
    assert_eq!(sim.cuts_added.len(), 1);
    assert_eq!(lib.cuts(call).unwrap().len(), 2);
}

#[test]
fn restoring_passages_removes_their_cuts_in_the_same_batch_and_undo_brings_both_back() {
    let (_t, _app, mut lib, call, ids) = fixture();
    lib.delete_blocks(&[ids[1], ids[2]], Origin::Ui, false).unwrap();
    let r = lib.restore_blocks(&[ids[1]], Origin::Ui, false).unwrap();
    assert_eq!((r.changed.len(), r.cuts_removed.len()), (1, 1));
    assert_eq!(spans(&lib, call), [(20.0, 30.0)], "só o corte do trecho 3 sobrou");
    let h = lib.history(Some(call), 2).unwrap();
    assert!(h.iter().all(|e| e.batch_kind.as_deref() == Some("restore") && e.batch_size == Some(2)));
    // desfazer o restore: trecho excluído de novo e corte de volta
    lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!(live(&lib, call), [1, 4, 5]);
    assert_eq!(spans(&lib, call), [(10.0, 20.0), (20.0, 30.0)]);
    // desfazer a exclusão: tudo vivo, sem cortes
    lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    assert!(lib.cuts(call).unwrap().is_empty());
}

#[test]
fn deleting_without_audio_creates_no_cuts_and_still_restores() {
    let (_t, _app, mut lib, call, ids) = fixture();
    lib.conn.execute("UPDATE calls SET audio_deleted_at = 't' WHERE id = 1", []).unwrap();
    let r = lib.delete_blocks(&[ids[0]], Origin::Cli, false).unwrap();
    assert!(r.cuts_added.is_empty() && lib.cuts(call).unwrap().is_empty());
    assert_eq!(code(lib.add_cuts(call, &[(1.0, 2.0)], Origin::Cli, false)), "audio_deleted");
    lib.restore_blocks(&[ids[0]], Origin::Cli, false).unwrap();
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    lib.conn.execute("UPDATE calls SET audio_deleted_at = NULL, mic_path = NULL, sys_path = NULL WHERE id = 1", []).unwrap();
    assert_eq!(code(lib.add_cuts(call, &[(1.0, 2.0)], Origin::Cli, false)), "no_audio");
}

#[test]
fn removing_a_cut_restores_only_what_a_cut_save_deleted_and_is_now_under_half() {
    let (_t, _app, mut lib, call, ids) = fixture();
    // o usuário exclui o trecho 4 direto (com corte ligado) e salva um corte manual sobre os trechos 2-4
    lib.delete_blocks(&[ids[3]], Origin::Ui, false).unwrap();
    let r = lib.add_cuts(call, &[(10.0, 40.0)], Origin::Ui, false).unwrap();
    // o 4 já estava excluído: o manual não o "reclama" (e o corte de 30..40 já existia como ligado, mas o manual entra inteiro)
    assert_eq!(r.deleted_blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(live(&lib, call), [1, 5]);
    let manual = r.added[0].id;
    let r = lib.remove_cut(call, manual, Origin::Ui, false).unwrap();
    assert_eq!(r.restored_blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), [2, 3], "voltam os que o corte salvo tirou");
    assert_eq!(live(&lib, call), [1, 2, 3, 5], "o 4 foi excluído pelo usuário: nunca volta por aqui");
    assert_eq!(spans(&lib, call), [(30.0, 40.0)], "sobra só o corte ligado ao 4");
}

#[test]
fn removing_a_cut_while_the_rest_still_covers_half_keeps_the_passage_deleted() {
    let (_t, _app, mut lib, call, _) = fixture();
    // 40 % + 60 % do trecho 2, salvos em dois momentos: só o segundo salvamento o exclui
    let a = lib.add_cuts(call, &[(10.0, 14.0)], Origin::Cli, false).unwrap().added[0].id;
    let b = lib.add_cuts(call, &[(14.0, 20.0)], Origin::Cli, false).unwrap().added[0].id;
    assert_eq!(live(&lib, call), [1, 3, 4, 5]);
    // sem o primeiro, o segundo ainda cobre 60 %: continua excluído
    let r = lib.remove_cut(call, a, Origin::Cli, false).unwrap();
    assert!(r.restored_blocks.is_empty());
    assert_eq!(live(&lib, call), [1, 3, 4, 5]);
    // sem o segundo, 0 %: volta
    let r = lib.remove_cut(call, b, Origin::Cli, false).unwrap();
    assert_eq!(r.restored_blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), [2]);
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
}

#[test]
fn a_linked_cut_cannot_be_removed_directly_but_an_orphan_one_can() {
    let (_t, _app, mut lib, call, ids) = fixture();
    lib.delete_blocks(&[ids[1]], Origin::Ui, false).unwrap();
    let cut = lib.cuts(call).unwrap()[0].id;
    assert_eq!(code(lib.remove_cut(call, cut, Origin::Cli, false)), "conflict");
    assert_eq!(code(lib.remove_cut(call, 999, Origin::Cli, false)), "not_found");
    // depois de uma nova versão (o trecho ligado já não está na versão ativa) o corte pode sair como um manual
    lib.conn
        .execute_batch(
            "UPDATE transcripts SET is_active = 0 WHERE id = 1;
             INSERT INTO transcripts (id, call_id, version, created_at, is_active) VALUES (2, 1, 2, 't', 1);",
        )
        .unwrap();
    assert_eq!(lib.cuts(call).unwrap()[0].block_seq, None);
    lib.remove_cut(call, cut, Origin::Cli, false).unwrap();
    assert!(lib.cuts(call).unwrap().is_empty());
}

/// O motivo da exclusão sai do histórico e por isso acompanha o desfazer.
#[test]
fn the_deletion_reason_survives_undo() {
    let (_t, _app, mut lib, call, ids) = fixture();
    // 1) cortes salvos excluem o trecho 3; remover o corte o devolve
    let c = lib.add_cuts(call, &[(20.0, 30.0)], Origin::Ui, false).unwrap().added[0].id;
    assert_eq!(live(&lib, call), [1, 2, 4, 5]);
    lib.remove_cut(call, c, Origin::Ui, false).unwrap();
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    // 2) desfazer a remoção: o trecho volta a estar excluído PELO CORTE (e o corte volta)...
    lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!((live(&lib, call), spans(&lib, call)), (vec![1, 2, 4, 5], vec![(20.0, 30.0)]));
    // ...logo remover de novo o devolve de novo
    lib.remove_cut(call, c, Origin::Ui, false).unwrap();
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    // 3) o usuário restaura à mão o trecho de um corte salvo e depois o exclui direto: agora é exclusão direta
    lib.undo(Some(call), false).unwrap().unwrap(); // desfaz a remoção: trecho 3 excluído pelo corte
    lib.restore_blocks(&[ids[2]], Origin::Ui, false).unwrap();
    lib.delete_blocks(&[ids[2]], Origin::Ui, false).unwrap();
    let r = lib.remove_cut(call, c, Origin::Ui, false).unwrap();
    assert!(r.restored_blocks.is_empty(), "excluído direto pelo usuário: não volta");
    assert_eq!(live(&lib, call), [1, 2, 4, 5]);
    // 4) desfazendo exclusão direta + restauração, a razão volta a ser o corte salvo
    lib.undo(Some(call), false).unwrap().unwrap(); // desfaz a remoção do corte
    lib.undo(Some(call), false).unwrap().unwrap(); // desfaz a exclusão direta (trecho 3 vivo, cortes ligados fora)
    lib.undo(Some(call), false).unwrap().unwrap(); // desfaz a restauração: trecho 3 excluído pelo corte salvo de novo
    assert_eq!(live(&lib, call), [1, 2, 4, 5]);
    let r = lib.remove_cut(call, c, Origin::Ui, false).unwrap();
    assert_eq!(r.restored_blocks.iter().map(|b| b.seq).collect::<Vec<_>>(), [3]);
}

#[test]
fn undo_of_a_cut_removal_brings_cut_and_passages_back_together() {
    let (_t, _app, mut lib, call, _) = fixture();
    let c = lib.add_cuts(call, &[(10.0, 40.0)], Origin::Ui, false).unwrap().added[0].id;
    lib.remove_cut(call, c, Origin::Ui, false).unwrap();
    assert_eq!(live(&lib, call), [1, 2, 3, 4, 5]);
    let h = lib.history(Some(call), 1).unwrap();
    assert_eq!(h[0].batch_kind.as_deref(), Some("cut_remove"));
    lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!((live(&lib, call), spans(&lib, call)), (vec![1, 5], vec![(10.0, 40.0)]));
    lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!((live(&lib, call), spans(&lib, call)), (vec![1, 2, 3, 4, 5], vec![]));
}

#[test]
fn removing_with_dry_run_changes_nothing() {
    let (_t, _app, mut lib, call, _) = fixture();
    let c = lib.add_cuts(call, &[(10.0, 40.0)], Origin::Ui, false).unwrap().added[0].id;
    let sim = lib.remove_cut(call, c, Origin::Cli, true).unwrap();
    assert_eq!(sim.restored_blocks.len(), 3);
    assert_eq!((live(&lib, call), spans(&lib, call)), (vec![1, 5], vec![(10.0, 40.0)]));
}

#[test]
fn call_detail_lists_the_live_cuts() {
    let (_t, _app, mut lib, call, _) = fixture();
    lib.add_cuts(call, &[(70.0, 80.0)], Origin::Cli, false).unwrap();
    let d = lib.call_detail(call, None).unwrap();
    assert_eq!(d.cuts.len(), 1);
    assert_eq!((d.cuts[0].t_start, d.cuts[0].t_end, d.cuts[0].block_id), (70.0, 80.0, None));
}

/// Biblioteca criada na versão 5 do esquema, com dados, é migrada para a 6 sem perder nada.
#[test]
fn migration_from_schema_v5_keeps_data_and_adds_audio_cuts() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("library.db");
    {
        let conn = db::open(&path, &schema::LIBRARY_MIGRATIONS[..5]).unwrap();
        assert_eq!(conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 5);
        conn.execute_batch(
            "INSERT INTO calls (id, key, started_at, created_at) VALUES (1, 'call_2026-01-01_10-00-00', '2026-01-01T10:00:00', 't');
             INSERT INTO transcripts (id, call_id, version, created_at, is_active) VALUES (1, 1, 1, 't', 1);
             INSERT INTO speakers (id, call_id, track, label) VALUES (1, 1, 'sys', 'Pessoa 1');
             INSERT INTO blocks (id, transcript_id, seq, t_start, t_end, speaker_id, original_text, text, deleted_at) VALUES
                (1, 1, 1, 0, 1, 1, 'primeiro', 'primeiro', NULL), (2, 1, 2, 1, 2, 1, 'segundo', 'segundo', 'ontem');
             INSERT INTO edit_history (id, call_id, entity, entity_id, old_value, new_value, origin, at, batch_id, batch_kind)
                VALUES (4, 1, 'block_deleted', 2, NULL, 'ontem', 'ui', 't', 2, 'delete');",
        )
        .unwrap();
        // a CHECK da versão 5 recusa a entidade nova
        assert!(conn.execute("INSERT INTO edit_history (call_id, entity, entity_id, origin, at) VALUES (1, 'audio_cut', 1, 'ui', 't')", []).is_err());
        assert!(conn.prepare("SELECT * FROM audio_cuts").is_err());
    }
    let conn = db::open(&path, schema::LIBRARY_MIGRATIONS).unwrap();
    assert_eq!(conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), schema::LIBRARY_MIGRATIONS.len() as i64);
    assert!(schema::LIBRARY_MIGRATIONS.len() >= 6);
    // o que existia continua igual
    let h: (i64, String, i64, String) =
        conn.query_row("SELECT id, entity, batch_id, batch_kind FROM edit_history", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!(h, (4, "block_deleted".into(), 2, "delete".into()));
    let b: Vec<(i64, Option<String>)> = conn
        .prepare("SELECT seq, deleted_at FROM blocks ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(b, [(1, None), (2, Some("ontem".into()))]);
    let idx: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN ('edit_history_call', 'edit_history_batch', 'audio_cuts_call')", [], |r| r.get(0)).unwrap();
    assert_eq!(idx, 3);
    // a tabela nova e a entidade nova funcionam; as outras CHECKs continuam valendo
    conn.execute("INSERT INTO audio_cuts (call_id, t_start, t_end, created_at) VALUES (1, 1.0, 2.0, 't')", []).unwrap();
    assert!(conn.execute("INSERT INTO audio_cuts (call_id, t_start, t_end, created_at) VALUES (1, 2.0, 2.0, 't')", []).is_err(), "t_end > t_start");
    conn.execute("INSERT INTO edit_history (call_id, entity, entity_id, origin, at) VALUES (1, 'audio_cut', 1, 'ui', 't')", []).unwrap();
    assert!(conn.execute("INSERT INTO edit_history (call_id, entity, entity_id, origin, at) VALUES (1, 'outra_coisa', 1, 'ui', 't')", []).is_err());
    // apagar a chamada leva os cortes (cascata)
    conn.execute("DELETE FROM calls WHERE id = 1", []).unwrap();
    assert_eq!(conn.query_row::<i64, _, _>("SELECT count(*) FROM audio_cuts", [], |r| r.get(0)).unwrap(), 0);
}

#[test]
fn moving_a_call_to_another_library_carries_cuts_and_keeps_undo_working() {
    let (tmp, app, mut lib, call, ids) = fixture();
    lib.delete_blocks(&[ids[1]], Origin::Ui, false).unwrap();
    lib.add_cuts(call, &[(60.0, 70.0)], Origin::Ui, false).unwrap();
    let inbox_id = lib.id();
    drop(lib);
    let company = app.add_library("Empresa Sintetica", &tmp.path().join("Empresa")).unwrap();
    // o teste não tem arquivos de áudio: a pasta da chamada fica vazia
    let (lib_id, new_call) = transfer::assign(&app, inbox_id, call, company.id, None).unwrap();
    let mut lib = app.open_library(lib_id).unwrap();
    let cuts = lib.cuts(new_call).unwrap();
    assert_eq!(cuts.iter().map(|c| (c.t_start, c.t_end)).collect::<Vec<_>>(), [(10.0, 20.0), (60.0, 70.0)]);
    let linked = cuts.iter().find(|c| c.t_start == 10.0).unwrap();
    assert_eq!(linked.block_seq, Some(2), "o corte ligado aponta para o trecho novo do destino");
    // desfazer funciona no destino: primeiro o corte manual, depois a exclusão (com o corte ligado)
    lib.undo(Some(new_call), false).unwrap().unwrap();
    assert_eq!(lib.cuts(new_call).unwrap().len(), 1);
    lib.undo(Some(new_call), false).unwrap().unwrap();
    assert!(lib.cuts(new_call).unwrap().is_empty());
    assert_eq!(lib.call_detail(new_call, None).unwrap().blocks.len(), 5);
}
