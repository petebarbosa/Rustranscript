//! `ProcessEngine` com o `worker.py --fake` de verdade (opt-in: `cargo test -- --ignored`; precisa de `python3`
//! no PATH, ou do interpretador indicado em `TRANSCRICOES_FAKE_WORKER`). Sem rede, sem modelos, áudio sintético.
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use core_lib::transcription::engine::{Engine, FakeEngine, Flow, ProcessEngine, Terminal, TICK_STAGE, WorkerLaunch};
use core_lib::transcription::protocol::{FromWorker, ToWorker};
use core_lib::transcription::runtime::WORKER_PY;

/// Os testes mexem em variáveis de ambiente herdadas pelo filho: um por vez.
static SERIAL: Mutex<()> = Mutex::new(());

fn flac(dir: &Path, name: &str, secs: u32) -> PathBuf {
    let wav = dir.join(format!("{name}.wav"));
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for i in 0..16_000 * secs {
        w.write_sample(((i % 50) as i16) - 25).unwrap();
    }
    w.finalize().unwrap();
    let out = dir.join(format!("{name}.flac"));
    core_lib::audio::wav_to_flac(&wav, &out, &mut |_, _| {}).unwrap();
    out
}

fn python() -> PathBuf {
    std::env::var("TRANSCRICOES_FAKE_WORKER").ok().map(PathBuf::from).filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("python3"))
}

fn engine(dir: &Path) -> ProcessEngine {
    let script = dir.join("worker.py");
    std::fs::write(&script, WORKER_PY).unwrap();
    ProcessEngine::spawn(WorkerLaunch { python: python(), script, fake: true, low_priority: true, kill_after_s: 20 }).unwrap()
}

fn transcribe(id: &str, audio: &Path, track: &str, start_s: f64) -> ToWorker {
    ToWorker::Transcribe {
        id: id.into(),
        audio: audio.display().to_string(),
        track: track.into(),
        model_dir: "/unused".into(),
        language: Some("pt".into()),
        hotwords: None,
        beam_size: 5,
        threads: 1,
        word_timestamps: true,
        vad_min_silence_ms: 500,
        start_s,
    }
}

fn diarize(id: &str, audio: &Path, clusters: Option<u32>) -> ToWorker {
    ToWorker::Diarize { id: id.into(), audio: audio.display().to_string(), seg_model: "/x".into(), emb_model: "/y".into(), num_clusters: clusters, threshold: 0.7, threads: 1 }
}

