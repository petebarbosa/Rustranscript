//! Especificação executável da `Session` com o `FakeBackend` (tons sintéticos; nunca áudio real).
use std::sync::Arc;
use std::time::Duration;

use recorder::fake::{self, FakeBackend};
use recorder::sidecar::{MIC_WAV, SIDECAR_FILE, SYS_WAV, Sidecar, State};
use recorder::{Session, StartOptions, StreamChoice, repair_wav};

fn header_sizes(path: &std::path::Path) -> (u32, u32) {
    let b = std::fs::read(path).unwrap();
    (u32::from_le_bytes(b[4..8].try_into().unwrap()), u32::from_le_bytes(b[40..44].try_into().unwrap()))
}

#[test]
fn records_two_tracks_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = StartOptions::new(dir.path().join("rec"), "call_2026-10-02_09-00-00");
    opts.extra = serde_json::json!({"meta": {"title": "tom"}});
    let session = Session::start(Arc::new(FakeBackend::new()), opts).unwrap();
    assert!(Sidecar::read(session.dir()).unwrap().state == State::Recording);
    std::thread::sleep(Duration::from_millis(1500));
    let live = session.levels();
    assert!(live.mic.unwrap().peak > 0.1 && live.sys.unwrap().peak > 0.1);
    let status = session.status();
    assert!(status.elapsed_s > 1.0 && status.mic.as_ref().unwrap().alive);
    let dir = session.dir().to_path_buf();
    let sc = session.stop().unwrap();
    assert_eq!(sc.state, State::Complete);
    assert_eq!(Sidecar::read(&dir).unwrap().state, State::Complete);
    assert_eq!(sc.extra["meta"]["title"], "tom");
    assert!((sc.duration_s.unwrap() - 1.5).abs() < 0.4);
    let mic = sc.mic.as_ref().unwrap();
    assert_eq!(mic.device, fake::MIC);
    assert!(mic.first_sample_unix_ms.is_some() && sc.mic_offset_ms().unwrap().abs() < 200);
    for (name, samples) in [(MIC_WAV, mic.samples), (SYS_WAV, sc.sys.as_ref().unwrap().samples)] {
        let p = dir.join(name);
        let len = std::fs::metadata(&p).unwrap().len();
        assert_eq!(len, 44 + samples * 2);
        assert_eq!(header_sizes(&p), (36 + samples as u32 * 2, samples as u32 * 2));
    }
}

#[test]
fn unknown_device_fails_synchronously_and_leaves_no_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = StartOptions::new(dir.path().join("rec"), "call_2026-10-02_09-00-01");
    opts.mic = StreamChoice::Named("nope".into());
    let err = Session::start(Arc::new(FakeBackend::fast()), opts).err().unwrap();
    assert_eq!(err.code(), "device_not_found");
    assert!(!dir.path().join("rec").join(MIC_WAV).exists());
}

#[test]
fn drop_without_stop_is_recoverable_like_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("rec");
    let session = Session::start(Arc::new(FakeBackend::new()), StartOptions::new(&rec, "call_2026-10-02_09-00-02")).unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    drop(session);
    assert_eq!(Sidecar::read(&rec).unwrap().state, State::Recording);
    // simula queda no meio de uma amostra e cabeçalho velho
    let p = rec.join(MIC_WAV);
    let f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    use std::io::Write;
    (&f).write_all(&[1]).unwrap();
    let report = repair_wav(&p).unwrap();
    assert!(report.samples >= 16_000);
    assert_eq!(std::fs::metadata(&p).unwrap().len(), 44 + report.samples * 2);
    assert_eq!(header_sizes(&p).1 as u64, report.samples * 2);
    assert_eq!(repair_wav(&p).unwrap().dropped_bytes, 0, "idempotente");
    assert!(rec.join(SIDECAR_FILE).exists());
}

#[test]
fn flaky_mic_reconnects_and_records_a_cut() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = StartOptions::new(dir.path().join("rec"), "call_2026-10-02_09-00-03");
    opts.mic = StreamChoice::Named(fake::MIC_FLAKY.into());
    let session = Session::start(Arc::new(FakeBackend::new()), opts).unwrap();
    std::thread::sleep(Duration::from_millis(3500));
    let sc = session.stop().unwrap();
    let mic = sc.mic.unwrap();
    assert_eq!(mic.cuts.len(), 1);
    assert!(mic.cuts[0].reconnected && mic.reconnects == 1);
    // a lacuna foi preenchida: amostras ≈ tempo real
    assert!((mic.samples as f64 / 16_000.0 - sc.duration_s.unwrap()).abs() < 0.3);
}

