//! Ponta a ponta do núcleo com dados sintéticos (nada de gravações reais).
use std::path::{Path, PathBuf};

use core_lib::import::{self, ImportOptions};
use core_lib::{App, ClientFilter, Origin, search, transfer};

const KEY: &str = "call_2026-02-01_14-30-00";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.path().is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

fn write_wav(path: &Path, secs: u32) {
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..16_000 * secs {
        w.write_sample(((i % 200) as i16 - 100) * 50).unwrap();
    }
    w.finalize().unwrap();
}

struct Env {
    _tmp: tempfile::TempDir,
    src: PathBuf,
    data: PathBuf,
    company: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    copy_dir(&fixtures(), &src);
    write_wav(&src.join(format!("{KEY}_teste-gateway.mic.wav")), 3);
    write_wav(&src.join(format!("{KEY}_teste-gateway.sys.wav")), 200);
    Env { data: tmp.path().join("data"), company: tmp.path().join("Empresa"), src, _tmp: tmp }
}

fn import_all(app: &App, src: &Path, opts: &ImportOptions) -> import::ImportReport {
    let cands = import::scan(&[src.to_path_buf()]).unwrap();
    import::import(app, &cands, opts, &mut |_| {}).unwrap()
}

#[test]
fn fts5_is_available_and_ignores_accents() {
    let e = env();
    let app = App::open(&e.data).unwrap();
    import_all(&app, &e.src, &ImportOptions { convert_audio: false, ..Default::default() });
    let hits = search::search(&app, "relatorio", 10).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].snippet.contains("\u{2}relatório\u{3}"));
    // prefixo
    assert!(!search::search(&app, "gatewa", 10).unwrap().is_empty());
}

#[test]
fn import_groups_versions_skips_empty_and_is_idempotent() {
    let e = env();
    let app = App::open(&e.data).unwrap();
    let r = import_all(&app, &e.src, &ImportOptions { convert_audio: true, ..Default::default() });
    let by_key = |k: &str| r.items.iter().find(|i| i.key == k).unwrap();
    assert_eq!(by_key("call_2026-01-11_10-00-00").status, "skipped");
    assert!(by_key("call_2026-01-11_10-00-00").reason.as_deref().unwrap().starts_with("empty"));
    assert_eq!(by_key("call_2026-03-01_08-00-00").status, "skipped");
    let it = by_key(KEY);
    assert_eq!((it.status.as_str(), it.versions_added.clone(), it.edits_applied), ("new", vec![1, 2], 1));
    assert_eq!(it.audio_converted, vec!["mic", "sys"]);

    let inbox = app.open_library(app.inbox_id().unwrap()).unwrap();
    let id = inbox.call_id_by_key(KEY).unwrap().unwrap();
    let d = inbox.call_detail(id, None).unwrap();
    assert_eq!(d.summary.title, "Teste gateway");
    assert_eq!(d.summary.duration_s, 200, "duração vem do WAV");
    assert_eq!(d.transcripts.len(), 2);
    assert_eq!(d.transcripts.iter().find(|t| t.is_active).unwrap().version, 2);
    assert_eq!(d.chapters.len(), 2);
    assert!(d.summary.has_audio);
    let mic = inbox.audio_abs(d.audio.mic_path.as_deref().unwrap());
    assert_eq!(&std::fs::read(&mic).unwrap()[..4], b"fLaC");
    // a edição do protótipo ficou presa à versão 1
    let v1 = d.transcripts.iter().find(|t| t.version == 1).unwrap().id;
    let v1_blocks = inbox.blocks(v1).unwrap();
    let edited: Vec<_> = v1_blocks.iter().filter(|b| b.edited).collect();
    assert_eq!(edited.len(), 1);
    assert_eq!(edited[0].text, "Vou olhar o Gateway Service agora.");
    assert_eq!(edited[0].t_start, 15.0);
    // "Speaker N" vira falante do sistema; "Eu" é o microfone
    let s = inbox.speakers(id).unwrap();
    assert_eq!(s.iter().find(|s| s.label == "Eu").unwrap().track, "mic");
    // originais intactos
    assert!(e.src.join(format!("{KEY}_teste-gateway.sys.wav")).is_file());
    drop(inbox);

    let again = import_all(&app, &e.src, &ImportOptions { convert_audio: true, ..Default::default() });
    let it = again.items.iter().find(|i| i.key == KEY).unwrap();
    assert_eq!(it.status, "unchanged");
    assert!(it.audio_pending.is_empty());
    assert!(!import::reclaimable(&app).unwrap().is_empty());
}

