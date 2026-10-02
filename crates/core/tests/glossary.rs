//! Glossário de ponta a ponta no núcleo, só com dados sintéticos.
use std::path::{Path, PathBuf};

use core_lib::import::{self, ImportOptions};
use core_lib::model::{RuleKind, Scope};
use core_lib::rules::{ImportScope, RuleInput};
use core_lib::{App, Error, Library, Origin, db, schema, transfer};

const FIXTURES_KEY: &str = "call_2026-02-01_14-30-00";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Texto de `call_*.txt` com falantes alternados (cada fala vira um bloco).
fn write_call(dir: &Path, stem: &str, lines: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let body: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("[00:{:02}:00] {}: {l}", i + 1, if i % 2 == 0 { "Outros" } else { "Eu" }))
        .collect();
    std::fs::write(dir.join(format!("{stem}.txt")), body.join("\n") + "\n").unwrap();
}

fn import_dir(app: &App, src: &Path, opts: &ImportOptions) -> import::ImportReport {
    let cands = import::scan(&[src.to_path_buf()]).unwrap();
    import::import(app, &cands, opts, &mut |_| {}).unwrap()
}

fn plain() -> ImportOptions {
    ImportOptions { convert_audio: false, ..Default::default() }
}

struct Env {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    src: PathBuf,
    company: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    Env { data: tmp.path().join("data"), src: tmp.path().join("src"), company: tmp.path().join("Empresa"), _tmp: tmp }
}

fn texts(lib: &Library, call: i64) -> Vec<String> {
    let tid = lib.active_transcript_id(call).unwrap().unwrap();
    lib.blocks(tid).unwrap().into_iter().map(|b| b.text).collect()
}