// ------------------------------------------------------------------ mais cenários

use std::sync::atomic::{AtomicU32, Ordering};

use recorder::{CaptureBackend, CaptureStream, DeviceInfo, DeviceKind, Error, Monitor};

fn read_samples(path: &std::path::Path) -> Vec<i16> {
    hound::WavReader::open(path).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
}

/// Frequência estimada pelas passagens por zero (suficiente para distinguir 440 de 880 Hz).
fn zero_cross_hz(s: &[i16]) -> f64 {
    let crossings = s.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    crossings as f64 / 2.0 / (s.len() as f64 / 16_000.0)
}

#[test]
fn recorded_tones_have_expected_frequency_and_amplitude() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("rec");
    let session = Session::start(Arc::new(FakeBackend::new()), StartOptions::new(&rec, "call_2026-10-02_10-00-00")).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let sc = session.stop().unwrap();
    let (mic, sys) = (read_samples(&rec.join(MIC_WAV)), read_samples(&rec.join(SYS_WAV)));
    assert_eq!(mic.len() as u64, sc.mic.as_ref().unwrap().samples);
    assert!((zero_cross_hz(&mic) - 440.0).abs() < 15.0, "{}", zero_cross_hz(&mic));
    assert!((zero_cross_hz(&sys) - 880.0).abs() < 25.0, "{}", zero_cross_hz(&sys));
    let peak = mic.iter().map(|s| s.unsigned_abs()).max().unwrap() as f64 / 32768.0;
    assert!((peak - 0.25).abs() < 0.01, "{peak}");
    // os dois relógios nascem juntos
    assert_eq!(sc.sys.as_ref().unwrap().device, fake::MONITOR);
    assert!(sc.mic_offset_ms().unwrap().abs() < 200);
    assert_eq!((sc.sample_rate, sc.channels, sc.format.as_str()), (16_000, 1, "s16le"));
}

#[test]
fn off_track_creates_nothing_and_both_off_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("rec");
    let mut opts = StartOptions::new(&rec, "call_2026-10-02_10-00-01");
    opts.sys = StreamChoice::Off;
    let session = Session::start(Arc::new(FakeBackend::new()), opts).unwrap();
    assert!(session.status().sys.is_none() && session.levels().sys.is_none());
    std::thread::sleep(Duration::from_millis(300));
    let sc = session.stop().unwrap();
    assert!(sc.sys.is_none() && sc.mic.is_some());
    assert!(rec.join(MIC_WAV).is_file() && !rec.join(SYS_WAV).exists());

    let mut none = StartOptions::new(dir.path().join("rec2"), "call_2026-10-02_10-00-02");
    none.mic = StreamChoice::Off;
    none.sys = StreamChoice::Off;
    let err = Session::start(Arc::new(FakeBackend::fast()), none).err().unwrap();
    assert_eq!(err.code(), "invalid");
    assert!(!dir.path().join("rec2").exists());
}

#[test]
fn failed_start_removes_what_it_created_but_keeps_a_preexisting_dir() {
    let dir = tempfile::tempdir().unwrap();
    // mic abre, sys falha: o WAV do mic criado antes do erro não pode sobrar
    let rec = dir.path().join("novo");
    let mut opts = StartOptions::new(&rec, "call_2026-10-02_10-00-03");
    opts.sys = StreamChoice::Named("nope.monitor".into());
    assert_eq!(Session::start(Arc::new(FakeBackend::new()), opts).err().unwrap().code(), "device_not_found");
    assert!(!rec.exists(), "pasta criada pela sessão some");

    let existing = dir.path().join("existente");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("keep.txt"), b"x").unwrap();
    let mut opts = StartOptions::new(&existing, "call_2026-10-02_10-00-04");
    opts.mic = StreamChoice::Named("nope".into());
    assert!(Session::start(Arc::new(FakeBackend::new()), opts).is_err());
    let left: Vec<_> = std::fs::read_dir(&existing).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("keep.txt")]);
}

