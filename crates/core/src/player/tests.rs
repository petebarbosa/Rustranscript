//! Testes do player com áudio sintético (nada de gravações reais): FLACs gerados pelo mesmo codificador da app
//! (flacenc, sem tabela de busca), saída falsa e relógio nenhum, exceto onde o teste diz que é tempo real.
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use recorder::{FakeSink, PlaybackSink, SinkOpener};

use super::engine::{PlayState, Player, PlayerEvent};
use super::mixer::Mixer;
use super::peaks::{self, PEAKS_PER_S};
use super::session::{PositionTracker, Session};
use super::source::CallAudio;
use crate::audio::wav_to_flac;

const RATE: u32 = 16_000;

fn flac(dir: &Path, name: &str, samples: &[i16]) -> PathBuf {
    let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let wav = dir.join(format!("{name}.wav"));
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    let out = dir.join(format!("{name}.flac"));
    wav_to_flac(&wav, &out, &mut |_, _| {}).unwrap();
    std::fs::remove_file(wav).unwrap();
    out
}

/// Cada posição tem um valor próprio (a cada 65536 amostras repete): ler "o que está no índice i" prova a posição.
fn ramp(n: usize) -> Vec<i16> {
    (0..n).map(|i| (i as u16) as i16 / 2).collect()
}

fn tone(hz: f64, amp: f64, n: usize) -> Vec<i16> {
    (0..n).map(|i| ((i as f64 * hz * std::f64::consts::TAU / f64::from(RATE)).sin() * amp) as i16).collect()
}

/// Amplitude da componente de `hz` (Goertzel, como o `pulse_live`).
fn tone_amp(x: &[i16], hz: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / f64::from(RATE);
    let (c, s) = (w.cos(), w.sin());
    let (mut re, mut im, mut cr, mut ci) = (0.0f64, 0.0f64, 1.0f64, 0.0f64);
    for &v in x {
        let v = f64::from(v) / 32768.0;
        re += v * cr;
        im -= v * ci;
        (cr, ci) = (cr * c - ci * s, cr * s + ci * c);
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn read_all(m: &mut Mixer) -> Vec<i16> {
    let mut out = vec![0i16; m.len() as usize];
    let mut at = 0;
    while at < out.len() {
        // pedaços de tamanho torto, de propósito
        let end = (at + 3001).min(out.len());
        let n = m.read(&mut out[at..end]);
        assert!(n > 0);
        at += n;
    }
    out
}

// ---------------------------------------------------------------- decodificação e busca

#[test]
fn seek_is_sample_exact_on_flacenc_files_without_seektable() {
    let dir = tempfile::tempdir().unwrap();
    let data = ramp(100_000);
    let path = flac(dir.path(), "a", &data);
    let mut r = super::decode::TrackReader::open(&path).unwrap();
    assert_eq!((r.rate, r.frames), (RATE, 100_000));
    let mut all = vec![0i16; 100_000];
    assert_eq!(r.read(&mut all).unwrap(), 100_000);
    assert_eq!(all, data);
    assert_eq!(r.read(&mut all).unwrap(), 0, "fim da trilha");
    // para a frente, para trás, no limite de quadros (4096) e perto do fim
    for &at in &[50_001u64, 0, 1, 4095, 4096, 4097, 99_000, 12_345, 99_999] {
        r.seek(at).unwrap();
        assert_eq!(r.pos(), at);
        let mut buf = [0i16; 700];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(n, (100_000 - at as usize).min(700));
        assert_eq!(&buf[..n], &data[at as usize..at as usize + n], "seek {at}");
    }
    // do começo, atravessando quadros, depois de já ter lido tudo
    for &at in &[0u64, 3, 4096, 8000] {
        r.seek(at).unwrap();
        let mut buf = vec![0i16; 30_000];
        assert_eq!(r.read(&mut buf).unwrap(), 30_000);
        assert_eq!(buf.iter().zip(&data[at as usize..]).position(|(a, b)| a != b), None, "seek {at} + 30 000 amostras");
    }
    r.seek(100_000).unwrap();
    assert_eq!(r.read(&mut [0i16; 10]).unwrap(), 0);
    r.seek(5_000_000).unwrap();
    assert_eq!(r.pos(), 100_000, "além do fim = fim");
}

#[test]
fn many_seeks_in_any_order_stay_exact_even_around_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let n = RATE as usize * 40;
    let data = ramp(n);
    let path = flac(dir.path(), "a", &data);
    let mut r = super::decode::TrackReader::open(&path).unwrap();
    let mut x: u64 = 12345;
    let mut buf = vec![0i16; 9000];
    for i in 0..400 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // metade dos pulos vai para as pontas (início, fim, fronteiras de quadro), onde o parser costuma tropeçar
        let at = match i % 4 {
            0 => (x >> 33) % 20_000,
            1 => n as u64 - 1 - (x >> 33) % 20_000,
            2 => ((x >> 33) % (n as u64 / 4096)) * 4096,
            _ => (x >> 33) % n as u64,
        };
        r.seek(at).unwrap();
        let got = r.read(&mut buf).unwrap();
        assert_eq!(got, buf.len().min(n - at as usize), "seek {at}");
        assert_eq!(&buf[..got], &data[at as usize..at as usize + got], "seek {at} (volta {i})");
        if i % 50 == 0 {
            // ler até o fim e voltar
            let mut sink = vec![0i16; 70_000];
            while r.read(&mut sink).unwrap() > 0 {}
        }
    }
}