#[test]
fn v1_databases_upgrade_cleanly() {
    let e = env();
    // app.db e library.db criados como na fase 1 (só a migração 1), com dados dentro
    std::fs::create_dir_all(&e.data).unwrap();
    {
        let conn = db::open(&e.data.join("app.db"), &schema::APP_MIGRATIONS[..1]).unwrap();
        conn.execute(
            "INSERT INTO glossary_global (kind, pattern, replacement, case_sensitive, created_at) VALUES ('replace', 'gate', 'gateway', 0, 't')",
            [],
        )
        .unwrap();
        assert_eq!(conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
    std::fs::create_dir_all(&e.company).unwrap();
    {
        let conn = db::open(&e.company.join("library.db"), &schema::LIBRARY_MIGRATIONS[..1]).unwrap();
        conn.execute("INSERT INTO clients (name, slug, created_at) VALUES ('Cliente', 'cliente', 't')", []).unwrap();
        conn.execute(
            "INSERT INTO calls (key, title, started_at, created_at) VALUES ('call_2026-01-01_10-00-00', 'x', '2026-01-01T10:00:00', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edit_history (call_id, entity, entity_id, old_value, new_value, origin, at) VALUES (1, 'call_title', 1, 'a', 'b', 'ui', 't')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO glossary_client (client_id, kind, pattern, created_at) VALUES (1, 'term', 'Kafka', 't')", []).unwrap();
    }

    let app = App::open(&e.data).unwrap();
    assert_eq!(app.db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 2);
    let g = app.global_rules().unwrap();
    assert_eq!((g.len(), g[0].pattern.as_str(), g[0].source_edit_id), (1, "gate", None));

    let row = app.add_library("Empresa", &e.company).unwrap();
    let lib = app.open_library(row.id).unwrap();
    assert_eq!(lib.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 3);
    // v3 (fase 3): chamadas que já existiam ficam com a transcrição "done"
    assert_eq!(lib.call_summary(1).unwrap().transcription_state, "done");
    let h = lib.history(Some(1), 10).unwrap();
    assert_eq!((h.len(), h[0].batch_id, h[0].batch_kind.clone(), h[0].batch_size), (1, None, None, None));
    assert_eq!(lib.client_rules(1).unwrap()[0].pattern, "Kafka");
    // reabrir não migra de novo
    drop(lib);
    assert!(app.open_library(row.id).is_ok());
    // e uma regra nova funciona nas tabelas migradas
    app.add_global_rule(&RuleInput::term("Kubernetes"), None).unwrap();
}

#[test]
fn rules_crud_validation_and_two_layer_precedence() {
    let e = env();
    let app = App::open(&e.data).unwrap();
    let company = app.add_library("Empresa", &e.company).unwrap();
    let mut lib = app.open_library(company.id).unwrap();
    let client = lib.add_client("Cliente Alfa").unwrap();
    let other = lib.add_client("Cliente Beta").unwrap();

    // validação
    assert!(matches!(app.add_global_rule(&RuleInput::term("  "), None), Err(Error::Invalid(_))));
    assert!(matches!(app.add_global_rule(&RuleInput::replace("a", "a"), None), Err(Error::Invalid(_))));
    assert!(matches!(app.add_global_rule(&RuleInput::replace("a", ""), None), Err(Error::Invalid(_))));

    let g = app.add_global_rule(&RuleInput::replace("Gate  Wei", "Gateway"), None).unwrap();
    assert_eq!((g.scope, g.pattern.as_str(), g.library_id), (Scope::Global, "Gate Wei", None));
    // duplicata na mesma camada: conflito (caixa/espaços não importam); outro tipo é outra regra
    for dup in ["gate wei", " GATE   WEI"] {
        assert!(matches!(app.add_global_rule(&RuleInput::replace(dup, "x"), None), Err(Error::Conflict(_))));
    }
    app.add_global_rule(&RuleInput::term("Gate Wei"), None).unwrap();
    let t = app.add_global_rule(&RuleInput::term("Kubernetes"), None).unwrap();

    // atualizar: não conflita consigo mesmo, conflita com as outras
    let g2 = app.update_global_rule(g.id, &RuleInput { case_sensitive: true, ..RuleInput::replace("gate wei", "Gateway API") }).unwrap();
    assert_eq!((g2.pattern.as_str(), g2.replacement.as_deref(), g2.case_sensitive), ("gate wei", Some("Gateway API"), true));
    assert!(matches!(app.update_global_rule(t.id, &RuleInput::term("gate wei")), Err(Error::Conflict(_))));
    assert!(matches!(app.update_global_rule(999, &RuleInput::term("zzz")), Err(Error::NotFound(_))));
    app.update_global_rule(g.id, &RuleInput::replace("gate wei", "Gateway")).unwrap();

    // cliente sobrescreve global; outro cliente e a inbox só veem a global
    let c = lib.add_client_rule(client.id, &RuleInput::replace("GATE WEI", "Gate Way"), None).unwrap();
    assert_eq!((c.scope, c.library_id, c.client_id), (Scope::Client, Some(company.id), Some(client.id)));
    assert!(matches!(lib.add_client_rule(client.id, &RuleInput::replace("gate wei", "x"), None), Err(Error::Conflict(_))));
    assert!(matches!(lib.add_client_rule(9999, &RuleInput::term("x"), None), Err(Error::NotFound(_))));
    let merged = app.merged_rules(&lib, Some(client.id)).unwrap();
    let view: Vec<_> = merged.iter().map(|r| (r.scope, r.kind, r.pattern.as_str(), r.overridden)).collect();
    assert_eq!(
        view,
        [
            (Scope::Client, RuleKind::Replace, "GATE WEI", false),
            (Scope::Global, RuleKind::Replace, "gate wei", true),
            (Scope::Global, RuleKind::Term, "Gate Wei", false),
            (Scope::Global, RuleKind::Term, "Kubernetes", false),
        ]
    );
    assert_eq!(app.effective_rules(&lib, Some(other.id)).unwrap().len(), 3);
    assert!(app.effective_rules(&lib, Some(other.id)).unwrap().iter().all(|r| r.scope == Scope::Global));
    let inbox = app.open_library(app.inbox_id().unwrap()).unwrap();
    assert!(app.merged_rules(&inbox, None).unwrap().iter().all(|r| r.scope == Scope::Global));
    // contexto sem cliente numa biblioteca de empresa: só globais
    assert!(app.merged_rules(&lib, None).unwrap().iter().all(|r| r.scope == Scope::Global));

    // atualizar e remover regras de cliente
    let c2 = lib.update_client_rule(c.id, &RuleInput::replace("gate wei", "Gate-Way")).unwrap();
    assert_eq!(c2.replacement.as_deref(), Some("Gate-Way"));
    let removed = lib.remove_client_rule(c.id).unwrap();
    assert_eq!(removed.id, c.id);
    assert!(matches!(lib.remove_client_rule(c.id), Err(Error::NotFound(_))));
    assert!(app.merged_rules(&lib, Some(client.id)).unwrap().iter().all(|r| !r.overridden));

    // promover: cliente → global, sem a cópia no cliente
    let c3 = lib.add_client_rule(client.id, &RuleInput::replace("Zenit", "Zenith"), Some(77)).unwrap();
    let promoted = app.promote_rule(&lib, c3.id).unwrap();
    assert_eq!((promoted.scope, promoted.pattern.as_str(), promoted.source_edit_id, promoted.source_library_id), (Scope::Global, "Zenit", Some(77), Some(company.id)));
    assert!(lib.client_rules(client.id).unwrap().is_empty());
    // global idêntica já existe: só limpa a cópia; com substituição diferente: conflito
    lib.add_client_rule(client.id, &RuleInput::replace("zenit", "Zenith"), None).unwrap();
    let again = app.promote_rule(&lib, lib.client_rules(client.id).unwrap()[0].id).unwrap();
    assert_eq!(again.id, promoted.id);
    assert!(lib.client_rules(client.id).unwrap().is_empty());
    let diff = lib.add_client_rule(client.id, &RuleInput::replace("zenit", "Outro"), None).unwrap();
    assert!(matches!(app.promote_rule(&lib, diff.id), Err(Error::Conflict(_))));
    assert_eq!(lib.client_rules(client.id).unwrap().len(), 1, "conflito não remove a cópia");

    app.remove_global_rule(t.id).unwrap();
    assert!(matches!(app.remove_global_rule(t.id), Err(Error::NotFound(_))));
    let _ = &mut lib;
}

#[test]
fn apply_previews_writes_through_history_and_undoes_as_one_unit() {
    let e = env();
    write_call(
        &e.src,
        "call_2026-06-01_10-00-00_sintetica",
        &["o Gate Wei Service caiu", "ok, vou ver", "o gate wei service de novo e o relatório", "valeu"],
    );
    let app = App::open(&e.data).unwrap();
    import_dir(&app, &e.src, &plain());
    let mut lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    let call = lib.call_id_by_key("call_2026-06-01_10-00-00").unwrap().unwrap();
    let before = texts(&lib, call);
    assert_eq!(before.len(), 4);

    app.add_global_rule(&RuleInput::replace("gate wei service", "Gateway Service"), None).unwrap();
    app.add_global_rule(&RuleInput::replace("relatório", "report"), None).unwrap();
    app.add_global_rule(&RuleInput::term("Kubernetes"), None).unwrap();

    // prévia: nada gravado
    let dry = app.apply_glossary(&mut lib, call, None, Origin::Ui, true).unwrap();
    assert!(dry.dry_run && dry.batch_id.is_none());
    assert_eq!((dry.blocks_changed, dry.replacements), (2, 3));
    assert_eq!(dry.changes[0].seq, 1);
    assert_eq!(dry.changes[0].after, "o Gateway Service caiu");
    assert_eq!(dry.changes[1].rules.iter().map(|h| (h.pattern.as_str(), h.count)).collect::<Vec<_>>(), [("gate wei service", 1), ("relatório", 1)]);
    assert_eq!(texts(&lib, call), before);
    assert!(lib.history(Some(call), 10).unwrap().is_empty());

    // aplicar de verdade
    let r = app.apply_glossary(&mut lib, call, None, Origin::Cli, false).unwrap();
    assert_eq!((r.blocks_changed, r.replacements), (2, 3));
    let batch = r.batch_id.unwrap();
    let after = texts(&lib, call);
    assert_eq!(after[0], "o Gateway Service caiu");
    assert_eq!(after[2], "o Gateway Service de novo e o report");
    assert_eq!((&after[1], &after[3]), (&before[1], &before[3]));
    let tid = lib.active_transcript_id(call).unwrap().unwrap();
    let blocks = lib.blocks(tid).unwrap();
    assert!(blocks[0].edited && blocks[2].edited && !blocks[1].edited, "edited_at acompanha");
    assert_eq!(blocks[0].original_text, before[0]);
    // FTS acompanha
    assert_eq!(core_lib::search::search(&app, "report", 5).unwrap().len(), 1);
    // histórico: duas entradas no mesmo lote, origem cli
    let h = lib.history(Some(call), 10).unwrap();
    assert_eq!(h.len(), 2);
    assert!(h.iter().all(|x| x.batch_id == Some(batch) && x.batch_kind.as_deref() == Some("glossary") && x.batch_size == Some(2) && x.origin == "cli"));

    // idempotente: reaplicar não muda nada e não cria lote
    let again = app.apply_glossary(&mut lib, call, None, Origin::Cli, false).unwrap();
    assert_eq!((again.blocks_changed, again.batch_id), (0, None));

    // uma edição avulsa depois do lote
    let b4 = lib.block_id_by_seq(call, 4).unwrap();
    lib.set_block_text(b4, "valeu mesmo", Origin::Ui, false).unwrap();
    assert!(lib.history(Some(call), 1).unwrap()[0].batch_id.is_none());

    // 1º undo: só a edição avulsa
    let u1 = lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!((u1.batch_id, u1.entity.as_str()), (None, "block_text"));
    assert_eq!(texts(&lib, call)[3], "valeu");
    assert_eq!(texts(&lib, call)[0], "o Gateway Service caiu", "o lote ainda está aplicado");
    // 2º undo: o lote inteiro
    let u2 = lib.undo(Some(call), false).unwrap().unwrap();
    assert_eq!((u2.batch_id, u2.batch_size), (Some(batch), Some(2)));
    assert_eq!(texts(&lib, call), before);
    let blocks = lib.blocks(tid).unwrap();
    assert!(blocks.iter().all(|b| !b.edited));
    assert_eq!(core_lib::search::search(&app, "report", 5).unwrap().len(), 0);
    assert!(lib.history(Some(call), 10).unwrap().iter().all(|x| x.undone_at.is_some()));
    assert!(lib.undo(Some(call), false).unwrap().is_none());

    // undo em simulação não desfaz
    app.apply_glossary(&mut lib, call, None, Origin::Ui, false).unwrap();
    assert!(lib.undo(Some(call), true).unwrap().is_some());
    assert_eq!(texts(&lib, call)[0], "o Gateway Service caiu");

    // versão inexistente / de outra chamada
    assert!(matches!(app.apply_glossary(&mut lib, call, Some(9999), Origin::Ui, true), Err(Error::NotFound(_))));
}

#[test]
fn batches_survive_moving_the_call_to_another_library() {
    let e = env();
    write_call(&e.src, "call_2026-06-01_10-00-00_um", &["o gate caiu", "ok"]);
    write_call(&e.src, "call_2026-06-02_10-00-00_dois", &["o gate caiu de novo", "ok"]);
    let app = App::open(&e.data).unwrap();
    import_dir(&app, &e.src, &plain());
    app.add_global_rule(&RuleInput::replace("gate", "gateway"), None).unwrap();
    let company = app.add_library("Empresa", &e.company).unwrap();
    let client = app.open_library(company.id).unwrap().add_client("Cliente").unwrap();
    let inbox_id = app.inbox_id().unwrap();

    let mut inbox = app.open_library(inbox_id).unwrap();
    let (a, b) = (inbox.call_id_by_key("call_2026-06-01_10-00-00").unwrap().unwrap(), inbox.call_id_by_key("call_2026-06-02_10-00-00").unwrap().unwrap());
    app.apply_glossary(&mut inbox, a, None, Origin::Ui, false).unwrap();
    app.apply_glossary(&mut inbox, b, None, Origin::Ui, false).unwrap();
    drop(inbox);
    let (_, new_a) = transfer::assign(&app, inbox_id, a, company.id, Some(client.id)).unwrap();
    let (_, new_b) = transfer::assign(&app, inbox_id, b, company.id, Some(client.id)).unwrap();

    let mut lib = app.open_library(company.id).unwrap();
    let (ha, hb) = (lib.history(Some(new_a), 10).unwrap(), lib.history(Some(new_b), 10).unwrap());
    assert_eq!((ha.len(), hb.len()), (1, 1));
    assert_ne!(ha[0].batch_id, hb[0].batch_id, "lotes de chamadas diferentes não podem se fundir");
    assert_eq!((ha[0].batch_kind.as_deref(), ha[0].batch_size), (Some("glossary"), Some(1)));
    // desfazer a chamada B não encosta na A
    lib.undo(Some(new_b), false).unwrap().unwrap();
    assert_eq!(texts(&lib, new_a)[0], "o gateway caiu");
    assert_eq!(texts(&lib, new_b)[0], "o gate caiu de novo");
}

/// Importa só a v1 das fixtures, cria a regra, importa a v2 e confere o que o glossário fez.
#[test]
fn auto_apply_on_new_version_import_with_origin_import() {
    let e = env();
    std::fs::create_dir_all(e.src.join("edits")).unwrap();
    let v1 = format!("{FIXTURES_KEY}_teste-gateway");
    let v2 = format!("{v1}_v2");
    std::fs::copy(fixtures().join(format!("{v1}.txt")), e.src.join(format!("{v1}.txt"))).unwrap();
    std::fs::copy(fixtures().join("edits").join(format!("{v1}.json")), e.src.join("edits").join(format!("{v1}.json"))).unwrap();

    let app = App::open(&e.data).unwrap();
    let r = import_dir(&app, &e.src, &plain());
    assert_eq!(r.items[0].glossary_replacements, 0);
    let mut lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    let call = lib.call_id_by_key(FIXTURES_KEY).unwrap().unwrap();
    let v1_tid = lib.active_transcript_id(call).unwrap().unwrap();
    let v1_before: Vec<_> = lib.blocks(v1_tid).unwrap().into_iter().map(|b| b.text).collect();

    // a regra nasce depois da v1: a v1 não é tocada
    app.add_global_rule(&RuleInput::replace("Gateway Service", "Gateway API"), None).unwrap();
    std::fs::copy(fixtures().join(format!("{v2}.txt")), e.src.join(format!("{v2}.txt"))).unwrap();

    // simulação mostra o número e não grava
    let dry = import_dir(&app, &e.src, &ImportOptions { dry_run: true, ..plain() });
    assert_eq!(dry.items[0].glossary_replacements, 1);
    assert_eq!(lib.transcripts(call).unwrap().len(), 1);

    let r = import_dir(&app, &e.src, &plain());
    let it = &r.items[0];
    assert_eq!((it.status.as_str(), it.versions_added.clone(), it.glossary_replacements), ("updated", vec![2], 1));
    let d = lib.call_detail(call, None).unwrap();
    assert_eq!(d.transcripts.len(), 2);
    assert!(d.transcripts[1].is_active);
    let v2_blocks = lib.blocks(d.transcript_id.unwrap()).unwrap();
    assert_eq!(v2_blocks[0].text, "Oi pessoal, o Gateway API caiu.");
    assert_eq!(v2_blocks[0].original_text, "Oi pessoal, o Gateway Service caiu.");
    assert!(v2_blocks[0].edited);
    assert!(!v2_blocks[1].edited);
    let v1_after: Vec<_> = lib.blocks(v1_tid).unwrap().into_iter().map(|b| b.text).collect();
    assert_eq!(v1_after, v1_before, "v1 intacta");

    // histórico: origem import, em lote do glossário; `undo` não mexe nisso
    let h: Vec<_> = lib.history(Some(call), 20).unwrap().into_iter().filter(|x| x.entity_id == v2_blocks[0].id).collect();
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].origin.as_str(), h[0].batch_kind.as_deref(), h[0].batch_size), ("import", Some("glossary"), Some(1)));
    assert!(lib.undo(Some(call), false).unwrap().is_none(), "tudo aqui veio da importação: undo ignora");
    // reverter o bloco continua valendo
    let rev = lib.revert_block(v2_blocks[0].id, Origin::Ui, false).unwrap();
    assert_eq!(rev.text, "Oi pessoal, o Gateway Service caiu.");
    // reimportar é idempotente
    let again = import_dir(&app, &e.src, &plain());
    assert_eq!((again.items[0].status.as_str(), again.items[0].glossary_replacements), ("unchanged", 0));
}