#[test]
fn folder_already_used_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("rec");
    let s = Session::start(Arc::new(FakeBackend::new()), StartOptions::new(&rec, "call_2026-10-02_10-00-05")).unwrap();
    let err = Session::start(Arc::new(FakeBackend::new()), StartOptions::new(&rec, "call_2026-10-02_10-00-05")).err().unwrap();
    assert_eq!(err.code(), "invalid");
    assert!(rec.join(MIC_WAV).is_file(), "a gravação em andamento não foi tocada");
    drop(s);
}

#[test]
fn update_extra_rewrites_the_sidecar_while_recording() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("rec");
    let mut opts = StartOptions::new(&rec, "call_2026-10-02_10-00-06");
    opts.extra = serde_json::json!({"intent": {"title": "antes"}});
    let session = Session::start(Arc::new(FakeBackend::new()), opts).unwrap();
    session.update_extra(serde_json::json!({"intent": {"title": "depois"}})).unwrap();
    let live = Sidecar::read(&rec).unwrap();
    assert_eq!((live.state, live.extra["intent"]["title"].as_str()), (State::Recording, Some("depois")));
    assert!(!rec.join("recording.json.part").exists());
    let sc = session.stop().unwrap();
    assert_eq!(sc.extra["intent"]["title"], "depois");
}

#[test]
fn levels_report_peak_rms_and_digital_silence() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = StartOptions::new(dir.path().join("rec"), "call_2026-10-02_10-00-07");
    opts.mic = StreamChoice::Named(fake::MIC_SILENT.into());
    let session = Session::start(Arc::new(FakeBackend::new()), opts).unwrap();
    std::thread::sleep(Duration::from_millis(1300));
    let l = session.levels();
    let (mic, sys) = (l.mic.unwrap(), l.sys.unwrap());
    assert_eq!(mic.peak, 0.0);
    assert!(mic.silent_s > 0.8, "{}", mic.silent_s);
    assert!((sys.peak - 0.25).abs() < 0.01 && (sys.rms - 0.25 / 2f32.sqrt()).abs() < 0.02, "{sys:?}");
    assert!(mic.alive && sys.alive && sys.silent_s == 0.0);
    session.stop().unwrap();
}

#[test]
fn monitor_measures_levels_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let before = std::fs::read_dir(dir.path()).unwrap().count();
    let m = Monitor::start(Arc::new(FakeBackend::new()), &StreamChoice::Default, &StreamChoice::Off).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let l = m.levels();
    assert!(l.sys.is_none() && l.mic.unwrap().peak > 0.2);
    m.stop();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);

    let err = Monitor::start(Arc::new(FakeBackend::fast()), &StreamChoice::Named("nope".into()), &StreamChoice::Default).err().unwrap();
    assert_eq!(err.code(), "device_not_found");
}

/// Backend com roteiro: o 1º fluxo morre depois de `fail_after` leituras; o que acontece ao religar
/// depende de `Mode`.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Religar nunca mais funciona (cabo arrancado).
    GoneForever,
    /// O dispositivo pedido sumiu, mas o padrão existe: religa nele (`device_changed`).
    FallsBackToDefault,
}

struct Scripted {
    mode: Mode,
    fail_after: u32,
    opens: AtomicU32,
}

struct ScriptedStream {
    info: DeviceInfo,
    reads: u32,
    fail_after: Option<u32>,
}

fn dev(name: &str) -> DeviceInfo {
    DeviceInfo { name: name.into(), description: format!("{name} desc"), is_monitor: false, is_default: false }
}

impl CaptureBackend for Scripted {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn list_devices(&self) -> recorder::Result<Vec<DeviceInfo>> {
        Ok(vec![dev("usb_mic"), dev("builtin")])
    }
    fn open(&self, _kind: DeviceKind, device: Option<&str>, _fragment_ms: u32) -> recorder::Result<Box<dyn CaptureStream>> {
        let n = self.opens.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            return Ok(Box::new(ScriptedStream { info: dev("usb_mic"), reads: 0, fail_after: Some(self.fail_after) }));
        }
        match (self.mode, device) {
            (Mode::FallsBackToDefault, None) => Ok(Box::new(ScriptedStream { info: dev("builtin"), reads: 0, fail_after: None })),
            _ => Err(Error::DeviceNotFound(device.unwrap_or("default").into())),
        }
    }
}