// ---------------------------------------------------------------- mistura

#[test]
fn mixer_replays_identically_after_reaching_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let data = ramp(48_000);
    let path = flac(dir.path(), "a", &data);
    let mut m = Mixer::open(Some(&path), None, 0.0).unwrap();
    for round in 0..3 {
        m.seek(0);
        let mut got = Vec::new();
        let mut buf = vec![0i16; 1600];
        loop {
            let n = m.read(&mut buf);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        let bad = got.iter().zip(&data).position(|(a, b)| a != b);
        if let Some(b) = bad {
            eprintln!("volta {round}: 1º erro em {b}: got {:?} want {:?}; got[b..] acha em {:?}", &got[b..b + 4], &data[b..b + 4], data.iter().position(|&v| v == got[b]));
        }
        assert_eq!((got.len(), bad), (data.len(), None), "volta {round}");
    }
}

#[test]
fn mixer_sums_tracks_with_the_mic_offset_of_either_sign() {
    let dir = tempfile::tempdir().unwrap();
    let sys: Vec<i16> = tone(300.0, 8000.0, 20_000);
    let mic: Vec<i16> = ramp(12_000);
    let (sys_f, mic_f) = (flac(dir.path(), "sys", &sys), flac(dir.path(), "mic", &mic));
    for off in [0i64, 700, -500, 15_000] {
        let mut m = Mixer::open_at(&[(&sys_f, 0), (&mic_f, off)]).unwrap();
        let len = 20_000.max(12_000 + off) as usize;
        assert_eq!(m.len(), len as u64, "duração = o que acaba por último (offset {off})");
        let at = |v: &[i16], i: i64| if i >= 0 && (i as usize) < v.len() { i32::from(v[i as usize]) } else { 0 };
        let want: Vec<i16> = (0..len as i64).map(|n| (at(&sys, n) + at(&mic, n - off)).clamp(-32768, 32767) as i16).collect();
        assert_eq!(read_all(&mut m), want, "offset {off}");
        // pular para o meio (uma trilha ainda não começou ou já acabou) dá o mesmo som
        for &p in &[0usize, 699, 700, 11_999, 12_000, 12_700, 19_999] {
            if p >= len {
                continue;
            }
            m.seek(p as u64);
            let mut buf = vec![0i16; 500.min(len - p)];
            let n = m.read(&mut buf);
            assert_eq!(&buf[..n], &want[p..p + n], "seek {p} (offset {off})");
        }
    }
}

