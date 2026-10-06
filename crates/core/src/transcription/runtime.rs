//! Runtime Python fixado, instalado dentro de `<dados>/runtime` (nada fora de `<dados>`): uv verificado por
//! sha256 → Python 3.12.15 gerenciado pelo uv → venv com `uv pip sync --require-hashes` (lock embutido no
//! binário). Linux x86_64 apenas. O `worker.py` e o lock vêm do repositório (`worker/`) embutidos aqui.
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::engine::{ProcessEngine, WorkerLaunch};
use super::{FAKE_WORKER_ENV, models};
use crate::{Error, Result, db, fsx};

pub const UV_VERSION: &str = "0.12.22";
pub const UV_URL: &str = "https://github.com/astral-sh/uv/releases/download/0.12.22/uv-x86_64-unknown-linux-gnu.tar.gz";
pub const UV_SHA256: &str = "b9980552309f09c15172b8be828555e375097f16deb459795ce7bfd200380f0b";
pub const UV_BYTES: u64 = 19_916_278;
/// Membro do tar com o binário (`uv-x86_64-unknown-linux-gnu/uv`).
pub const UV_TAR_MEMBER: &str = "uv-x86_64-unknown-linux-gnu/uv";
pub const PYTHON_VERSION: &str = "3.12.15";
/// Sobe quando mudar o lock ou o protocolo: `manifest.json` diferente = reinstalar/atualizar. Só o `worker.py` mudar
/// não precisa: `status_with` atualiza o script e o `worker_sha256` sozinho (sem refazer o venv).
pub const RUNTIME_VERSION: u32 = 1;

pub const WORKER_PY: &str = include_str!("../../../../worker/worker.py");
pub const LOCK: &str = include_str!("../../../../worker/requirements-linux.lock");

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub root: PathBuf,
    pub uv: PathBuf,
    pub python_dir: PathBuf,
    pub venv: PathBuf,
    /// `<root>/venv/bin/python`
    pub python: PathBuf,
    /// `<root>/worker.py` (reescrito a cada start se o conteúdo diferir do embutido)
    pub worker: PathBuf,
    pub manifest: PathBuf,
}

pub fn paths(data_dir: &Path) -> RuntimePaths {
    let root = data_dir.join("runtime");
    RuntimePaths {
        uv: root.join("uv").join("uv"),
        python_dir: root.join("python"),
        venv: root.join("venv"),
        python: root.join("venv").join("bin").join("python"),
        worker: root.join("worker.py"),
        manifest: root.join("manifest.json"),
        root,
    }
}

/// `missing` (nada instalado) | `outdated` (manifest com versão/lock/sha diferente) | `ready` | `fake`
/// (`FAKE_WORKER_ENV` ativo: não precisa de runtime).
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub state: String,
    pub runtime_version: u32,
    pub python: String,
    pub uv: String,
    pub installed_at: Option<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Marca de runtime instalado (`<runtime>/manifest.json`); é o que decide `ready` × `outdated`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Manifest {
    runtime_version: u32,
    uv: String,
    python: String,
    lock_sha256: String,
    worker_sha256: String,
    installed_at: String,
}

fn expected_manifest(installed_at: String) -> Manifest {
    Manifest {
        runtime_version: RUNTIME_VERSION,
        uv: UV_VERSION.into(),
        python: PYTHON_VERSION.into(),
        lock_sha256: sha256_hex(LOCK.as_bytes()),
        worker_sha256: sha256_hex(WORKER_PY.as_bytes()),
        installed_at,
    }
}

fn read_manifest(p: &RuntimePaths) -> Option<Manifest> {
    serde_json::from_slice(&std::fs::read(&p.manifest).ok()?).ok()
}

pub fn status(data_dir: &Path) -> Result<RuntimeStatus> {
    status_with(data_dir, std::env::var(FAKE_WORKER_ENV).is_ok_and(|v| !v.trim().is_empty()))
}