impl CaptureStream for ScriptedStream {
    fn device(&self) -> &DeviceInfo {
        &self.info
    }
    fn read(&mut self, buf: &mut [i16]) -> recorder::Result<()> {
        if self.fail_after.is_some_and(|n| self.reads >= n) {
            return Err(Error::Capture("unplugged".into()));
        }
        self.reads += 1;
        std::thread::sleep(Duration::from_millis(buf.len() as u64 * 1000 / 16_000));
        buf.fill(5000);
        Ok(())
    }
    fn latency(&self) -> Option<Duration> {
        Some(Duration::from_millis(30))
    }
}

fn scripted_session(mode: Mode, dir: &std::path::Path) -> Session {
    let backend = Arc::new(Scripted { mode, fail_after: 10, opens: AtomicU32::new(0) });
    let mut opts = StartOptions::new(dir, "call_2026-10-02_10-00-08");
    opts.mic = StreamChoice::Named("usb_mic".into());
    opts.sys = StreamChoice::Off;
    Session::start(backend, opts).unwrap()
}

#[test]
fn cut_is_filled_with_silence_matching_the_wall_clock() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("rec");
    let session = scripted_session(Mode::FallsBackToDefault, &rec);
    std::thread::sleep(Duration::from_millis(500));
    assert!(session.levels().mic.unwrap().alive);
    std::thread::sleep(Duration::from_millis(1700)); // a falha vem em ~1 s, a reconexão ~0,1 s depois
    let status = session.status();
    assert!(status.mic.as_ref().unwrap().alive && status.cuts == 1, "{status:?}");
    let elapsed = status.elapsed_s;
    let sc = session.stop().unwrap();
    let mic = sc.mic.unwrap();
    assert_eq!((mic.cuts.len(), mic.reconnects), (1, 1));
    let cut = &mic.cuts[0];
    assert!(cut.reconnected);
    assert_eq!(cut.reason, "device_changed", "o `usb_mic` sumiu e voltou o padrão");
    assert_eq!((mic.device.as_str(), mic.description.as_str()), ("builtin", "builtin desc"));
    assert!((cut.at_sample as i64 - 16_000).abs() < 1_700, "{}", cut.at_sample);
    assert!(cut.gap_ms >= 100 && cut.gap_ms < 1_500, "{}", cut.gap_ms);
    // trilha ≈ relógio de parede, e a lacuna é de zeros
    let samples = read_samples(&rec.join(MIC_WAV));
    assert!((samples.len() as f64 / 16_000.0 - elapsed).abs() < 0.5, "{} vs {elapsed}", samples.len());
    let gap = &samples[cut.at_sample as usize..cut.at_sample as usize + (cut.gap_ms as usize * 16)];
    assert!(gap.iter().all(|&s| s == 0));
    assert!(samples[..cut.at_sample as usize].iter().all(|&s| s == 5000));
    assert_eq!(samples.last(), Some(&5000));
}

#[test]
fn stopping_while_disconnected_pads_to_the_clock_and_marks_cut_not_reconnected() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("rec");
    let session = scripted_session(Mode::GoneForever, &rec);
    std::thread::sleep(Duration::from_millis(2200));
    let st = session.status();
    assert!(!st.mic.as_ref().unwrap().alive, "sem dispositivo");
    assert!(!session.levels().mic.unwrap().alive);
    let sc = session.stop().unwrap();
    let mic = sc.mic.unwrap();
    assert_eq!((mic.cuts.len(), mic.reconnects), (1, 0));
    assert!(!mic.cuts[0].reconnected);
    let secs = mic.samples as f64 / 16_000.0;
    assert!((secs - 2.2).abs() < 0.5, "{secs}");
    assert!((mic.cuts[0].gap_ms as f64 / 1000.0 - (secs - 1.0)).abs() < 0.3);
    assert_eq!(read_samples(&rec.join(MIC_WAV)).len() as u64, mic.samples);
}

#[test]
fn handles_can_cross_threads() {
    // o shell guarda a sessão e o monitor em estado compartilhado entre threads
    fn send<T: Send>() {}
    send::<Session>();
    send::<Monitor>();
}
