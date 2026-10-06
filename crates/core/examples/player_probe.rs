//! Medições do player (issue #22) com FLACs sintéticos, fora da UI e sem banco. Dois modos:
//!
//! - `peaks <sys.flac> [mic.flac] [mic_offset_s]`: tempo dos picos (1ª vez e do cache), tempo de pular em uma
//!   chamada longa e até o 1º som do motor. Para a prova de 2 h: `/usr/bin/time -v` no binário `--release`.
//! - `play <sys.flac> [mic.flac] [mic_offset_s]`: toca um roteiro (pular, tocar, pausar, 2×) pela saída de áudio
//!   da execução. Com `PULSE_SINK=<null sink>` só neste processo, `parec` no monitor mostra o que saiu e quando.
//!
//! `cargo build --release -p rstt-core --example player_probe`
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_lib::player::{CallAudio, EventFn, PlayState, Player, peaks};
use recorder::{FakeSink, PlaybackSink, SinkOpener, default_sink_opener};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(mode), Some(sys)) = (args.first(), args.get(1)) else {
        eprintln!("uso: player_probe peaks|play <sys.flac> [mic.flac] [mic_offset_s]");
        std::process::exit(2);
    };
    let sys = PathBuf::from(sys);
    let mic = args.get(2).map(PathBuf::from);
    let mic_offset_s = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let dir = sys.parent().unwrap().to_path_buf();
    let audio = CallAudio { sys: Some(sys), mic, mic_offset_s, dir };
    match mode.as_str() {
        "peaks" => measure(&audio),
        "play" => play(&audio),
        other => eprintln!("modo desconhecido: {other}"),
    }
}

fn measure(audio: &CallAudio) {
    let t = Instant::now();
    let p = peaks::load_or_compute(audio, |_, _| {}).expect("picos");
    println!("picos (1a vez): {:.2?}  duracao={:.1}s  pontos={}  maior={}", t.elapsed(), p.frames as f64 / f64::from(p.rate), p.data.len(), p.data.iter().max().unwrap());
    let t = Instant::now();
    let q = peaks::load_or_compute(audio, |_, _| {}).expect("picos");
    println!("picos (cache):  {:.2?}  iguais={}", t.elapsed(), p == q);

    let mut m = audio.open_mixer().expect("mixer");
    let mut buf = vec![0i16; m.rate() as usize];
    for frac in [0.5, 0.99, 0.0, 0.75] {
        let at = (m.len() as f64 * frac) as u64;
        let t = Instant::now();
        m.seek(at);
        let n = m.read(&mut buf);
        println!("pular para {:>5.1}% ({:>7.0}s) + ler 1 s: {:.2?}  ({n} amostras)", frac * 100.0, at as f64 / f64::from(m.rate()), t.elapsed());
    }

    // do comando "pular e tocar" ao 1o som entregue à saída
    let captured = Arc::new(Mutex::new(Vec::new()));
    let opener: SinkOpener = {
        let captured = captured.clone();
        Arc::new(move |rate| {
            let mut s = FakeSink::new(rate);
            s.captured = captured.clone();
            Ok(Box::new(s) as Box<dyn PlaybackSink>)
        })
    };
    let player = Player::open(audio, opener, Arc::new(|_| {})).expect("player");
    let at = player.duration_s * 0.97;
    let t = Instant::now();
    player.seek(at);
    player.play();
    while captured.lock().unwrap().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(30), "sem som");
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("pular para {at:.0}s e tocar -> 1o som: {:.2?}", t.elapsed());
    player.close();
}

fn play(audio: &CallAudio) {
    let t0 = Instant::now();
    let log = move |what: &str| eprintln!("[{:7.3}s] {what}", t0.elapsed().as_secs_f64());
    let last = Mutex::new((Instant::now() - Duration::from_secs(10), PlayState::Paused));
    let on_event: EventFn = Arc::new(move |e| {
        let mut l = last.lock().unwrap();
        if l.0.elapsed() >= Duration::from_millis(500) || l.1 != e.state {
            *l = (Instant::now(), e.state);
            eprintln!("[{:7.3}s]   evento: {:?} pos={:.3}s vel={}", t0.elapsed().as_secs_f64(), e.state, e.position_s, e.speed);
        }
    });
    let p = Player::open(audio, default_sink_opener(), on_event).expect("player");
    let wait = |s: f64| std::thread::sleep(Duration::from_secs_f64(s));
    wait(0.5);
    log("pular para 32 s e tocar");
    p.seek(32.0);
    p.play();
    wait(2.5);
    log("pausar");
    p.pause();
    wait(1.0);
    log("pular para 51 s e tocar");
    p.seek(51.0);
    p.play();
    wait(2.0);
    log("velocidade 2x");
    p.set_speed(2.0);
    wait(2.0);
    log("pausar");
    p.pause();
    wait(0.5);
    p.close();
    log("fim");
}
