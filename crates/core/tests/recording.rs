//! Fase 3 no núcleo: chamada sem transcrição ("pendente"), transferência, "último usado" e a
//! especificação executável do fluxo gravar → parar → finalizar (com `FakeBackend`; só tons sintéticos).
use core_lib::model::{TRANSCRIPTION_DONE, TRANSCRIPTION_PENDING};
use core_lib::recording::{self, ActiveRecording, LastUsed, Meta, Progress, StartRequest, Stopped};
use core_lib::{App, ClientFilter, search, transfer};
use recorder::{FakeBackend, Sidecar, State, StreamChoice};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn app() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::open(&tmp.path().join("data")).unwrap();
    (tmp, app)
}

/// Insere uma chamada como a de `finalize` faria: sem transcrição, pendente.
fn insert_pending(app: &App, key: &str) -> (i64, i64) {
    let lib_id = app.inbox_id().unwrap();
    let lib = app.open_library(lib_id).unwrap();
    lib.conn
        .execute(
            "INSERT INTO calls (key, title, slug, started_at, duration_s, language, dir, mic_path, sys_path, created_at, transcription_state)
             VALUES (?1, 'Reunião de tom', '', '2026-10-02T09:00:00', 90, 'pt', ?1, ?2, ?3, '2026-10-02T09:01:30', 'pending')",
            rusqlite_params(key),
        )
        .unwrap();
    (lib_id, lib.call_id_by_key(key).unwrap().unwrap())
}

fn rusqlite_params(key: &str) -> impl rusqlite::Params {
    [key.to_string(), format!("{key}/mic.flac"), format!("{key}/sys.flac")]
}

#[test]
fn pending_call_reads_without_transcript() {
    let (_tmp, app) = app();
    let (lib_id, id) = insert_pending(&app, "call_2026-10-02_09-00-00");
    let lib = app.open_library(lib_id).unwrap();

    let d = lib.call_detail(id, None).unwrap();
    assert_eq!(d.transcript_id, None);
    assert!(d.transcripts.is_empty() && d.blocks.is_empty() && d.speakers.is_empty() && d.chapters.is_empty());
    assert_eq!(d.summary.transcription_state, TRANSCRIPTION_PENDING);
    assert_eq!((d.summary.versions, d.summary.words), (0, 0));
    assert!(d.summary.has_audio);
    // pedir uma versão que não existe continua sendo erro
    assert_eq!(lib.call_detail(id, Some(1)).unwrap_err().code(), "not_found");

    let list = lib.calls(ClientFilter::Any).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].transcription_state, TRANSCRIPTION_PENDING);
    // o título entra na busca mesmo sem blocos
    assert_eq!(search::search(&app, "tom", 10).unwrap().len(), 1);
}

#[test]
fn assign_keeps_pending_state() {
    let (tmp, app) = app();
    let (inbox, id) = insert_pending(&app, "call_2026-10-02_09-00-01");
    let company = app.add_library("Empresa", &tmp.path().join("empresa")).unwrap();
    let (to_lib, new_id) = transfer::assign(&app, inbox, id, company.id, None).unwrap();
    let moved = app.open_library(to_lib).unwrap().call_summary(new_id).unwrap();
    assert_eq!(moved.transcription_state, TRANSCRIPTION_PENDING);
}

#[test]
fn existing_calls_default_to_done() {
    // migração: DEFAULT 'done' para quem já tinha transcrição (importadas)
    let (_tmp, app) = app();
    let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    lib.conn
        .execute(
            "INSERT INTO calls (key, started_at, created_at) VALUES ('call_2026-10-02_09-00-02', '2026-10-02T09:00:02', '2026-10-02T09:00:02')",
            [],
        )
        .unwrap();
    let id = lib.call_id_by_key("call_2026-10-02_09-00-02").unwrap().unwrap();
    assert_eq!(lib.call_summary(id).unwrap().transcription_state, TRANSCRIPTION_DONE);
}

#[test]
fn last_used_roundtrip_and_stale_target() {
    let (tmp, app) = app();
    assert_eq!(recording::last_used(&app).unwrap(), LastUsed::default());
    let company = app.add_library("Empresa", &tmp.path().join("empresa")).unwrap();
    let client = app.open_library(company.id).unwrap().add_client("Cliente").unwrap();
    let last = LastUsed {
        library_id: Some(company.id),
        client_id: Some(client.id),
        mic: StreamChoice::Named("alsa_input.x".into()),
        sys: StreamChoice::Off,
    };
    recording::save_last_used(&app, &last).unwrap();
    assert_eq!(recording::last_used(&app).unwrap(), last);
    app.remove_library(company.id).unwrap();
    let after = recording::last_used(&app).unwrap();
    assert_eq!((after.library_id, after.client_id), (None, None));
    assert_eq!(after.mic, last.mic);
}