#[test]
fn import_uses_client_rules_over_global_for_new_calls() {
    let e = env();
    write_call(&e.src, "call_2026-06-01_10-00-00_um", &["o Gate Wei caiu", "ok"]);
    let app = App::open(&e.data).unwrap();
    let company = app.add_library("Empresa", &e.company).unwrap();
    let lib = app.open_library(company.id).unwrap();
    let client = lib.add_client("Cliente").unwrap();
    app.add_global_rule(&RuleInput::replace("gate wei", "Global"), None).unwrap();
    lib.add_client_rule(client.id, &RuleInput::replace("gate wei", "Cliente"), None).unwrap();
    drop(lib);

    // com cliente: a regra do cliente vence
    let opts = ImportOptions { library_id: Some(company.id), client_id: Some(client.id), ..plain() };
    let r = import_dir(&app, &e.src, &opts);
    assert_eq!(r.items[0].glossary_replacements, 1);
    let lib = app.open_library(company.id).unwrap();
    let call = lib.call_id_by_key("call_2026-06-01_10-00-00").unwrap().unwrap();
    assert_eq!(texts(&lib, call)[0], "o Cliente caiu");
    drop(lib);

    // outra chamada na inbox (sem cliente): só a global
    let e2 = env();
    write_call(&e2.src, "call_2026-06-03_10-00-00_tres", &["o Gate Wei caiu", "ok"]);
    let r = import_dir(&app, &e2.src, &plain());
    assert_eq!(r.items[0].glossary_replacements, 1);
    let inbox = app.open_library(app.inbox_id().unwrap()).unwrap();
    let call = inbox.call_id_by_key("call_2026-06-03_10-00-00").unwrap().unwrap();
    assert_eq!(texts(&inbox, call)[0], "o Global caiu");
}