#[test]
fn mixer_saturates_instead_of_wrapping() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (flac(dir.path(), "a", &vec![30_000; 9000]), flac(dir.path(), "b", &vec![-30_000; 6000]));
    let c = flac(dir.path(), "c", &vec![30_000; 9000]);
    let mut m = Mixer::open_at(&[(&a, 0), (&c, 0)]).unwrap();
    assert!(read_all(&mut m).iter().all(|&s| s == 32767));
    let mut m = Mixer::open_at(&[(&a, 0), (&b, 0)]).unwrap();
    let out = read_all(&mut m);
    assert!(out[..6000].iter().all(|&s| s == 0) && out[6000..].iter().all(|&s| s == 30_000));
}

#[test]
fn mixer_open_applies_the_offset_only_when_both_tracks_exist() {
    let dir = tempfile::tempdir().unwrap();
    let (sys, mic) = (flac(dir.path(), "sys", &vec![100; 16_000]), flac(dir.path(), "mic", &vec![7; 16_000]));
    let both = Mixer::open(Some(&sys), Some(&mic), 0.5).unwrap();
    assert_eq!(both.len(), 24_000, "o mic entra 0,5 s depois");
    let mic_only = Mixer::open(None, Some(&mic), 0.5).unwrap();
    assert_eq!(mic_only.len(), 16_000);
    assert!(Mixer::open(None, None, 0.0).is_err());
}

// ---------------------------------------------------------------- picos

fn call_audio(dir: &Path, sys: Option<PathBuf>, mic: Option<PathBuf>) -> CallAudio {
    CallAudio { sys, mic, mic_offset_s: 0.0, dir: dir.to_path_buf() }
}

#[test]
fn peaks_are_deterministic_for_a_synthetic_signal() {
    let dir = tempfile::tempdir().unwrap();
    // 1 s a 1/2 da escala, 1 s a 1/4, 0,5 s de silêncio, e um estalo baixo no último pico
    let mut s = vec![0i16; 0];
    s.extend((0..16_000).map(|i| if i % 2 == 0 { 16_384 } else { -16_384 }));
    s.extend((0..16_000).map(|i| if i % 2 == 0 { 8_192 } else { -8_192 }));
    s.extend(vec![0; 8_000 - 1]);
    s.push(40);
    let path = flac(dir.path(), "sys", &s);
    let audio = call_audio(dir.path(), Some(path), None);
    let p = peaks::load_or_compute(&audio, |_, _| {}).unwrap();
    assert_eq!((p.per_s, p.rate, p.frames), (PEAKS_PER_S, RATE, 40_000));
    assert_eq!(p.data.len(), 125, "40 000 amostras / 320 por pico");
    assert!(p.data[..50].iter().all(|&v| v == 128), "16384 -> ceil(255/2)");
    assert!(p.data[50..100].iter().all(|&v| v == 64));
    assert!(p.data[100..124].iter().all(|&v| v == 0));
    assert_eq!(p.data[124], 1, "som baixo mas não nulo nunca vira zero");
    // redução: o maior de cada faixa
    assert_eq!(peaks::downsample(&p.data, 5), vec![128, 128, 64, 64, 1]);
    assert_eq!(peaks::downsample(&p.data, 1000), p.data, "nunca inventa picos");
    assert_eq!(peaks::downsample(&p.data, 0), Vec::<u8>::new());
}