#[test]
fn key_format() {
    let k = recording::new_key();
    assert!(core_lib::parse::parse_stem(&k).is_some_and(|s| s.key == k), "{k}");
}

/// Duas gravações no mesmo segundo: a segunda tem a chave `<base>_2`, que o `parse_stem` leria como slug.
/// A busca pela chave completa tem de achar a chamada certa; o nome antigo (slug/versão) continua valendo.
#[test]
fn find_call_by_suffixed_key_does_not_resolve_to_the_base_call() {
    let (_tmp, app) = app();
    let (lib_id, base) = insert_pending(&app, "call_2026-10-02_09-00-00");
    let (_, second) = insert_pending(&app, "call_2026-10-02_09-00-00_2");
    assert_ne!(base, second);
    assert_eq!(app.find_call("call_2026-10-02_09-00-00").unwrap(), (lib_id, base));
    assert_eq!(app.find_call("call_2026-10-02_09-00-00_2").unwrap(), (lib_id, second));
    // sem chamada com essa chave exata, cai no nome antigo (chave base + slug)
    assert_eq!(app.find_call("call_2026-10-02_09-00-00_reuniao_v2").unwrap(), (lib_id, base));
    assert_eq!(app.find_call("call_2026-10-02_09-00-00_3").unwrap(), (lib_id, base));
}

#[test]
fn record_stop_finalize_with_fake_backend() {
    let (_tmp, app) = app();
    let backend = std::sync::Arc::new(recorder::FakeBackend::new());
    let req = StartRequest { meta: Meta { title: Some("Tom".into()), expected_speakers: Some(2), ..Meta::default() }, ..Default::default() };
    let rec = recording::start(&app, backend, req).unwrap();
    let (key, dir) = (rec.key().to_string(), rec.dir().to_path_buf());
    assert!(dir.starts_with(recording::recording_root(&app.data_dir)));
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // enquanto grava, não é órfã
    assert!(recording::scan_orphans(&app, Some(&key)).unwrap().is_empty());
    let stopped = rec.stop().unwrap();
    assert_eq!(stopped.key, key);
    // parada limpa mas ainda não finalizada: aparece como órfã `complete`
    assert_eq!(recording::scan_orphans(&app, None).unwrap().len(), 1);

    let mut stages = Vec::new();
    let call = recording::finalize(&app, &key, &mut |p| stages.push(p)).unwrap();
    assert!(!stages.is_empty());
    let lib = app.open_library(call.library_id).unwrap();
    let d = lib.call_detail(call.call_id, None).unwrap();
    assert_eq!((d.summary.title.as_str(), d.expected_speakers), ("Tom", Some(2)));
    assert_eq!(d.summary.transcription_state, TRANSCRIPTION_PENDING);
    assert!(d.transcript_id.is_none() && d.audio.mic_path.is_some() && d.audio.sys_path.is_some());
    let call_dir = lib.root().join(d.audio.mic_path.unwrap()).parent().unwrap().to_path_buf();
    assert!(call_dir.join(recording::MIC_FLAC).is_file() && call_dir.join(recording::SYS_FLAC).is_file());
    assert!(call_dir.join(recording::SIDECAR_IN_CALL).is_file());
    assert!(!dir.exists(), "pasta de gravação (e os WAVs) removida");
}

// ------------------------------------------------------------------ gravar → finalizar (só tons sintéticos)

fn fake() -> Arc<FakeBackend> {
    // tempo real: o modo `fast` gera áudio sem pausa e encheria o disco em poucos segundos
    Arc::new(FakeBackend::new())
}

fn begin(app: &App, meta: Meta) -> ActiveRecording {
    recording::start(app, fake(), StartRequest { meta, ..Default::default() }).unwrap()
}

fn record_for(app: &App, meta: Meta, ms: u64) -> Stopped {
    let rec = begin(app, meta);
    std::thread::sleep(Duration::from_millis(ms));
    rec.stop().unwrap()
}

fn flac_samples(path: &Path) -> u64 {
    claxon::FlacReader::open(path).unwrap().streaminfo().samples.unwrap()
}

fn call_dir_of(app: &App, call: &recording::CallRef) -> PathBuf {
    let lib = app.open_library(call.library_id).unwrap();
    let d = lib.call_detail(call.call_id, None).unwrap();
    let any = d.audio.mic_path.or(d.audio.sys_path).unwrap();
    lib.root().join(any).parent().unwrap().to_path_buf()
}