#[test]
fn block_edit_suggestions_filter_covered_rules_and_count_occurrences() {
    let e = env();
    write_call(
        &e.src,
        "call_2026-06-01_10-00-00_sintetica",
        &["o Zenit Service caiu", "ok", "de novo o Zenit Service", "zenit-service? não", "nada a ver", "Zenit Services é outra coisa"],
    );
    let app = App::open(&e.data).unwrap();
    let company = app.add_library("Empresa", &e.company).unwrap();
    let lib0 = app.open_library(company.id).unwrap();
    let client = lib0.add_client("Cliente Alfa").unwrap();
    drop(lib0);
    import_dir(&app, &e.src, &ImportOptions { library_id: Some(company.id), client_id: Some(client.id), ..plain() });
    let mut lib = app.open_library(company.id).unwrap();
    let call = lib.call_id_by_key("call_2026-06-01_10-00-00").unwrap().unwrap();
    let b1 = lib.block_id_by_seq(call, 1).unwrap();

    // simulação: sugere, mas sem edit_id nem alteração
    let dry = app.edit_block_text(&mut lib, b1, "o Zenith Service caiu", Origin::Ui, true).unwrap();
    assert!(dry.edit_id.is_none() && !lib.block(b1).unwrap().edited);
    assert_eq!(dry.suggestions.len(), 1);

    let edit = app.edit_block_text(&mut lib, b1, "o Zenith Service caiu", Origin::Ui, false).unwrap();
    assert_eq!(edit.block.text, "o Zenith Service caiu");
    assert_eq!(edit.suggestions.len(), 1);
    let s = &edit.suggestions[0];
    assert_eq!((s.pattern.as_str(), s.replacement.as_str()), ("Zenit", "Zenith"));
    // "Zenit" por palavra inteira: blocos 3, 4 ("zenit-service": hífen separa palavras) e 6
    assert_eq!(s.occurrences_in_call, 3);
    let c = s.client.as_ref().unwrap();
    assert_eq!((c.id, c.name.as_str()), (client.id, "Cliente Alfa"));
    let edit_id = edit.edit_id.unwrap();
    assert_eq!(lib.history(Some(call), 1).unwrap()[0].id, edit_id);

    // criar a regra a partir da sugestão guarda a edição de origem
    let rule = lib.add_client_rule(client.id, &RuleInput::replace(&s.pattern, &s.replacement), Some(edit_id)).unwrap();
    assert_eq!(rule.source_edit_id, Some(edit_id));
    // agora está coberta: nova edição parecida não sugere de novo (nem com a global escondida)
    let b3 = lib.block_id_by_seq(call, 3).unwrap();
    let again = app.edit_block_text(&mut lib, b3, "de novo o Zenith Service", Origin::Ui, false).unwrap();
    assert!(again.suggestions.is_empty());
    app.add_global_rule(&RuleInput::replace("nada", "algo"), None).unwrap();
    let b5 = lib.block_id_by_seq(call, 5).unwrap();
    assert!(app.edit_block_text(&mut lib, b5, "algo a ver", Origin::Ui, false).unwrap().suggestions.is_empty(), "coberta pela global");
    // texto igual / só caixa: sem sugestões e sem edit_id
    let same = app.edit_block_text(&mut lib, b5, "algo a ver", Origin::Ui, false).unwrap();
    assert!(same.edit_id.is_none() && same.suggestions.is_empty());

    // na inbox não há cliente
    let mut e2 = env();
    e2.src = e2._tmp.path().join("src2");
    write_call(&e2.src, "call_2026-06-04_10-00-00_inbox", &["o Zenit caiu", "ok"]);
    import_dir(&app, &e2.src, &plain());
    let mut inbox = app.open_library(app.inbox_id().unwrap()).unwrap();
    let ic = inbox.call_id_by_key("call_2026-06-04_10-00-00").unwrap().unwrap();
    let ib = inbox.block_id_by_seq(ic, 1).unwrap();
    let r = app.edit_block_text(&mut inbox, ib, "o Zenith caiu", Origin::Cli, false).unwrap();
    assert_eq!(r.suggestions.len(), 1);
    assert!(r.suggestions[0].client.is_none());
    // JSON: campos do bloco no nível de cima + edit_id + suggestions
    let v = serde_json::to_value(&r).unwrap();
    assert!(v["id"].is_i64() && v["text"].is_string() && v["edit_id"].is_i64() && v["suggestions"].is_array());
}