#[test]
fn peaks_mix_both_tracks_and_use_the_cache_until_the_audio_changes() {
    let dir = tempfile::tempdir().unwrap();
    let sys = flac(dir.path(), "sys", &vec![8_192; 16_000]);
    let mic = flac(dir.path(), "mic", &vec![8_192; 16_000]);
    let audio = call_audio(dir.path(), Some(sys.clone()), Some(mic));
    let mut calls = 0;
    let p = peaks::load_or_compute(&audio, |_, _| calls += 1).unwrap();
    assert!(calls > 0 && p.data.iter().all(|&v| v == 128), "8192 + 8192 = 16384 -> 128");
    assert!(peaks::cache_path(&audio).is_file());
    let mut again = 0;
    assert_eq!(peaks::load_or_compute(&audio, |_, _| again += 1).unwrap(), p);
    assert_eq!(again, 0, "veio do cache, sem decodificar");
    // outro deslocamento do mic = outro eixo = recalcula
    let shifted = CallAudio { mic_offset_s: 0.25, ..audio.clone() };
    let mut redone = 0;
    let q = peaks::load_or_compute(&shifted, |_, _| redone += 1).unwrap();
    assert!(redone > 0 && q.frames == 20_000);
    // áudio trocado (mesmo nome, outro tamanho) = recalcula
    flac(dir.path(), "sys", &vec![100; 8_000]);
    let mut changed = 0;
    let r = peaks::load_or_compute(&shifted, |_, _| changed += 1).unwrap();
    assert!(changed > 0 && r.frames == 20_000, "o mic (16 000) + 0,25 s de deslocamento ainda manda na duração");
    // cache truncado não vale
    let path = peaks::cache_path(&shifted);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
    let mut third = 0;
    peaks::load_or_compute(&shifted, |_, _| third += 1).unwrap();
    assert!(third > 0);
}

// ---------------------------------------------------------------- posição

#[test]
fn position_tracker_interpolates_between_marks() {
    let mut t = PositionTracker::new(1000);
    assert_eq!(t.at(0), 1000);
    t.push(1600, 2600); // 1× : 1600 de saída = 1600 de entrada
    t.push(3200, 5800); // 2× : 1600 de saída = 3200 de entrada
    assert_eq!(t.at(800), 1800);
    assert_eq!(t.at(1600), 2600);
    assert_eq!(t.at(2400), 4200);
    assert_eq!(t.at(3200), 5800);
    assert_eq!(t.at(99_999), 5800, "depois da última marca fica nela");
    t.push(3200, 9999); // marca repetida é ignorada
    assert_eq!(t.at(3200), 5800);
}

#[test]
fn session_position_subtracts_what_the_server_still_holds() {
    let dir = tempfile::tempdir().unwrap();
    let path = flac(dir.path(), "a", &ramp(RATE as usize * 4));
    let mut s = Session::new(Mixer::open(Some(&path), None, 0.0).unwrap());
    s.seek_s(1.0);
    let mut out = Vec::new();
    assert!(!s.next_chunk(&mut out, 1600), "100 ms: ainda não é o fim");
    assert_eq!(out, ramp(RATE as usize * 4)[16_000..17_600]);
    // entregues 100 ms; com 40 ms ainda no buffer do servidor, está tocando o 60 ms
    let pos = s.position_s(Some(Duration::from_millis(40)));
    assert!((pos - 1.06).abs() < 1e-4, "{pos}");
    assert!((s.position_s(None) - 1.1).abs() < 1e-4);
    assert!((s.position_s(Some(Duration::from_secs(5))) - 1.0).abs() < 1e-4, "latência maior que o produzido não volta antes do ponto de partida");
    // pular zera a conta
    s.seek_s(3.5);
    assert!((s.position_s(Some(Duration::from_millis(40))) - 3.5).abs() < 1e-4);
    out.clear();
    assert!(s.next_chunk(&mut out, 100_000), "passou do fim");
    assert_eq!(out.len(), 8_000);
    assert!((s.position_s(None) - 4.0).abs() < 1e-4);
}

// ---------------------------------------------------------------- velocidade

