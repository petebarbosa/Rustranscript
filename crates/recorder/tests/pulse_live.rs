//! Verificação **manual** do `PulseBackend` com um servidor PulseAudio/PipeWire real:
//! `cargo test -p rstt-recorder --test pulse_live -- --ignored --nocapture`
//!
//! Privacidade: o monitor do sink padrão captura tudo que está tocando no sistema, por isso o teste
//! só calcula números (RMS e potência em 1 kHz) em memória — **nada é gravado em disco**. O único
//! arquivo é um tom senoidal gerado aqui (apagado ao final) e tocado em volume baixo.
#![cfg(all(target_os = "linux", feature = "pulse"))]

use std::process::{Command, Stdio};
use std::time::Duration;

use recorder::pulse::PulseBackend;
use recorder::{CaptureBackend, CaptureStream, DeviceKind, SAMPLE_RATE};

const TONE_HZ: f64 = 1000.0;

/// Amplitude de pico (0..1) da componente de `hz` em `x` (Goertzel).
fn tone_amplitude(x: &[i16], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / f64::from(SAMPLE_RATE);
    let (c, s) = (w.cos(), w.sin());
    let (mut re, mut im) = (0.0f64, 0.0f64);
    // DFT direta em um único bin (equivalente ao Goertzel; mais simples de ler)
    let (mut cr, mut ci) = (1.0f64, 0.0f64);
    for &v in x {
        let v = f64::from(v) / 32768.0;
        re += v * cr;
        im -= v * ci;
        (cr, ci) = (cr * c - ci * s, cr * s + ci * c);
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn rms(x: &[i16]) -> f64 {
    (x.iter().map(|&v| (f64::from(v) / 32768.0).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn db(v: f64) -> f64 {
    20.0 * v.max(1e-10).log10()
}

fn read_secs(stream: &mut dyn CaptureStream, secs: f64) -> Vec<i16> {
    let mut out = vec![0i16; (secs * f64::from(SAMPLE_RATE)) as usize];
    for chunk in out.chunks_mut(1600) {
        stream.read(chunk).unwrap();
    }
    out
}

fn report(label: &str, x: &[i16]) -> (f64, f64) {
    let (r, t) = (db(rms(x)), db(tone_amplitude(x, TONE_HZ)));
    println!("{label:<22} rms {r:7.1} dBFS | 1 kHz component {t:7.1} dBFS | peak {:.4}", x.iter().map(|v| v.unsigned_abs()).max().unwrap() as f64 / 32768.0);
    (r, t)
}

#[test]
#[ignore = "precisa de PulseAudio/PipeWire e toca um tom por alguns segundos"]
fn live_default_monitor_hears_a_generated_tone() {
    let backend = PulseBackend::new();
    let devices = backend.list_devices().unwrap();
    println!("{} fontes:", devices.len());
    for d in &devices {
        println!("  {:<70} monitor={:<5} default={:<5} {}", d.name, d.is_monitor, d.is_default, d.description);
    }
    assert!(devices.iter().any(|d| d.is_monitor), "nenhum monitor listado");
    assert_eq!(devices.iter().filter(|d| d.is_default && d.is_monitor).count(), 1);
    assert!(devices.iter().filter(|d| d.is_default && !d.is_monitor).count() <= 1);
    assert!(devices.iter().filter(|d| d.is_monitor).all(|d| d.name.ends_with(".monitor")));

    assert_eq!(backend.open(DeviceKind::Mic, Some("nao_existe"), 100).err().unwrap().code(), "device_not_found");

    // referência: o monitor sem o nosso tom (pode haver outro áudio do sistema)
    let mut base = backend.open(DeviceKind::Monitor, None, 100).unwrap();
    println!("monitor padrão aberto: {} ({})", base.device().name, base.device().description);
    assert!(base.device().is_monitor && base.device().is_default);
    println!("latência: {:?}", base.latency());
    let _ = read_secs(base.as_mut(), 0.3); // descarta o início
    let baseline = read_secs(base.as_mut(), 1.5);
    drop(base);
    let (base_rms, base_tone) = report("sem tom", &baseline);

    // tom de 1 kHz, 3 s, amplitude 0,1 no arquivo + volume do player a ~30 %
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..48_000 {
        let s = (2.0 * std::f64::consts::PI * TONE_HZ * f64::from(i) / 16_000.0).sin() * 0.1;
        w.write_sample((s * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();

    let mut stream = backend.open(DeviceKind::Monitor, None, 100).unwrap();
    let mut player = Command::new("paplay")
        .arg("--volume=20000")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("paplay não encontrado");
    std::thread::sleep(Duration::from_millis(100));
    let _ = read_secs(stream.as_mut(), 0.6); // o tom ainda está começando
    let with_tone = read_secs(stream.as_mut(), 1.5);
    let _ = player.wait();
    drop(stream);
    let (tone_rms, tone_amp) = report("com tom", &with_tone);
    drop(dir); // apaga o WAV do tom

    println!("ganho do 1 kHz sobre a referência: {:+.1} dB; RMS: {:+.1} dB", tone_amp - base_tone, tone_rms - base_rms);
    assert!(tone_amp > base_tone + 10.0, "o tom não apareceu acima do fundo");
    assert!(tone_amp > -60.0, "tom fraco demais: {tone_amp:.1} dBFS");
}

#[test]
#[ignore = "precisa de PulseAudio/PipeWire"]
fn live_default_mic_opens_with_the_requested_format() {
    // só abre e lê: o mic pode estar mudo (zeros), então só o formato/tempo é verificado, nada é guardado
    let backend = PulseBackend::new();
    let mut s = backend.open(DeviceKind::Mic, None, 50).unwrap();
    println!("mic padrão: {} ({}), latência {:?}", s.device().name, s.device().description, s.latency());
    let t = std::time::Instant::now();
    let x = read_secs(s.as_mut(), 1.0);
    let took = t.elapsed().as_secs_f64();
    println!("1,0 s de áudio lidos em {took:.2} s; pico {:.4}, rms {:.1} dBFS", x.iter().map(|v| v.unsigned_abs()).max().unwrap() as f64 / 32768.0, db(rms(&x)));
    assert!((0.7..1.6).contains(&took), "relógio do servidor: {took}");
}

/// Sink nulo temporário (`rstt_test`): a sessão grava o monitor DELE, nunca o do sistema, então
/// nenhum áudio real (reunião, música) pode ir parar no WAV. Descarregado no `Drop`, mesmo se o teste falhar.
struct NullSink {
    module: String,
}

impl NullSink {
    const NAME: &'static str = "rstt_test";

    fn load() -> NullSink {
        let out = Command::new("pactl")
            .args(["load-module", "module-null-sink", &format!("sink_name={}", Self::NAME)])
            .output()
            .expect("pactl não encontrado");
        assert!(out.status.success(), "não consegui criar o sink nulo: {}", String::from_utf8_lossy(&out.stderr));
        NullSink { module: String::from_utf8_lossy(&out.stdout).trim().to_string() }
    }

    fn monitor() -> String {
        format!("{}.monitor", Self::NAME)
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        let _ = Command::new("pactl").args(["unload-module", &self.module]).status();
    }
}

#[test]
#[ignore = "precisa de PulseAudio/PipeWire (cria um sink nulo temporário) e toca um tom"]
fn live_session_records_a_null_sink_monitor_to_a_valid_wav_and_sidecar() {
    use std::sync::Arc;

    use recorder::sidecar::{SYS_WAV, Sidecar, State};
    use recorder::{Session, StartOptions, StreamChoice};

    let sink = NullSink::load();
    let dir = tempfile::tempdir().unwrap();
    let tone = dir.path().join("tone.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&tone, spec).unwrap();
    for i in 0..48_000 {
        w.write_sample(((2.0 * std::f64::consts::PI * TONE_HZ * f64::from(i) / 16_000.0).sin() * 0.1 * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();

    // o dispositivo aparece na listagem, marcado como monitor e não como padrão
    let backend = Arc::new(PulseBackend::new());
    let listed = backend.list_devices().unwrap();
    let ours = listed.iter().find(|d| d.name == NullSink::monitor()).expect("monitor do sink nulo não listado");
    assert!(ours.is_monitor && !ours.is_default);

    let rec = dir.path().join("rec");
    let mut opts = StartOptions::new(&rec, "call_2026-10-02_11-00-00");
    opts.mic = StreamChoice::Off;
    opts.sys = StreamChoice::Named(NullSink::monitor());
    let session = Session::start(backend, opts).unwrap();
    let mut player = Command::new("paplay")
        .arg(format!("--device={}", NullSink::NAME))
        .arg(&tone)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(2200));
    let live = session.levels().sys.unwrap();
    let sc = session.stop().unwrap();
    let _ = player.wait();

    let sys = sc.sys.as_ref().unwrap();
    println!(
        "sys: {} ({}) amostras={} latência={:?} ms primeira amostra={:?} nível ao vivo peak={:.4}",
        sys.device, sys.description, sys.samples, sys.latency_ms, sys.first_sample_unix_ms, live.peak
    );
    assert_eq!(sc.state, State::Complete);
    assert_eq!(sys.device, NullSink::monitor());
    assert!(sc.mic.is_none());
    assert!((sys.samples as f64 / 16_000.0 - 2.2).abs() < 0.4, "{}", sys.samples);
    assert_eq!(Sidecar::read(&rec).unwrap().state, State::Complete);
    let samples: Vec<i16> = hound::WavReader::open(rec.join(SYS_WAV)).unwrap().samples::<i16>().map(|s| s.unwrap()).collect();
    assert_eq!(samples.len() as u64, sys.samples);
    let tail = &samples[samples.len() - 16_000..];
    let amp = db(tone_amplitude(tail, TONE_HZ));
    println!("1 kHz no WAV gravado (último 1 s): {amp:.1} dBFS (tom gerado a -20 dBFS; o paplay atenua pelo volume do stream)");
    assert!(amp > -65.0 && amp < -20.0, "o tom deveria chegar ao sink nulo: {amp:.1} dBFS");
    drop(sink);
}