#[test]
fn dry_run_writes_nothing() {
    let e = env();
    let app = App::open(&e.data).unwrap();
    let r = import_all(&app, &e.src, &ImportOptions { convert_audio: true, dry_run: true, ..Default::default() });
    assert!(r.items.iter().any(|i| i.status == "new"));
    // a prévia mostra o áudio que seria convertido, mesmo com a chamada ainda não gravada
    let it = r.items.iter().find(|i| i.key == KEY).unwrap();
    assert_eq!(it.audio_pending, vec!["mic", "sys"]);
    assert!(it.audio_converted.is_empty());
    let skipped = r.items.iter().find(|i| i.key == "call_2026-01-11_10-00-00").unwrap();
    assert!(skipped.audio_pending.is_empty());
    let inbox = app.open_library(app.inbox_id().unwrap()).unwrap();
    assert!(inbox.calls(ClientFilter::Any).unwrap().is_empty());
    assert!(!e.data.join("inbox").join(format!("{KEY}_teste-gateway")).exists(), "dry-run não converte");
}

#[test]
fn prototype_edits_land_on_their_own_version_with_origin_import() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("edits")).unwrap();
    let stem = "call_2026-05-05_10-00-00_sintetica";
    std::fs::write(src.join(format!("{stem}.txt")), "[00:00:01] Outros: um\n[00:02:00] Eu: dois\n").unwrap();
    std::fs::write(src.join(format!("{stem}_v2.txt")), "[00:00:01] Outros: um v2\n[00:02:00] Eu: dois v2\n").unwrap();
    std::fs::write(src.join("edits").join(format!("{stem}_v2.json")), r#"{"120": "dois editado", "999": "sem bloco"}"#).unwrap();

    let app = App::open(&tmp.path().join("data")).unwrap();
    let r = import_all(&app, &src, &ImportOptions { convert_audio: false, ..Default::default() });
    let it = r.items.iter().find(|i| i.key == "call_2026-05-05_10-00-00").unwrap();
    assert_eq!((it.versions_added.clone(), it.edits_applied), (vec![1, 2], 1));

    let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    let id = lib.call_id_by_key("call_2026-05-05_10-00-00").unwrap().unwrap();
    let d = lib.call_detail(id, None).unwrap();
    let (v1, v2) = (d.transcripts.iter().find(|t| t.version == 1).unwrap(), d.transcripts.iter().find(|t| t.version == 2).unwrap());
    assert!(v2.is_active && !v1.is_active, "a maior versão fica ativa");
    assert!(lib.blocks(v1.id).unwrap().iter().all(|b| !b.edited), "v1 não recebe edições da v2");
    let edited: Vec<_> = lib.blocks(v2.id).unwrap().into_iter().filter(|b| b.edited).collect();
    assert_eq!(edited.len(), 1);
    assert_eq!((edited[0].t_start, edited[0].text.as_str(), edited[0].original_text.as_str()), (120.0, "dois editado", "dois v2"));
    let hist = lib.history(Some(id), 10).unwrap();
    assert_eq!(hist.len(), 1);
    assert_eq!((hist[0].origin.as_str(), hist[0].entity.as_str(), hist[0].entity_id), ("import", "block_text", edited[0].id));
}

#[test]
fn edits_history_and_undo() {
    let e = env();
    let app = App::open(&e.data).unwrap();
    import_all(&app, &e.src, &ImportOptions::default());
    let mut lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    let id = lib.call_id_by_key(KEY).unwrap().unwrap();
    let b = lib.block_id_by_seq(id, 1).unwrap();

    let dry = lib.set_block_text(b, "  texto   novo ", Origin::Cli, true).unwrap();
    assert_eq!(dry.text, "texto novo");
    assert!(!lib.block(b).unwrap().edited, "dry-run não grava");

    let after = lib.set_block_text(b, "texto novo", Origin::Ui, false).unwrap();
    assert!(after.edited);
    assert!(search::search(&app, "novo", 5).unwrap().iter().any(|h| h.block_id == Some(b)), "FTS acompanha a edição");

    lib.set_title(id, "Incidente do Gateway", Origin::Ui, false).unwrap();
    let spk = lib.find_speaker(id, "Outros").unwrap();
    lib.rename_speaker(spk.id, Some("Maria"), Origin::Cli, false).unwrap();
    assert_eq!(lib.find_speaker(id, "maria").unwrap().id, spk.id);
    let eu = lib.find_speaker(id, "Eu").unwrap();
    lib.set_block_speaker(b, eu.id, Origin::Ui, false).unwrap();
    assert_eq!(lib.history(Some(id), 10).unwrap().len(), 4 + 1, "4 edições + a do protótipo");

    // desfaz na ordem inversa
    assert_eq!(lib.undo(Some(id), false).unwrap().unwrap().entity, "block_speaker");
    assert_eq!(lib.block(b).unwrap().speaker_id, spk.id);
    assert_eq!(lib.undo(Some(id), false).unwrap().unwrap().entity, "speaker_name");
    assert_eq!(lib.undo(Some(id), false).unwrap().unwrap().entity, "call_title");
    assert_eq!(lib.call_summary(id).unwrap().title, "Teste gateway");
    assert_eq!(lib.undo(Some(id), false).unwrap().unwrap().entity, "block_text");
    assert!(!lib.block(b).unwrap().edited);
    assert!(lib.undo(Some(id), false).unwrap().is_none(), "edições da importação não são desfeitas por undo");

    // reverter volta ao original
    lib.set_block_text(b, "outra", Origin::Ui, false).unwrap();
    let r = lib.revert_block(b, Origin::Ui, false).unwrap();
    assert!(!r.edited);
    assert!(lib.set_block_text(b, "   ", Origin::Ui, false).is_err());
}