fn stretched(samples: &[i16], speed: f64) -> (Vec<i16>, Session) {
    let dir = tempfile::tempdir().unwrap();
    let path = flac(dir.path(), "a", samples);
    let mut s = Session::new(Mixer::open(Some(&path), None, 0.0).unwrap());
    s.set_speed(speed, 0);
    let mut out = Vec::new();
    assert!(s.next_chunk(&mut out, usize::MAX));
    (out, s)
}

#[test]
fn speed_changes_duration_but_not_pitch() {
    let src = tone(440.0, 12_000.0, RATE as usize * 6);
    for speed in [1.5, 2.0, 0.75] {
        let (out, s) = stretched(&src, speed);
        let want = src.len() as f64 / speed;
        assert!((out.len() as f64 - want).abs() < 1600.0, "{speed}×: {} amostras, esperava ~{want}", out.len());
        // o tom continua em 440 Hz (um tom reamostrado a 2× estaria em 880 Hz)
        let mid = &out[8000..out.len() - 8000];
        let a440 = tone_amp(mid, 440.0);
        let other = [880.0, 220.0, 440.0 * speed].into_iter().map(|hz| tone_amp(mid, hz)).fold(0.0, f64::max);
        assert!(a440 > 0.25, "{speed}×: amplitude em 440 Hz {a440}");
        assert!(a440 > 6.0 * other, "{speed}×: 440 Hz = {a440}, tom deslocado = {other}");
        // a posição acompanha a entrada: acabou a saída = acabou a chamada
        assert!((s.position_s(None) - 6.0).abs() < 0.05, "{speed}×: {}", s.position_s(None));
    }
}

#[test]
fn speech_like_signal_keeps_its_fundamental_when_sped_up() {
    // 120 Hz com harmônicos (uma voz grave) + um pouco de ruído
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let src: Vec<i16> = (0..RATE as usize * 8)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let t = i as f64 / f64::from(RATE);
            let v: f64 = (1..=6).map(|h| (std::f64::consts::TAU * 120.0 * h as f64 * t).sin() / h as f64).sum();
            (v * 6000.0 + ((x >> 40) % 400) as f64 - 200.0) as i16
        })
        .collect();
    let (out, _) = stretched(&src, 2.0);
    let ratio = out.len() as f64 / src.len() as f64;
    assert!((ratio - 0.5).abs() < 0.02, "{ratio}");
    let mid = &out[8000..out.len() - 8000];
    let (f0, double) = (tone_amp(mid, 120.0), tone_amp(mid, 240.0));
    assert!(f0 > 2.0 * double, "o fundamental (120 Hz) deve continuar acima do dobro dele: {f0} vs {double}");
}

#[test]
fn speed_midway_keeps_position_continuous() {
    let dir = tempfile::tempdir().unwrap();
    let path = flac(dir.path(), "a", &tone(300.0, 9000.0, RATE as usize * 10));
    let mut s = Session::new(Mixer::open(Some(&path), None, 0.0).unwrap());
    let mut out = Vec::new();
    s.next_chunk(&mut out, 16_000 * 2); // 2 s a 1×
    assert!((s.position_s(None) - 2.0).abs() < 1e-3);
    let at = (s.position_s(None) * f64::from(RATE)) as u64;
    s.set_speed(2.0, at);
    out.clear();
    s.next_chunk(&mut out, 16_000 * 2); // 2 s de saída a 2× = 4 s de entrada
    assert!((s.position_s(None) - 6.0).abs() < 0.1, "{}", s.position_s(None));
}

// ---------------------------------------------------------------- motor

fn opener(captured: Arc<Mutex<Vec<i16>>>, realtime: bool) -> SinkOpener {
    Arc::new(move |rate| {
        let mut s = if realtime { FakeSink::realtime(rate) } else { FakeSink::new(rate) };
        s.captured = captured.clone();
        Ok(Box::new(s) as Box<dyn PlaybackSink>)
    })
}

fn events() -> (Arc<dyn Fn(PlayerEvent) + Send + Sync>, Receiver<PlayerEvent>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    (Arc::new(move |e| drop(tx.lock().unwrap().send(e))), rx)
}

