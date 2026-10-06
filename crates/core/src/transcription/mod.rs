//! Transcrição e separação de falantes (fase 4). Contrato: TRANSCRIPTION_CONTRACT.md (local).
//!
//! Camadas (de baixo para cima): `protocol` (mensagens do worker Python) → `engine` (quem executa um pedido:
//! `FakeEngine` em memória, `ProcessEngine` com o worker real) → `staging`/`assemble`/`commit` (bruto no banco,
//! montagem pura, versão numa transação) → `runner` (uma tarefa de ponta a ponta) → `queue` (tabela de tarefas
//! em `app.db`). `runtime` e `models` instalam o Python fixado e os modelos. A fila é da GUI (thread no shell);
//! o núcleo não cria threads de agendamento.
pub mod assemble;
pub mod commit;
pub mod engine;
pub mod keys;
pub mod models;
pub mod params;
pub mod protocol;
pub mod queue;
pub mod runner;
pub mod runtime;
pub mod staging;

/// Variável de ambiente de teste: `1` = shell usa o `ProcessEngine` com `worker.py --fake` (Python do
/// sistema, sem runtime nem modelos; instantâneo); `slow` = idem com 300 ms entre segmentos (testes de
/// cancelar/matar). Valor que é um caminho existente = esse interpretador em vez de `python3`.
pub const FAKE_WORKER_ENV: &str = "TARY_FAKE_WORKER";

/// Dentro de um AppImage, o runtime exporta `PYTHONHOME`/`PYTHONPATH` e põe as pastas do pacote na frente de
/// `PATH`/`LD_LIBRARY_PATH`: o Python do runtime herdaria isso e morreria antes do `hello` ("No module named
/// 'encodings'"). Tira o que veio do AppImage antes de iniciar o worker ou o `uv`; fora de um AppImage, nada muda.
pub(crate) fn host_env(cmd: &mut std::process::Command) {
    let appdir = std::env::var("APPDIR").unwrap_or_default();
    for (k, v) in host_env_fixes(&appdir, |k| std::env::var(k).ok()) {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
}

fn host_env_fixes(appdir: &str, get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, Option<String>)> {
    if appdir.is_empty() {
        return vec![];
    }
    let mut out: Vec<_> = ["PYTHONHOME", "PYTHONPATH", "PYTHONDONTWRITEBYTECODE"].map(|k| (k, None)).into();
    for k in ["PATH", "LD_LIBRARY_PATH"] {
        let Some(v) = get(k) else { continue };
        let kept: Vec<_> = v.split(':').filter(|e| !e.is_empty() && !e.starts_with(appdir)).collect();
        out.push((k, (!kept.is_empty()).then(|| kept.join(":"))));
    }
    out
}

#[cfg(test)]
mod host_env_tests {
    use super::host_env_fixes;

    #[test]
    fn strips_appimage_python_and_paths() {
        let get = |k: &str| match k {
            "PATH" => Some("/tmp/.mount_x/usr/bin/:/tmp/.mount_x/bin/:/usr/local/bin:/usr/bin".into()),
            "LD_LIBRARY_PATH" => Some("/tmp/.mount_x/usr/lib/:/tmp/.mount_x/lib64/:".into()),
            _ => None,
        };
        let fixes = host_env_fixes("/tmp/.mount_x", get);
        assert_eq!(
            fixes,
            vec![
                ("PYTHONHOME", None),
                ("PYTHONPATH", None),
                ("PYTHONDONTWRITEBYTECODE", None),
                ("PATH", Some("/usr/local/bin:/usr/bin".into())),
                ("LD_LIBRARY_PATH", None),
            ]
        );
        assert!(host_env_fixes("", get).is_empty());
    }
}