#[test]
fn prompt_terms_merge_client_first_dedup_and_budget() {
    let e = env();
    let app = App::open(&e.data).unwrap();
    let company = app.add_library("Empresa", &e.company).unwrap();
    let lib = app.open_library(company.id).unwrap();
    let client = lib.add_client("Cliente").unwrap();
    app.add_global_rule(&RuleInput::term("Kubernetes"), None).unwrap();
    app.add_global_rule(&RuleInput::term("zenith service"), None).unwrap();
    app.add_global_rule(&RuleInput::replace("zenit", "Zenith"), None).unwrap();
    lib.add_client_rule(client.id, &RuleInput::term("Zenith Service"), None).unwrap();
    lib.add_client_rule(client.id, &RuleInput::term("Kafka"), None).unwrap();

    let t = app.prompt_terms(&lib, Some(client.id)).unwrap();
    // o termo do cliente sobrescreve o global de mesmo padrão; client primeiro; replace não entra
    assert_eq!(t, ["Zenith Service", "Kafka", "Kubernetes"]);
    assert_eq!(app.prompt_terms(&lib, None).unwrap(), ["Kubernetes", "zenith service"]);

    for i in 0..300 {
        lib.add_client_rule(client.id, &RuleInput::term(&format!("termo-numero-{i}")), None).unwrap();
    }
    let t = app.prompt_terms(&lib, Some(client.id)).unwrap();
    let cost: usize = t.iter().map(|x| core_lib::glossary::estimate_tokens(x) + 1).sum();
    assert!(cost <= core_lib::glossary::PROMPT_TOKEN_BUDGET && t.len() < 300, "{} termos, custo {cost}", t.len());
    assert_eq!(t[0], "Zenith Service", "prioridade de quem veio primeiro");
}