fn rec_dirs(app: &App) -> Vec<String> {
    let root = recording::recording_root(&app.data_dir);
    let mut v: Vec<String> = std::fs::read_dir(root).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

#[test]
fn finalized_flac_has_every_recorded_frame_and_the_sidecar_follows() {
    let (_tmp, app) = app();
    let stopped = record_for(&app, Meta { title: Some("  Reunião   de tom ".into()), language: Some("en-US".into()), ..Meta::default() }, 900);
    let (mic_n, sys_n) = (stopped.sidecar.mic.as_ref().unwrap().samples, stopped.sidecar.sys.as_ref().unwrap().samples);
    assert!(mic_n >= 8_000);
    let call = recording::finalize(&app, &stopped.key, &mut |_| {}).unwrap();
    let dir = call_dir_of(&app, &call);
    assert_eq!((flac_samples(&dir.join(recording::MIC_FLAC)), flac_samples(&dir.join(recording::SYS_FLAC))), (mic_n, sys_n));
    let sc = Sidecar::read(&dir).unwrap();
    assert_eq!((sc.state, sc.key.as_str()), (State::Complete, call.key.as_str()));
    assert_eq!(sc.extra["intent"]["title"], "Reunião de tom");
    let d = app.open_library(call.library_id).unwrap().call_detail(call.call_id, None).unwrap();
    assert_eq!((d.summary.title.as_str(), d.language.as_deref()), ("Reunião de tom", Some("en")));
    assert_eq!(d.summary.duration_s, (mic_n.max(sys_n) as f64 / 16_000.0).round() as i64);
    assert_eq!(d.summary.started_at, stopped.sidecar.started_at);
    assert!(recording::scan_orphans(&app, None).unwrap().is_empty());
    // o "último usado" foi gravado pelo start
    let last = recording::last_used(&app).unwrap();
    assert_eq!((last.library_id, last.mic, last.sys), (Some(app.inbox_id().unwrap()), StreamChoice::Default, StreamChoice::Default));
}

#[test]
fn off_track_has_no_flac_and_no_path() {
    let (_tmp, app) = app();
    let rec = recording::start(
        &app,
        fake(),
        StartRequest { sys: StreamChoice::Off, meta: Meta::default(), ..Default::default() },
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let key = rec.stop().unwrap().key;
    let call = recording::finalize(&app, &key, &mut |_| {}).unwrap();
    let d = app.open_library(call.library_id).unwrap().call_detail(call.call_id, None).unwrap();
    assert!(d.audio.mic_path.is_some() && d.audio.sys_path.is_none());
    let dir = call_dir_of(&app, &call);
    assert!(dir.join(recording::MIC_FLAC).is_file() && !dir.join(recording::SYS_FLAC).exists());
    assert_eq!(d.summary.title, "", "sem título: vazio (a UI mostra \"Chamada de <data>\")");
}

#[test]
fn crash_then_recover_keeps_every_frame_on_disk() {
    let (_tmp, app) = app();
    let rec = begin(&app, Meta { title: Some("Caiu".into()), expected_speakers: Some(3), ..Meta::default() });
    let (key, dir) = (rec.key().to_string(), rec.dir().to_path_buf());
    std::thread::sleep(Duration::from_millis(1200));
    drop(rec); // queda: sidecar continua `recording`, cabeçalhos com tamanho 0
    // e a queda cortou a última amostra do mic no meio
    {
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(dir.join("mic.wav")).unwrap().write_all(&[7]).unwrap();
    }
    let expected_mic = (std::fs::metadata(dir.join("mic.wav")).unwrap().len() - 44) / 2;
    let expected_sys = (std::fs::metadata(dir.join("sys.wav")).unwrap().len() - 44) / 2;
    assert!(expected_mic >= 16_000);

    let orphans = recording::scan_orphans(&app, None).unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!((orphans[0].key.as_str(), orphans[0].state), (key.as_str(), State::Recording));
    assert!((orphans[0].duration_s - expected_mic as f64 / 16_000.0).abs() < 0.2);
    assert_eq!(orphans[0].intent.as_ref().unwrap().title, "Caiu");
    assert_eq!(orphans[0].mic_device.as_deref(), Some("Fake Microphone"));
    // `finalize` não aceita gravação interrompida
    assert_eq!(recording::finalize(&app, &key, &mut |_| {}).unwrap_err().code(), "invalid");
    assert!(dir.exists());

    let mut stages = Vec::new();
    let call = recording::recover(&app, &key, &mut |p| stages.push(p)).unwrap();
    assert!(stages.contains(&Progress::Repair { track: "mic".into() }) && stages.contains(&Progress::Repair { track: "sys".into() }));
    assert_eq!(stages.last(), Some(&Progress::Call));
    assert!(stages.iter().any(|p| matches!(p, Progress::Convert { done, of, .. } if done == of)));
    let cdir = call_dir_of(&app, &call);
    assert_eq!(flac_samples(&cdir.join(recording::MIC_FLAC)), expected_mic);
    assert_eq!(flac_samples(&cdir.join(recording::SYS_FLAC)), expected_sys);
    let sc = Sidecar::read(&cdir).unwrap();
    assert_eq!(sc.state, State::Complete);
    assert_eq!(sc.mic.as_ref().unwrap().samples, expected_mic);
    assert!(sc.ended_at.is_some());
    let d = app.open_library(call.library_id).unwrap().call_detail(call.call_id, None).unwrap();
    assert_eq!((d.summary.title.as_str(), d.expected_speakers, d.summary.transcription_state.as_str()), ("Caiu", Some(3), "pending"));
    assert!(!dir.exists());
}

#[test]
fn recover_also_accepts_a_clean_stop_whose_finalize_never_ran() {
    let (_tmp, app) = app();
    let stopped = record_for(&app, Meta::default(), 800);
    let call = recording::recover(&app, &stopped.key, &mut |_| {}).unwrap();
    assert!(call.call_id > 0 && !stopped.dir.exists());
}

#[test]
fn empty_recording_creates_no_call_and_removes_the_folder() {
    let (_tmp, app) = app();
    let stopped = record_for(&app, Meta::default(), 0); // para na hora: < 0,5 s
    let err = recording::finalize(&app, &stopped.key, &mut |_| {}).unwrap_err();
    assert_eq!(err.code(), "empty_recording");
    assert!(rec_dirs(&app).is_empty());
    assert_eq!(app.open_library(app.inbox_id().unwrap()).unwrap().calls(ClientFilter::Any).unwrap().len(), 0);
}

#[test]
fn crashed_file_shorter_than_a_header_counts_as_empty_track() {
    let (_tmp, app) = app();
    let rec = begin(&app, Meta::default());
    let (key, dir) = (rec.key().to_string(), rec.dir().to_path_buf());
    std::thread::sleep(Duration::from_millis(800));
    drop(rec);
    std::fs::write(dir.join("sys.wav"), *b"RI").unwrap();
    let call = recording::recover(&app, &key, &mut |_| {}).unwrap();
    let d = app.open_library(call.library_id).unwrap().call_detail(call.call_id, None).unwrap();
    assert!(d.audio.mic_path.is_some() && d.audio.sys_path.is_none());
}

#[test]
fn discard_removes_only_the_recording_folder() {
    let (_tmp, app) = app();
    let (lib_id, call_id) = insert_pending(&app, "call_2026-10-02_09-00-09");
    let stopped = record_for(&app, Meta::default(), 600);
    assert!(stopped.dir.exists());
    recording::discard(&app, &stopped.key).unwrap();
    assert!(!stopped.dir.exists());
    assert_eq!(recording::discard(&app, &stopped.key).unwrap_err().code(), "not_found");
    assert_eq!(recording::discard(&app, "../inbox").unwrap_err().code(), "invalid");
    assert_eq!(recording::finalize(&app, "call_2026-01-01_00-00-00", &mut |_| {}).unwrap_err().code(), "not_found");
    assert!(app.open_library(lib_id).unwrap().call_exists(call_id).is_ok(), "chamadas não são tocadas");
    assert!(app.data_dir.join("inbox").exists());
}

#[test]
fn keys_that_are_paths_are_rejected_everywhere() {
    let (_tmp, app) = app();
    for bad in ["", "..", "../x", "call_/../..", "call_2026\\x", "/etc", "other"] {
        assert_eq!(recording::discard(&app, bad).unwrap_err().code(), "invalid", "{bad:?}");
        assert_eq!(recording::recover(&app, bad, &mut |_| {}).unwrap_err().code(), "invalid", "{bad:?}");
        assert_eq!(recording::finalize(&app, bad, &mut |_| {}).unwrap_err().code(), "invalid", "{bad:?}");
    }
}

fn fixture_orphan(app: &App, key: &str, started_at: &str, state: State) {
    let dir = recording::recording_dir(&app.data_dir, key);
    std::fs::create_dir_all(&dir).unwrap();
    let mut w = recorder::WavWriter::create(&dir.join("mic.wav")).unwrap();
    w.write(&vec![100i16; 32_000]).unwrap();
    drop(w);
    let stream = recorder::StreamMeta {
        file: "mic.wav".into(),
        device: "dev_x".into(),
        description: String::new(),
        is_monitor: false,
        first_sample_unix_ms: None,
        first_read_unix_ms: None,
        latency_ms: None,
        fragment_ms: 100,
        samples: 0,
        cuts: vec![],
        reconnects: 0,
    };
    Sidecar {
        schema: 1,
        state,
        key: key.into(),
        app_version: "0".into(),
        started_at: started_at.into(),
        started_unix_ms: 0,
        ended_at: None,
        duration_s: (state == State::Complete).then_some(2.0),
        sample_rate: 16_000,
        channels: 1,
        format: "s16le".into(),
        mic: Some(stream),
        sys: None,
        extra: serde_json::Value::Null,
    }
    .write(&dir)
    .unwrap();
}

#[test]
fn scan_orphans_sorts_newest_first_excludes_active_and_skips_junk() {
    let (_tmp, app) = app();
    assert!(recording::scan_orphans(&app, None).unwrap().is_empty(), "sem pasta recording/ ainda");
    fixture_orphan(&app, "call_2026-10-01_08-00-00", "2026-10-01T08:00:00", State::Recording);
    fixture_orphan(&app, "call_2026-10-02_08-00-00", "2026-10-02T08:00:00", State::Complete);
    fixture_orphan(&app, "call_2026-09-30_08-00-00", "2026-09-30T08:00:00", State::Recording);
    std::fs::create_dir_all(recording::recording_dir(&app.data_dir, "call_2026-10-03_08-00-00")).unwrap(); // sem sidecar
    std::fs::write(recording::recording_root(&app.data_dir).join("stray.txt"), b"x").unwrap();

    let all = recording::scan_orphans(&app, None).unwrap();
    let keys: Vec<_> = all.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys, ["call_2026-10-02_08-00-00", "call_2026-10-01_08-00-00", "call_2026-09-30_08-00-00"]);
    // `recording`: duração pelo tamanho do WAV (32000 amostras = 2 s); `complete`: pelo sidecar
    assert_eq!((all[0].state, all[0].duration_s), (State::Complete, 2.0));
    assert_eq!((all[1].state, all[1].duration_s), (State::Recording, 2.0));
    assert_eq!((all[1].mic_device.as_deref(), all[1].sys_device.as_deref(), all[1].intent.is_none()), (Some("dev_x"), None, true));
    assert!(all[1].size_bytes > 64_000);

    let rest = recording::scan_orphans(&app, Some("call_2026-10-02_08-00-00")).unwrap();
    assert_eq!(rest.len(), 2);
    assert!(rest.iter().all(|o| o.key != "call_2026-10-02_08-00-00"));
}

#[test]
fn running_recording_is_never_an_orphan_and_cannot_be_recovered_or_discarded() {
    let (_tmp, app) = app();
    let rec = begin(&app, Meta::default());
    let key = rec.key().to_string();
    // a trava da pasta vale mesmo que o chamador esqueça o `exclude`
    assert!(recording::scan_orphans(&app, None).unwrap().is_empty());
    assert!(recording::scan_orphans(&app, Some(&key)).unwrap().is_empty());
    assert_eq!(recording::recover(&app, &key, &mut |_| {}).unwrap_err().code(), "conflict");
    assert_eq!(recording::discard(&app, &key).unwrap_err().code(), "conflict");
    assert!(rec.dir().join("mic.wav").is_file(), "o WAV vivo não foi reparado nem apagado");
    // parada limpa: a trava cai e a pasta passa a ser órfã `complete` até a finalização
    std::thread::sleep(Duration::from_millis(700));
    let stopped = rec.stop().unwrap();
    assert_eq!(recording::scan_orphans(&app, None).unwrap().len(), 1);
    assert!(recording::finalize(&app, &stopped.key, &mut |_| {}).is_ok());
}

#[test]
fn only_one_finalize_runs_per_recording_and_busy_folders_are_not_orphans() {
    use std::fs::OpenOptions;
    let (_tmp, app) = app();
    let stopped = record_for(&app, Meta::default(), 700);
    // simula outra finalização em andamento segurando a trava
    let other = OpenOptions::new().create(true).write(true).truncate(false).open(stopped.dir.join(".lock")).unwrap();
    other.lock().unwrap();
    assert!(recording::scan_orphans(&app, None).unwrap().is_empty());
    for r in [recording::finalize(&app, &stopped.key, &mut |_| {}), recording::recover(&app, &stopped.key, &mut |_| {})] {
        assert_eq!(r.unwrap_err().code(), "conflict");
    }
    assert_eq!(recording::discard(&app, &stopped.key).unwrap_err().code(), "conflict");
    assert!(stopped.dir.join("mic.wav").is_file() && stopped.dir.join("recording.json").is_file());
    let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    assert!(!lib.root().join(&stopped.key).exists() && lib.calls(ClientFilter::Any).unwrap().is_empty());
    // a outra terminou: agora funciona (e a pasta volta a ser oferecida)
    drop(other);
    assert_eq!(recording::scan_orphans(&app, None).unwrap().len(), 1);
    assert!(recording::finalize(&app, &stopped.key, &mut |_| {}).is_ok());
}

// ------------------------------------------------------------------ queda → reparo → FLAC (arquivos montados à mão)

#[test]
fn every_crash_shape_repairs_to_a_flac_with_all_the_frames() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let tone: Vec<i16> = (0..48_000).map(|i| ((i as f64 * 0.07).sin() * 9000.0) as i16).collect();
    let make = |name: &str| dir.path().join(format!("{name}.wav"));
    let flac_n = |wav: &Path| {
        let out = wav.with_extension("flac");
        core_lib::audio::wav_to_flac(wav, &out, &mut |_, _| {}).unwrap();
        flac_samples(&out)
    };

    // A: tamanhos 0 (nunca corrigido): o conversor recusa ("no audio")
    let a = make("a");
    let mut w = recorder::WavWriter::create(&a).unwrap();
    w.write(&tone).unwrap();
    drop(w);
    assert!(core_lib::audio::wav_to_flac(&a, &a.with_extension("flac"), &mut |_, _| {}).is_err());
    assert_eq!(recorder::repair_wav(&a).unwrap().samples, 48_000);
    assert_eq!(flac_n(&a), 48_000);

    // B: 0xFFFFFFFF (estilo pipe) + cortada no meio de uma amostra
    let b = make("b");
    let mut bytes = recorder::wav::canonical_header(u32::MAX).to_vec();
    bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend(tone.iter().flat_map(|s| s.to_le_bytes()));
    bytes.push(0x55);
    std::fs::write(&b, &bytes).unwrap();
    assert!(core_lib::audio::wav_to_flac(&b, &b.with_extension("flac"), &mut |_, _| {}).is_err());
    assert_eq!(recorder::repair_wav(&b).unwrap().dropped_bytes, 1);
    assert_eq!(flac_n(&b), 48_000);

    // C: cabeçalho velho (1 s de 3 s): o conversor ACEITA e perde 2 s em silêncio — o caso perigoso
    let c = make("c");
    let mut w = recorder::WavWriter::create(&c).unwrap();
    w.write(&tone[..16_000]).unwrap();
    w.sync().unwrap();
    w.patch_header().unwrap();
    w.write(&tone[16_000..]).unwrap();
    drop(w);
    let cf = c.with_extension("flac");
    core_lib::audio::wav_to_flac(&c, &cf, &mut |_, _| {}).unwrap();
    assert_eq!(flac_samples(&cf), 16_000, "sem reparo faltam 2 s");
    assert!(recorder::repair_wav(&c).unwrap().header_was_stale);
    assert_eq!(flac_n(&c), 48_000);

    // D: tamanhos certos, arquivo cortado no meio de uma amostra
    let d = make("d");
    let mut w = recorder::WavWriter::create(&d).unwrap();
    w.write(&tone).unwrap();
    w.finish().unwrap();
    std::fs::OpenOptions::new().append(true).open(&d).unwrap().write_all(&[1]).unwrap();
    assert_eq!(recorder::repair_wav(&d).unwrap().samples, 48_000);
    assert_eq!(flac_n(&d), 48_000);

    // E: tamanhos certos, mas faltam 1000 bytes no fim (o disco não recebeu tudo): erro no conversor
    let e = make("e");
    let mut w = recorder::WavWriter::create(&e).unwrap();
    w.write(&tone).unwrap();
    w.finish().unwrap();
    let len = std::fs::metadata(&e).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&e).unwrap().set_len(len - 1000).unwrap();
    assert!(core_lib::audio::wav_to_flac(&e, &e.with_extension("flac"), &mut |_, _| {}).is_err());
    assert_eq!(recorder::repair_wav(&e).unwrap().samples, 47_500);
    assert_eq!(flac_n(&e), 47_500);
}