fn wait_for(rx: &Receiver<PlayerEvent>, pred: impl Fn(&PlayerEvent) -> bool) -> Vec<PlayerEvent> {
    let mut seen = Vec::new();
    loop {
        let e = rx.recv_timeout(Duration::from_secs(20)).expect("evento esperado não veio");
        let done = pred(&e);
        seen.push(e);
        if done {
            return seen;
        }
    }
}

#[test]
fn player_plays_to_the_end_and_replays_from_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let data = ramp(RATE as usize * 3);
    let path = flac(dir.path(), "a", &data);
    let audio = call_audio(dir.path(), Some(path), None);
    let (cb, rx) = events();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let p = Player::open(&audio, opener(captured.clone(), false), cb).unwrap();
    assert!((p.duration_s - 3.0).abs() < 1e-9);
    p.play();
    let seen = wait_for(&rx, |e| e.state == PlayState::Ended);
    assert_eq!(seen[0].state, PlayState::Playing);
    assert!(seen.windows(2).all(|w| w[0].position_s <= w[1].position_s), "a posição nunca anda para trás tocando");
    assert_eq!(seen.last().unwrap().position_s, 3.0);
    let got = captured.lock().unwrap().clone();
    assert_eq!(got.len(), data.len());
    assert_eq!(got.iter().zip(&data).position(|(a, b)| a != b), None, "o que saiu é a chamada inteira, sem emenda");
    // tocar de novo no fim recomeça
    captured.lock().unwrap().clear();
    p.play();
    wait_for(&rx, |e| e.state == PlayState::Ended);
    let got = captured.lock().unwrap().clone();
    assert_eq!((got.len(), got.iter().zip(&data).position(|(a, b)| a != b)), (data.len(), None));
    p.close();
}

#[test]
fn player_seek_while_paused_then_play_starts_there() {
    let dir = tempfile::tempdir().unwrap();
    let (sys, mic) = (tone(440.0, 6000.0, RATE as usize * 4), ramp(RATE as usize * 4));
    let audio = call_audio(dir.path(), Some(flac(dir.path(), "sys", &sys)), Some(flac(dir.path(), "mic", &mic)));
    let (cb, rx) = events();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let p = Player::open(&audio, opener(captured.clone(), false), cb).unwrap();
    p.seek(2.5);
    let e = wait_for(&rx, |e| e.state == PlayState::Paused).pop().unwrap();
    assert!((e.position_s - 2.5).abs() < 1e-6);
    p.seek(99.0);
    assert_eq!(wait_for(&rx, |e| e.state == PlayState::Paused).pop().unwrap().position_s, 4.0, "além do fim = fim");
    p.seek(2.5);
    p.set_speed(1.0);
    p.play();
    wait_for(&rx, |e| e.state == PlayState::Ended);
    let got = captured.lock().unwrap().clone();
    let from = (2.5 * f64::from(RATE)) as usize;
    let want: Vec<i16> = (from..RATE as usize * 4).map(|i| (i32::from(sys[i]) + i32::from(mic[i])).clamp(-32768, 32767) as i16).collect();
    assert_eq!(got, want);
    p.close();
}

#[test]
fn player_speed_makes_the_output_shorter() {
    let dir = tempfile::tempdir().unwrap();
    let audio = call_audio(dir.path(), Some(flac(dir.path(), "a", &tone(440.0, 9000.0, RATE as usize * 8))), None);
    let (cb, rx) = events();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let p = Player::open(&audio, opener(captured.clone(), false), cb).unwrap();
    p.set_speed(2.0);
    assert_eq!(wait_for(&rx, |_| true)[0].speed, 2.0);
    p.play();
    wait_for(&rx, |e| e.state == PlayState::Ended);
    let n = captured.lock().unwrap().len() as f64;
    assert!((n - 64_000.0).abs() < 1600.0, "{n}");
    p.set_speed(99.0);
    assert_eq!(wait_for(&rx, |_| true)[0].speed, 2.0, "fora da faixa vale o limite");
    p.close();
}