#[test]
fn import_term_list_file() {
    let e = env();
    std::fs::create_dir_all(&e.src).unwrap();
    let file = e.src.join("termos.txt");
    std::fs::write(
        &file,
        "\u{feff}# lista sintética\nKubernetes\n\nGate Wei -> Gateway\nzenit → Zenith\nfoo => bar\nKubernetes\nC#\nruim ->\n  \n",
    )
    .unwrap();
    let app = App::open(&e.data).unwrap();

    let dry = app.import_glossary_file(&file, ImportScope::Global, None, true).unwrap();
    assert!(dry.dry_run);
    assert_eq!((dry.added, dry.skipped, dry.invalid), (5, 1, 1));
    assert!(app.global_rules().unwrap().is_empty(), "simulação não grava");

    let r = app.import_glossary_file(&file, ImportScope::Global, None, false).unwrap();
    assert_eq!((r.added, r.skipped, r.invalid), (5, 1, 1));
    let by_line = |n: usize| r.entries.iter().find(|x| x.line == n).unwrap();
    assert_eq!((by_line(7).status.as_str(), by_line(7).line), ("duplicate", 7));
    assert_eq!((by_line(9).status.as_str(), by_line(9).reason.as_deref()), ("invalid", Some("empty_side")));
    let rules = app.global_rules().unwrap();
    assert_eq!(rules.len(), 5);
    let gw = rules.iter().find(|r| r.pattern == "Gate Wei").unwrap();
    assert_eq!((gw.kind, gw.replacement.as_deref()), (RuleKind::Replace, Some("Gateway")));
    assert!(rules.iter().any(|r| r.pattern == "C#" && r.kind == RuleKind::Term), "'#' no meio da linha faz parte do termo");

    // de novo: tudo duplicado
    let again = app.import_glossary_file(&file, ImportScope::Global, None, false).unwrap();
    assert_eq!((again.added, again.skipped, again.invalid), (0, 6, 1));

    // dica de tipo
    let only_terms = app.import_glossary_file(&file, ImportScope::Global, Some(RuleKind::Term), true).unwrap();
    assert_eq!(only_terms.entries.iter().find(|x| x.line == 4).unwrap().reason.as_deref(), Some("not_a_term"));
    let only_repl = app.import_glossary_file(&file, ImportScope::Global, Some(RuleKind::Replace), true).unwrap();
    assert_eq!(only_repl.entries.iter().find(|x| x.line == 2).unwrap().reason.as_deref(), Some("not_a_replacement"));

    // escopo cliente
    let company = app.add_library("Empresa", &e.company).unwrap();
    let client = app.open_library(company.id).unwrap().add_client("Cliente").unwrap();
    let scope = ImportScope::Client { library_id: company.id, client_id: client.id };
    let r = app.import_glossary_file(&file, scope, None, false).unwrap();
    assert_eq!(r.added, 5, "a duplicata é contra a camada do cliente, não a global");
    assert_eq!(app.open_library(company.id).unwrap().client_rules(client.id).unwrap().len(), 5);
    let inbox_scope = ImportScope::Client { library_id: app.inbox_id().unwrap(), client_id: 1 };
    assert!(matches!(app.import_glossary_file(&file, inbox_scope, None, true), Err(Error::Invalid(_))));

    // arquivo que não é UTF-8 / não existe
    let bad = e.src.join("latin1.txt");
    std::fs::write(&bad, [b'a', 0xE9, b'\n']).unwrap();
    assert!(matches!(app.import_glossary_file(&bad, ImportScope::Global, None, true), Err(Error::Invalid(_))));
    assert!(matches!(app.import_glossary_file(&e.src.join("nao-existe.txt"), ImportScope::Global, None, true), Err(Error::NotFound(_))));
}