#[test]
fn target_gone_falls_back_to_inbox_and_no_client() {
    let (tmp, app) = app();
    let company = app.add_library("Empresa", &tmp.path().join("empresa")).unwrap();
    let client = app.open_library(company.id).unwrap().add_client("Cliente").unwrap();
    let meta = Meta { library_id: Some(company.id), client_id: Some(client.id), title: Some("Alvo".into()), ..Meta::default() };
    let stopped = record_for(&app, meta, 800);
    assert_eq!(stopped.sidecar.extra["intent"]["library_id"], company.id);
    app.remove_library(company.id).unwrap();
    let call = recording::finalize(&app, &stopped.key, &mut |_| {}).unwrap();
    assert_eq!(call.library_id, app.inbox_id().unwrap());
    let d = app.open_library(call.library_id).unwrap().call_detail(call.call_id, None).unwrap();
    assert_eq!((d.summary.client_id, d.summary.title.as_str()), (None, "Alvo"));
}

#[test]
fn library_folder_missing_also_falls_back_to_inbox() {
    let (tmp, app) = app();
    let company = app.add_library("Empresa", &tmp.path().join("empresa")).unwrap();
    let stopped = record_for(&app, Meta { library_id: Some(company.id), ..Meta::default() }, 700);
    std::fs::remove_dir_all(tmp.path().join("empresa")).unwrap(); // pendrive desmontado
    let call = recording::finalize(&app, &stopped.key, &mut |_| {}).unwrap();
    assert_eq!(call.library_id, app.inbox_id().unwrap());
    assert!(!tmp.path().join("empresa").exists(), "nada foi recriado no destino que sumiu");
}