/// Se o manifest instalado só difere do esperado no `worker_sha256` (lock, uv, Python e versão iguais), o venv
/// continua valendo: grava o `worker.py` embutido e depois o manifest (ambos atômicos), sem rede nem botão. Dois
/// processos fazendo isso juntos gravam os mesmos bytes; um worker em execução já carregou o script na memória.
/// `false` se há outra diferença ou se a gravação falhar (o status cai para `outdated`).
fn refresh_worker_if_only_change(p: &RuntimePaths, installed: &Manifest, want: &Manifest) -> bool {
    let only_worker = Manifest { worker_sha256: want.worker_sha256.clone(), ..installed.clone() } == *want;
    if !only_worker {
        return false;
    }
    fsx::write_atomic(&p.worker, WORKER_PY.as_bytes()).and_then(|()| fsx::write_atomic(&p.manifest, &serde_json::to_vec_pretty(want)?)).is_ok()
}

fn status_with(data_dir: &Path, fake: bool) -> Result<RuntimeStatus> {
    let p = paths(data_dir);
    let manifest = read_manifest(&p);
    let state = if fake {
        "fake"
    } else {
        match &manifest {
            None => "missing",
            Some(_) if !p.python.exists() => "missing",
            Some(m) => {
                let want = expected_manifest(m.installed_at.clone());
                if *m == want || refresh_worker_if_only_change(&p, m, &want) { "ready" } else { "outdated" }
            }
        }
    };
    Ok(RuntimeStatus {
        state: state.into(),
        runtime_version: RUNTIME_VERSION,
        python: PYTHON_VERSION.into(),
        uv: UV_VERSION.into(),
        installed_at: manifest.map(|m| m.installed_at),
    })
}

/// Etapa do bootstrap (evento `transcription-setup`, `phase = "runtime"`): `download_uv` | `install_python` |
/// `create_venv` | `sync_packages` | `verify`.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeProgress {
    pub step: String,
    pub index: u32,
    pub of: u32,
}

const STEPS: [&str; 5] = ["download_uv", "install_python", "create_venv", "sync_packages", "verify"];

fn setup_failed(detail: impl std::fmt::Display) -> Error {
    Error::transcription("setup_failed", detail)
}

fn cancelled() -> Error {
    Error::transcription("setup_cancelled", "cancelled by user")
}

/// Fim do log do comando (as últimas linhas dizem o porquê da falha).
fn log_tail(log: &Path) -> String {
    let Ok(mut f) = std::fs::File::open(log) else { return String::new() };
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(1500)));
    let mut text = String::new();
    let _ = f.read_to_string(&mut text);
    text.trim().to_string()
}