#[test]
fn assign_moves_call_audio_and_history_to_company() {
    let e = env();
    let app = App::open(&e.data).unwrap();
    import_all(&app, &e.src, &ImportOptions { convert_audio: true, ..Default::default() });
    let inbox_id = app.inbox_id().unwrap();
    let (call_id, b) = {
        let mut inbox = app.open_library(inbox_id).unwrap();
        let id = inbox.call_id_by_key(KEY).unwrap().unwrap();
        let b = inbox.block_id_by_seq(id, 1).unwrap();
        inbox.set_block_text(b, "editado antes de classificar", Origin::Ui, false).unwrap();
        (id, b)
    };
    let _ = b;
    let company = app.add_library("Empresa Teste", &e.company).unwrap();
    let client = app.open_library(company.id).unwrap().add_client("Cliente Alfa").unwrap();
    assert_eq!(client.slug, "cliente-alfa");

    let (lib_id, new_id) = transfer::assign(&app, inbox_id, call_id, company.id, Some(client.id)).unwrap();
    assert_eq!(lib_id, company.id);
    let inbox = app.open_library(inbox_id).unwrap();
    assert!(inbox.call_id_by_key(KEY).unwrap().is_none());
    assert!(!e.data.join("inbox").join(format!("{KEY}_teste-gateway")).exists());

    let mut lib = app.open_library(company.id).unwrap();
    let d = lib.call_detail(new_id, None).unwrap();
    assert_eq!(d.summary.client_name.as_deref(), Some("Cliente Alfa"));
    let mic = d.audio.mic_path.clone().unwrap();
    assert_eq!(mic, format!("cliente-alfa/{KEY}_teste-gateway/mic.flac"));
    assert!(e.company.join(&mic).is_file());
    assert_eq!(d.transcripts.len(), 2);
    assert_eq!(lib.history(Some(new_id), 10).unwrap().len(), 2);
    // o desfazer continua funcionando com os ids novos
    assert_eq!(lib.undo(Some(new_id), false).unwrap().unwrap().entity, "block_text");
    assert!(!lib.block(lib.block_id_by_seq(new_id, 1).unwrap()).unwrap().edited);

    // mudar de cliente na mesma empresa move a pasta
    let other = lib.add_client("Cliente Beta").unwrap();
    lib.set_client(new_id, Some(other.id)).unwrap();
    let d = lib.call_detail(new_id, None).unwrap();
    assert!(d.audio.sys_path.unwrap().starts_with("cliente-beta/"));
    assert!(!e.company.join("cliente-alfa").exists(), "pasta vazia do cliente antigo removida");

    // a chamada continua achável por chave e pela busca global
    assert_eq!(app.find_call(KEY).unwrap(), (company.id, new_id));
    assert_eq!(app.find_call(&format!("{KEY}_teste-gateway_v2")).unwrap(), (company.id, new_id));
    assert!(search::search(&app, "Gateway", 10).unwrap().iter().all(|h| h.library_id == company.id));
}

#[test]
fn libraries_are_reopened_from_a_copied_folder() {
    let e = env();
    {
        let app = App::open(&e.data).unwrap();
        let company = app.add_library("Empresa", &e.company).unwrap();
        import_all(&app, &e.src, &ImportOptions { library_id: Some(company.id), ..Default::default() });
    }
    let copy = e.company.with_file_name("Empresa-copia");
    copy_dir(&e.company, &copy);
    assert!(!copy.join("library.db-wal").exists() || std::fs::metadata(copy.join("library.db-wal")).unwrap().len() == 0);
    let other = App::open(&e.data.with_file_name("data2")).unwrap();
    let lib = other.add_library("Cópia", &copy).unwrap();
    assert_eq!(other.open_library(lib.id).unwrap().calls(ClientFilter::Any).unwrap().len(), 2);
}