#[test]
fn update_changes_target_title_speakers_and_language_while_recording() {
    let (tmp, app) = app();
    let company = app.add_library("Empresa", &tmp.path().join("empresa")).unwrap();
    let client = app.open_library(company.id).unwrap().add_client("Cliente").unwrap();
    let mut rec = begin(&app, Meta { title: Some("Antes".into()), expected_speakers: Some(2), ..Meta::default() });
    let new = Meta { library_id: Some(company.id), client_id: Some(client.id), title: Some("Depois".into()), expected_speakers: None, language: Some("es".into()) };
    let intent = rec.update(&app, &new).unwrap();
    assert_eq!((intent.library_id, intent.client_id, intent.expected_speakers), (company.id, Some(client.id), None));
    assert_eq!(rec.info().intent.title, "Depois");
    assert_eq!(Sidecar::read(rec.dir()).unwrap().extra["intent"]["title"], "Depois");
    // inválido: nada muda
    assert_eq!(rec.update(&app, &Meta { expected_speakers: Some(99), ..Meta::default() }).unwrap_err().code(), "invalid");
    assert_eq!(rec.intent().title, "Depois");
    std::thread::sleep(Duration::from_millis(700));
    let key = rec.stop().unwrap().key;
    let call = recording::finalize(&app, &key, &mut |_| {}).unwrap();
    assert_eq!(call.library_id, company.id);
    let lib = app.open_library(company.id).unwrap();
    let d = lib.call_detail(call.call_id, None).unwrap();
    assert_eq!((d.summary.client_id, d.summary.title.as_str(), d.language.as_deref(), d.expected_speakers), (Some(client.id), "Depois", Some("es"), None));
    assert!(lib.root().join(d.audio.mic_path.unwrap()).starts_with(lib.root().join(&client.slug)), "pasta do cliente");
}