/// Roda `program args` num grupo próprio, com a saída num arquivo de log; `cancel` mata o grupo todo.
fn run(program: &Path, args: &[&std::ffi::OsStr], env: &[(&str, &Path)], log: &Path, cancel: &AtomicBool) -> Result<()> {
    let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env("UV_LINK_MODE", "copy")
        .env("UV_NO_CONFIG", "1")
        .env("UV_MANAGED_PYTHON", "1")
        .env("UV_NO_PROGRESS", "1")
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| setup_failed(format!("{}: {e}", program.display())))?;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() { Ok(()) } else { Err(setup_failed(format!("{} {status}: {}", program.display(), log_tail(log)))) };
        }
        if cancel.load(Ordering::Relaxed) {
            // SAFETY: mata o grupo que criamos acima (pgid = pid do filho).
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            return Err(cancelled());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Instala/atualiza (venv novo ao lado + `hello` OK + rename atômico). `UV_CACHE_DIR` temporário apagado no
/// fim, `UV_LINK_MODE=copy`, `UV_NO_CONFIG=1`, `UV_PYTHON_INSTALL_DIR`, `UV_MANAGED_PYTHON=1`. Erros:
/// `setup_failed`, `download_failed`, `checksum_mismatch`, `setup_cancelled`.
pub fn ensure(data_dir: &Path, on_progress: &mut dyn FnMut(&RuntimeProgress), cancel: &AtomicBool) -> Result<()> {
    let p = paths(data_dir);
    if status_with(data_dir, false)?.state == "ready" {
        return Ok(());
    }
    std::fs::create_dir_all(&p.root)?;
    let cache = p.root.join(".uv-cache");
    let log = p.root.join("setup.log");
    let _ = std::fs::remove_file(&log);
    let venv_new = p.root.join("venv.new");
    let lock_path = p.root.join("requirements.lock");
    let env: [(&str, &Path); 2] = [("UV_CACHE_DIR", &cache), ("UV_PYTHON_INSTALL_DIR", &p.python_dir)];
    let mut step = 0usize;
    let mut begin = |on_progress: &mut dyn FnMut(&RuntimeProgress)| -> Result<()> {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        on_progress(&RuntimeProgress { step: STEPS[step].into(), index: step as u32 + 1, of: STEPS.len() as u32 });
        step += 1;
        Ok(())
    };
    let result = (|| -> Result<()> {
        // 1. uv: reaproveita o que já roda na versão certa; senão baixa e confere o sha256 do pacote
        begin(on_progress)?;
        let uv_ok = p.uv.is_file()
            && Command::new(&p.uv).arg("--version").output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(UV_VERSION));
        if !uv_ok {
            let _ = std::fs::remove_file(&p.uv);
            let item = models::Item {
                model: "uv".into(),
                dest: p.uv.clone(),
                url: UV_URL.into(),
                sha256: UV_SHA256.into(),
                bytes: UV_BYTES,
                extract: Some((UV_TAR_MEMBER.into(), None)),
            };
            models::install_item(&item, &mut |_| {}, cancel)?;
            std::fs::set_permissions(&p.uv, std::fs::Permissions::from_mode(0o755))?;
        }
        // 2. Python gerenciado pelo uv
        begin(on_progress)?;
        run(&p.uv, &["python".as_ref(), "install".as_ref(), PYTHON_VERSION.as_ref(), "--no-bin".as_ref()], &env, &log, cancel)?;
        // 3. venv novo ao lado do atual
        begin(on_progress)?;
        let _ = std::fs::remove_dir_all(&venv_new);
        run(&p.uv, &["venv".as_ref(), "--python".as_ref(), PYTHON_VERSION.as_ref(), venv_new.as_os_str()], &env, &log, cancel)?;
        // 4. pacotes do lock embutido, só com hashes
        begin(on_progress)?;
        fsx::write_atomic(&lock_path, LOCK.as_bytes())?;
        let venv_python = venv_new.join("bin").join("python");
        run(
            &p.uv,
            &["pip".as_ref(), "sync".as_ref(), "--require-hashes".as_ref(), "--python".as_ref(), venv_python.as_os_str(), lock_path.as_os_str()],
            &env,
            &log,
            cancel,
        )?;
        // 5. confere: as bibliotecas importam (o `hello` do worker sai antes dos imports pesados) e o worker responde
        begin(on_progress)?;
        run(&venv_python, &["-c".as_ref(), "import faster_whisper, sherpa_onnx, av".as_ref()], &[], &log, cancel)?;
        fsx::write_atomic(&p.worker, WORKER_PY.as_bytes())?;
        let mut engine = ProcessEngine::spawn(WorkerLaunch { python: venv_python, script: p.worker.clone(), fake: false, low_priority: false, kill_after_s: 120 })
            .map_err(|e| setup_failed(format!("worker hello: {e}")))?;
        super::engine::Engine::shutdown(&mut engine);
        // troca atômica: o venv antigo sai do caminho só depois de o novo estar provado
        let old = p.root.join("venv.old");
        let _ = std::fs::remove_dir_all(&old);
        if p.venv.exists() {
            std::fs::rename(&p.venv, &old)?;
        }
        std::fs::rename(&venv_new, &p.venv)?;
        let _ = std::fs::remove_dir_all(&old);
        let manifest = expected_manifest(db::now());
        fsx::write_atomic(&p.manifest, &serde_json::to_vec_pretty(&manifest)?)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&cache);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&venv_new);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_missing_outdated_ready_fake() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        assert_eq!(status_with(dir.path(), false).unwrap().state, "missing");
        assert_eq!(status_with(dir.path(), true).unwrap().state, "fake");
        std::fs::create_dir_all(p.python.parent().unwrap()).unwrap();
        std::fs::write(&p.python, b"").unwrap();
        // manifest com sinal de runtime de outra época
        let mut m = expected_manifest("2026-01-01T00:00:00".into());
        std::fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        let st = status_with(dir.path(), false).unwrap();
        assert_eq!((st.state.as_str(), st.installed_at.as_deref()), ("ready", Some("2026-01-01T00:00:00")));
        for tweak in [|m: &mut Manifest| m.runtime_version += 1, |m: &mut Manifest| m.lock_sha256 = "0".into(), |m: &mut Manifest| m.uv = "0.0.1".into(), |m: &mut Manifest| m.python = "0.0.1".into()] {
            tweak(&mut m);
            std::fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
            assert_eq!(status_with(dir.path(), false).unwrap().state, "outdated");
            m = expected_manifest("2026-01-01T00:00:00".into());
        }
        // manifest sem o python do venv: não está pronto
        std::fs::remove_file(&p.python).unwrap();
        std::fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(status_with(dir.path(), false).unwrap().state, "missing");
        // manifest ilegível = nada instalado
        std::fs::write(&p.manifest, b"{").unwrap();
        assert_eq!(status_with(dir.path(), false).unwrap().state, "missing");
    }

    fn installed(dir: &Path) -> RuntimePaths {
        let p = paths(dir);
        std::fs::create_dir_all(p.python.parent().unwrap()).unwrap();
        std::fs::write(&p.python, b"").unwrap();
        p
    }

    #[test]
    fn worker_only_change_is_refreshed_silently() {
        let dir = tempfile::tempdir().unwrap();
        let p = installed(dir.path());
        let mut m = expected_manifest("2026-01-01T00:00:00".into());
        m.worker_sha256 = "0".into();
        std::fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        std::fs::write(&p.worker, b"# worker antigo").unwrap();
        let st = status_with(dir.path(), false).unwrap();
        assert_eq!((st.state.as_str(), st.installed_at.as_deref()), ("ready", Some("2026-01-01T00:00:00")));
        assert_eq!(std::fs::read_to_string(&p.worker).unwrap(), WORKER_PY);
        assert_eq!(read_manifest(&p).unwrap(), expected_manifest("2026-01-01T00:00:00".into()));
        // sem venv novo nem sobras de gravação
        assert!(!p.root.join("venv.new").exists() && !fsx::tmp_sibling(&p.worker).exists());
    }

    #[test]
    fn worker_change_with_other_difference_stays_outdated() {
        let dir = tempfile::tempdir().unwrap();
        let p = installed(dir.path());
        let mut m = expected_manifest("2026-01-01T00:00:00".into());
        m.worker_sha256 = "0".into();
        m.lock_sha256 = "0".into();
        std::fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(status_with(dir.path(), false).unwrap().state, "outdated");
        assert!(!p.worker.exists());
        assert_eq!(read_manifest(&p).unwrap(), m);
    }

    #[test]
    fn worker_refresh_failure_reports_outdated() {
        let dir = tempfile::tempdir().unwrap();
        let p = installed(dir.path());
        let mut m = expected_manifest("2026-01-01T00:00:00".into());
        m.worker_sha256 = "0".into();
        std::fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        // worker.py é um diretório: o rename atômico falha
        std::fs::create_dir(&p.worker).unwrap();
        assert_eq!(status_with(dir.path(), false).unwrap().state, "outdated");
        assert_eq!(read_manifest(&p).unwrap(), m);
    }

    #[test]
    fn cancel_kills_the_whole_command_group() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let cancel = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::Relaxed);
            });
            let t = std::time::Instant::now();
            let sleep = Path::new("/bin/sh");
            let e = run(sleep, &["-c".as_ref(), "sleep 30".as_ref()], &[], &log, &cancel).unwrap_err();
            assert_eq!(e.code(), "setup_cancelled");
            assert!(t.elapsed() < Duration::from_secs(5));
        });
    }

    #[test]
    fn failing_command_reports_the_log_tail() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let e = run(Path::new("/bin/sh"), &["-c".as_ref(), "echo boom-detail >&2; exit 3".as_ref()], &[], &log, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(e.code(), "setup_failed");
        assert!(e.to_string().contains("boom-detail"), "{e}");
    }

    #[test]
    fn embedded_files_are_present() {
        assert!(WORKER_PY.contains("hello"));
        assert!(LOCK.contains("--hash=sha256:") && LOCK.contains("av==18."));
    }
}