fn alive(pid: u32) -> bool {
    // zumbi também conta como "não vivo"
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

fn wait_dead(pid: u32) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn collect(e: &mut dyn Engine, req: &ToWorker) -> (Vec<serde_json::Value>, serde_json::Value) {
    let mut events = Vec::new();
    let term = e
        .execute(req, &mut |m| {
            if !matches!(m, FromWorker::Progress { stage, .. } if stage == TICK_STAGE) {
                events.push(serde_json::to_value(m).unwrap());
            }
            Flow::Continue
        })
        .unwrap();
    let Terminal::Result(r) = term else { panic!("expected a result") };
    (events, serde_json::to_value(r).unwrap())
}

/// Números iguais a menos de 1 ms (o worker arredonda tempos).
fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value::*;
    match (a, b) {
        (Number(x), Number(y)) => (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-3,
        (Array(x), Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Object(x), Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

#[test]
#[ignore = "precisa de python3"]
fn fake_worker_matches_the_in_memory_fake_engine() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let sys = flac(dir.path(), "sys", 40);
    let mic = flac(dir.path(), "mic", 40);
    let mut real = engine(dir.path());
    assert!(matches!(real.hello(), Some(FromWorker::Hello { protocol: 1, fake: true, .. })));
    let mut fake = FakeEngine::new();
    let requests = [
        transcribe("a", &sys, "sys", 0.0),
        transcribe("b", &mic, "mic", 0.0),
        transcribe("c", &sys, "sys", 12.0),
        diarize("d", &sys, None),
        diarize("e", &sys, Some(3)),
        ToWorker::Energy { id: "f".into(), audio: sys.display().to_string(), step_ms: 100 },
        ToWorker::Energy { id: "g".into(), audio: mic.display().to_string(), step_ms: 100 },
    ];
    for req in &requests {
        let (ev_real, mut res_real) = collect(&mut real, req);
        let (ev_fake, mut res_fake) = collect(&mut fake, req);
        // `seconds` é o tempo gasto (o worker mede; o motor em memória diz a duração do áudio): informativo
        res_real["seconds"] = serde_json::Value::Null;
        res_fake["seconds"] = serde_json::Value::Null;
        // o worker pode emitir mais eventos de progresso; os segmentos e o resultado têm de ser iguais
        let segs = |v: &[serde_json::Value]| v.iter().filter(|m| m["type"] == "segment").cloned().collect::<Vec<_>>();
        assert!(same(&serde_json::Value::Array(segs(&ev_real)), &serde_json::Value::Array(segs(&ev_fake))), "segments differ for {req:?}");
        assert!(same(&res_real, &res_fake), "result differs for {req:?}:\n{res_real}\n{res_fake}");
    }
    real.shutdown();
}

#[test]
#[ignore = "precisa de python3"]
fn cooperative_cancel_keeps_the_worker_and_hard_cancel_restarts_it() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let audio = flac(dir.path(), "sys", 60);
    // SAFETY: protegido por SERIAL; só este teste mexe nessa variável e os outros não rodam ao mesmo tempo.
    unsafe { std::env::set_var("TRANSCRICOES_FAKE_DELAY_MS", "150") };
    let mut e = engine(dir.path());
    let pid = e.pid().unwrap();
    let mut seen = 0;
    let t = e
        .execute(&transcribe("x", &audio, "sys", 0.0), &mut |m| {
            if matches!(m, FromWorker::Segment { .. }) {
                seen += 1;
            }
            if seen >= 2 { Flow::Cancel } else { Flow::Continue }
        })
        .unwrap();
    assert!(matches!(t, Terminal::Cancelled { .. }), "{t:?}");
    assert!((2..12).contains(&seen), "stopped early, saw {seen}");
    assert_eq!(e.pid(), Some(pid), "transcribe cancel is cooperative: same process");
    // o mesmo processo ainda atende
    let (_, r) = collect(&mut e, &transcribe("y", &audio, "sys", 55.0));
    assert_eq!(r["segments"], 1);
    // diarize cancelado = SIGKILL do grupo; o próximo pedido sobe um worker novo
    let t = e.execute(&diarize("z", &audio, None), &mut |_| Flow::Cancel).unwrap();
    match t {
        Terminal::Cancelled { .. } => assert_ne!(e.pid(), Some(pid)),
        Terminal::Result(_) => {} // terminou antes do primeiro evento: nada a cancelar
    }
    unsafe { std::env::remove_var("TRANSCRICOES_FAKE_DELAY_MS") };
    let (_, r) = collect(&mut e, &diarize("w", &audio, None));
    assert!(r["turns"].is_array());
    e.shutdown();
}

#[test]
#[ignore = "precisa de python3"]
fn killed_worker_is_reported_and_restarted_on_the_next_request() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let audio = flac(dir.path(), "sys", 20);
    let mut e = engine(dir.path());
    let pid = e.pid().unwrap();
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    assert!(wait_dead(pid));
    let err = e.execute(&transcribe("a", &audio, "sys", 0.0), &mut |_| Flow::Continue).unwrap_err();
    assert_eq!(err.code(), "worker_crashed");
    let (_, r) = collect(&mut e, &transcribe("b", &audio, "sys", 0.0));
    assert_eq!(r["segments"], 4);
    assert_ne!(e.pid(), Some(pid));
    // queda no meio de um pedido
    unsafe { std::env::set_var("TRANSCRICOES_FAKE_DELAY_MS", "200") };
    let mut e2 = engine(dir.path());
    unsafe { std::env::remove_var("TRANSCRICOES_FAKE_DELAY_MS") };
    let pid2 = e2.pid().unwrap();
    let mut n = 0;
    let err = e2
        .execute(&transcribe("c", &audio, "sys", 0.0), &mut |m| {
            if matches!(m, FromWorker::Segment { .. }) {
                n += 1;
                if n == 2 {
                    unsafe { libc::kill(pid2 as i32, libc::SIGKILL) };
                }
            }
            Flow::Continue
        })
        .unwrap_err();
    assert_eq!(err.code(), "worker_crashed");
    let (_, r) = collect(&mut e2, &transcribe("d", &audio, "sys", 0.0));
    assert_eq!(r["segments"], 4);
}

#[test]
#[ignore = "precisa de python3"]
fn bad_request_and_missing_audio_are_typed_errors_not_crashes() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path());
    let pid = e.pid().unwrap();
    let err = e.execute(&transcribe("a", &dir.path().join("nope.flac"), "sys", 0.0), &mut |_| Flow::Continue).unwrap_err();
    assert!(matches!(err.code(), "audio_decode" | "job_failed"), "{err}");
    assert_eq!(e.pid(), Some(pid), "a non-fatal error keeps the worker");
}

#[test]
#[ignore = "precisa de python3"]
fn shutdown_ends_the_process_and_parent_death_kills_the_worker() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path());
    let pid = e.pid().unwrap();
    assert!(alive(pid));
    e.shutdown();
    assert!(wait_dead(pid));
    e.shutdown(); // idempotente
    // PR_SET_PDEATHSIG dispara quando a THREAD que fez o spawn termina (aqui sem `Drop`: `forget`)
    let path = dir.path().to_path_buf();
    let pid = std::thread::spawn(move || {
        let e = engine(&path);
        let pid = e.pid().unwrap();
        std::mem::forget(e);
        pid
    })
    .join()
    .unwrap();
    assert!(wait_dead(pid), "worker must not outlive the thread that spawned it");
}