#[test]
fn key_already_in_the_library_is_a_conflict_and_leaves_the_recording_intact() {
    let (_tmp, app) = app();
    let rec = begin(&app, Meta::default());
    let key = rec.key().to_string();
    insert_pending(&app, &key);
    std::thread::sleep(Duration::from_millis(700));
    let dir = rec.stop().unwrap().dir;
    let err = recording::finalize(&app, &key, &mut |_| {}).unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(dir.join("mic.wav").is_file() && dir.join("sys.wav").is_file() && dir.join("recording.json").is_file());
    let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    assert!(!lib.root().join(&key).exists(), "nada parcial na biblioteca");
    assert_eq!(recording::scan_orphans(&app, None).unwrap().len(), 1, "continua oferecida para recuperar");
}

#[cfg(unix)]
#[test]
fn failure_while_converting_leaves_recording_and_library_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, app) = app();
    let stopped = record_for(&app, Meta::default(), 700);
    // a pasta da chamada já existe (vazia, então é aceita) mas ninguém pode escrever nela
    let blocked = app.data_dir.join("inbox").join(&stopped.key);
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::File::create(blocked.join("probe")).is_ok() {
        return; // rodando como root: a permissão não barra; nada a testar
    }
    let err = recording::finalize(&app, &stopped.key, &mut |_| {}).unwrap_err();
    assert!(matches!(err.code(), "io" | "audio"), "{}", err.code());
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(std::fs::read_dir(&blocked).unwrap().count(), 0, "sem FLAC nem sidecar pela metade");
    assert!(stopped.dir.join("mic.wav").is_file() && stopped.dir.join("sys.wav").is_file());
    let lib = app.open_library(app.inbox_id().unwrap()).unwrap();
    assert_eq!(lib.calls(ClientFilter::Any).unwrap().len(), 0);
    // com a pasta liberada, a mesma gravação finaliza normalmente
    assert!(recording::finalize(&app, &stopped.key, &mut |_| {}).is_ok());
}