#[test]
fn player_pause_in_real_time_resumes_exactly_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let data = ramp(RATE as usize * 6);
    let audio = call_audio(dir.path(), Some(flac(dir.path(), "a", &data)), None);
    let (cb, rx) = events();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let p = Player::open(&audio, opener(captured.clone(), true), cb).unwrap();
    p.play();
    wait_for(&rx, |e| e.state == PlayState::Playing && e.position_s > 0.6);
    p.pause();
    let paused = wait_for(&rx, |e| e.state == PlayState::Paused).pop().unwrap();
    // o que o "servidor" ainda guardava não conta como tocado; e não passou de 1 s em tempo real
    assert!(paused.position_s > 0.5 && paused.position_s < 1.5, "{}", paused.position_s);
    let written = captured.lock().unwrap().len();
    assert!(written as f64 / f64::from(RATE) > paused.position_s, "foi entregue mais do que tocou");
    captured.lock().unwrap().clear();
    p.play();
    wait_for(&rx, |e| e.state == PlayState::Playing && e.position_s > paused.position_s + 0.3);
    p.pause();
    wait_for(&rx, |e| e.state == PlayState::Paused);
    let after = captured.lock().unwrap().clone();
    let from = (paused.position_s * f64::from(RATE)).round() as usize;
    assert_eq!(&after[..2000], &data[from..from + 2000], "retomou na amostra em que parou");
    p.close();
}

#[test]
fn player_reports_a_failing_output_and_stays_usable() {
    let dir = tempfile::tempdir().unwrap();
    let audio = call_audio(dir.path(), Some(flac(dir.path(), "a", &ramp(16_000))), None);
    let (cb, rx) = events();
    let failing: SinkOpener = Arc::new(|_| Err(recorder::Error::BackendUnavailable("no server".into())));
    let p = Player::open(&audio, failing, cb).unwrap();
    p.play();
    let e = wait_for(&rx, |e| e.state == PlayState::Error).pop().unwrap();
    assert_eq!(e.error.unwrap().0, "backend_unavailable");
    p.seek(0.5);
    assert_eq!(wait_for(&rx, |_| true)[0].state, PlayState::Paused, "depois do erro volta a parado");
    p.close();
}

// ---------------------------------------------------------------- chamada longa

#[test]
fn long_call_peaks_and_seek_stay_cheap() {
    // 12 min a 16 kHz = 11,5 M de amostras por trilha; a prova de memória de 2 h está no exemplo `player_probe`
    let dir = tempfile::tempdir().unwrap();
    let n = RATE as usize * 60 * 12;
    let sys: Vec<i16> = (0..n).map(|i| if i / RATE as usize % 60 < 30 { ((i % 200) as i16 - 100) * 60 } else { 0 }).collect();
    let path = flac(dir.path(), "sys", &sys);
    let audio = call_audio(dir.path(), Some(path.clone()), None);
    let started = std::time::Instant::now();
    let p = peaks::load_or_compute(&audio, |_, _| {}).unwrap();
    assert_eq!(p.data.len(), 12 * 60 * PEAKS_PER_S as usize);
    // meio minuto de fala e meio de silêncio, 12 vezes
    assert!(p.data[..30 * 50].iter().all(|&v| v > 0) && p.data[30 * 50..60 * 50].iter().all(|&v| v == 0));
    // pular para o fim não lê o começo
    let mut m = audio.open_mixer().unwrap();
    m.seek(n as u64 - 10_000);
    let mut buf = vec![0i16; 20_000];
    assert_eq!(m.read(&mut buf), 10_000);
    assert_eq!(&buf[..10_000], &sys[n - 10_000..]);
    let t = started.elapsed();
    assert!(t < Duration::from_secs(60), "{t:?}");
}
