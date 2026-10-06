//! Bootstrap REAL do runtime (opt-in: `cargo test -- --ignored`; usa a rede: uv, Python 3.12.15 e as rodas do
//! lock, ~600 MB no disco). Num diretório temporário; nada fora dele.
use std::sync::atomic::AtomicBool;

use core_lib::transcription::engine::{Engine, Flow, ProcessEngine, Terminal, WorkerLaunch};
use core_lib::transcription::protocol::{FromWorker, ToWorker};
use core_lib::transcription::runtime;

#[test]
#[ignore = "baixa uv, Python e pacotes pela rede"]
fn real_bootstrap_installs_a_working_runtime() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(runtime::status(dir.path()).unwrap().state, "missing");
    let mut steps = Vec::new();
    runtime::ensure(dir.path(), &mut |p| steps.push(p.step.clone()), &AtomicBool::new(false)).unwrap();
    assert_eq!(steps, ["download_uv", "install_python", "create_venv", "sync_packages", "verify"]);
    let st = runtime::status(dir.path()).unwrap();
    assert_eq!(st.state, "ready");
    let p = runtime::paths(dir.path());
    assert!(p.python.exists() && p.worker.exists() && !p.root.join(".uv-cache").exists() && !p.root.join("venv.new").exists());
    // idempotente: pronto = nada a fazer
    let mut again = 0;
    runtime::ensure(dir.path(), &mut |_| again += 1, &AtomicBool::new(false)).unwrap();
    assert_eq!(again, 0);
    // o worker REAL sobe a partir do venv e mede energia de um áudio sintético
    let audio = dir.path().join("a.flac");
    let wav = dir.path().join("a.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for i in 0..16_000 * 3 {
        w.write_sample((8000.0 * (i as f32 * 0.05).sin()) as i16).unwrap();
    }
    w.finalize().unwrap();
    core_lib::audio::wav_to_flac(&wav, &audio, &mut |_, _| {}).unwrap();
    let mut engine = ProcessEngine::spawn(WorkerLaunch { python: p.python.clone(), script: p.worker.clone(), fake: false, low_priority: true, kill_after_s: 20 }).unwrap();
    let Some(FromWorker::Hello { faster_whisper, sherpa_onnx, fake, .. }) = engine.hello().cloned() else { panic!("no hello") };
    assert!(!fake && faster_whisper.is_some() && sherpa_onnx.is_some());
    let t = engine.execute(&ToWorker::Energy { id: "e".into(), audio: audio.display().to_string(), step_ms: 100, mute: vec![] }, &mut |_| Flow::Continue).unwrap();
    let Terminal::Result(FromWorker::Result { db: Some(db), .. }) = t else { panic!("no energy result") };
    assert!((29..=31).contains(&db.len()), "{} steps", db.len());
    assert!(db.iter().all(|d| *d > -30.0 && *d < 0.0));
    engine.shutdown();
    // um manifest de outra época = desatualizado
    std::fs::write(&p.manifest, br#"{"runtime_version":0,"uv":"","python":"","lock_sha256":"","worker_sha256":"","installed_at":"x"}"#).unwrap();
    assert_eq!(runtime::status(dir.path()).unwrap().state, "outdated");
}

#[test]
#[ignore = "baixa uv, Python e pacotes pela rede"]
fn cancelled_bootstrap_leaves_no_half_installed_venv() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let e = runtime::ensure(dir.path(), &mut |p| { if p.step == "install_python" { cancel.store(true, std::sync::atomic::Ordering::Relaxed) } }, &cancel).unwrap_err();
    assert_eq!(e.code(), "setup_cancelled");
    assert_ne!(runtime::status(dir.path()).unwrap().state, "ready");
    assert!(!runtime::paths(dir.path()).root.join("venv.new").exists());
    assert!(!runtime::paths(dir.path()).manifest.exists());
}