#[test]
fn failed_start_leaves_no_folder_and_no_last_used() {
    let (_tmp, app) = app();
    let req = StartRequest { mic: StreamChoice::Named("nope".into()), ..Default::default() };
    assert_eq!(recording::start(&app, fake(), req).err().unwrap().code(), "device_not_found");
    assert!(rec_dirs(&app).is_empty());
    assert_eq!(recording::last_used(&app).unwrap(), LastUsed::default());
    let off = StartRequest { mic: StreamChoice::Off, sys: StreamChoice::Off, ..Default::default() };
    assert_eq!(recording::start(&app, fake(), off).err().unwrap().code(), "invalid");
    assert!(rec_dirs(&app).is_empty());
}

#[test]
fn intent_resolve_rules() {
    let (tmp, app) = app();
    let inbox = app.inbox_id().unwrap();
    let company = app.add_library("Empresa", &tmp.path().join("empresa")).unwrap();
    let other = app.add_library("Outra", &tmp.path().join("outra")).unwrap();
    let client = app.open_library(company.id).unwrap().add_client("Cliente").unwrap();
    let r = |m: Meta| recording::Intent::resolve(&app, &m);

    let d = r(Meta::default()).unwrap();
    assert_eq!((d.library_id, d.client_id, d.title.as_str(), d.expected_speakers, d.language), (inbox, None, "", None, None));
    assert_eq!(r(Meta { title: Some("  a   b ".into()), language: Some(" PT_br ".into()), ..Meta::default() }).unwrap().title, "a b");
    assert_eq!(r(Meta { language: Some("PT_br".into()), ..Meta::default() }).unwrap().language.as_deref(), Some("pt"));
    assert_eq!(r(Meta { language: Some("".into()), ..Meta::default() }).unwrap().language, None);
    assert_eq!(r(Meta { language: Some("fr".into()), ..Meta::default() }).unwrap_err().code(), "invalid");
    assert_eq!(r(Meta { expected_speakers: Some(0), ..Meta::default() }).unwrap_err().code(), "invalid");
    assert_eq!(r(Meta { expected_speakers: Some(21), ..Meta::default() }).unwrap_err().code(), "invalid");
    assert_eq!(r(Meta { expected_speakers: Some(20), ..Meta::default() }).unwrap().expected_speakers, Some(20));
    assert_eq!(r(Meta { title: Some("x".repeat(201)), ..Meta::default() }).unwrap_err().code(), "invalid");
    assert_eq!(r(Meta { library_id: Some(9999), ..Meta::default() }).unwrap_err().code(), "not_found");
    // cliente: precisa existir NA biblioteca; a inbox não tem clientes
    let ok = r(Meta { library_id: Some(company.id), client_id: Some(client.id), ..Meta::default() }).unwrap();
    assert_eq!(ok.client_id, Some(client.id));
    assert_eq!(r(Meta { library_id: Some(other.id), client_id: Some(client.id), ..Meta::default() }).unwrap_err().code(), "invalid");
    assert_eq!(r(Meta { library_id: Some(company.id), client_id: Some(777), ..Meta::default() }).unwrap_err().code(), "invalid");
    assert_eq!(r(Meta { client_id: Some(client.id), ..Meta::default() }).unwrap_err().code(), "invalid");
    // biblioteca cadastrada mas sem `library.db` (indisponível)
    std::fs::remove_file(tmp.path().join("outra").join("library.db")).unwrap();
    assert_eq!(r(Meta { library_id: Some(other.id), ..Meta::default() }).unwrap_err().code(), "not_found");
}

#[test]
fn transcription_language_setting_is_the_fallback_language() {
    let (_tmp, app) = app();
    app.set_setting("transcription_language", Some("es-419")).unwrap();
    let stopped = record_for(&app, Meta::default(), 700);
    let call = recording::finalize(&app, &stopped.key, &mut |_| {}).unwrap();
    let d = app.open_library(call.library_id).unwrap().call_detail(call.call_id, None).unwrap();
    assert_eq!(d.language.as_deref(), Some("es"));
    let stopped = record_for(&app, Meta::default(), 700);
    app.set_setting("transcription_language", None).unwrap();
    let call = recording::finalize(&app, &stopped.key, &mut |_| {}).unwrap();
    let d = app.open_library(call.library_id).unwrap().call_detail(call.call_id, None).unwrap();
    assert_eq!(d.language.as_deref(), Some("pt"));
}

#[test]
fn active_recording_can_live_in_shared_state() {
    fn send<T: Send>() {}
    send::<ActiveRecording>();
}
