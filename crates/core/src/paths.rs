//! Onde ficam os dados. O nome é definitivo (`rustranscript`); os diretórios do nome antigo
//! (`transcricoes`) são migrados sozinhos na primeira execução (`migrate_legacy`).
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "rustranscript";
pub const DATA_DIR_ENV: &str = "RSTT_DATA_DIR";
/// Nome e variável de antes da renomeação: só leitura (fallback e migração).
const LEGACY_APP_NAME: &str = "transcricoes";
const LEGACY_DATA_DIR_ENV: &str = "TRANSCRICOES_DATA_DIR";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Diretório de dados escolhido pelo usuário (sobrescreve o padrão do SO).
    pub data_dir: Option<PathBuf>,
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", APP_NAME)
}

pub fn config_file() -> PathBuf {
    project_dirs()
        .map(|d| d.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from(".").join(format!("{APP_NAME}.config.json")))
}

pub fn load_config() -> Config {
    std::fs::read_to_string(config_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(cfg: &Config) -> crate::Result<()> {
    let path = config_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::fsx::write_atomic(&path, serde_json::to_string_pretty(cfg)?.as_bytes())
}

/// `RSTT_DATA_DIR`, ou a variável antiga quando a nova não está definida.
fn env_data_dir() -> Option<OsString> {
    [DATA_DIR_ENV, LEGACY_DATA_DIR_ENV].into_iter().find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
}

/// Ordem: argumento → variável de ambiente → config.json → padrão do SO (`~/.local/share/rustranscript`).
pub fn resolve_data_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = env_data_dir() {
        return PathBuf::from(p);
    }
    if let Some(p) = load_config().data_dir {
        return p;
    }
    project_dirs()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(APP_NAME))
}

/// Diretório só do usuário para o socket local (`$XDG_RUNTIME_DIR/rustranscript`).
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p).join(APP_NAME),
        None => data_dir.join("run"),
    }
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join("ipc.sock")
}

/// Locais do nome antigo e do novo (config, dados) e o socket da app antiga.
struct Migration {
    old_config: PathBuf,
    new_config: PathBuf,
    old_data: PathBuf,
    new_data: PathBuf,
    old_socket: PathBuf,
}

impl Migration {
    fn system() -> Option<Self> {
        let old = directories::ProjectDirs::from("", "", LEGACY_APP_NAME)?;
        let new = directories::ProjectDirs::from("", "", APP_NAME)?;
        let old_socket = match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(p) => PathBuf::from(p).join(LEGACY_APP_NAME).join("ipc.sock"),
            None => old.data_dir().join("run").join("ipc.sock"),
        };
        Some(Self {
            old_config: old.config_dir().to_path_buf(),
            new_config: new.config_dir().to_path_buf(),
            old_data: old.data_dir().to_path_buf(),
            new_data: new.data_dir().to_path_buf(),
            old_socket,
        })
    }
}

/// Existe (inclusive symlink, mesmo quebrado).
fn present(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// Move `from` para `to` com um único `rename`. Corrida (outro processo moveu antes: origem sumiu ou destino já
/// existe e não está vazio) não é erro; devolve se foi este processo que moveu.
fn move_once(from: &Path, to: &Path) -> std::io::Result<bool> {
    if !present(from) || present(to) {
        return Ok(false);
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(true),
        Err(e) if !present(from) || e.raw_os_error() == Some(libc::ENOTEMPTY) || e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// A app antiga está aberta e a migração dos dados está pendente: a app nova não pode seguir, senão criaria o
/// diretório novo vazio e a migração nunca mais aconteceria (as chamadas antigas pareceriam perdidas).
#[derive(Debug, PartialEq)]
pub struct LegacyRunning {
    /// Diretório de dados da app antiga (para a mensagem ao usuário).
    pub old_data: PathBuf,
}

/// Migra do nome antigo (`transcricoes`) para o novo, uma vez, antes de qualquer `resolve_data_dir`: primeiro o
/// diretório de config (o `config.json` pode apontar para um diretório de dados próprio), depois o de dados, só
/// se for o padrão do SO (sem `--data-dir`, sem variável de ambiente, sem `data_dir` no config). Idempotente e
/// segura contra CLI e GUI abrindo juntas (um `rename` só: um vence, o outro vê "origem sumiu"). Falhas vão para
/// o stderr e não impedem a app de abrir; a única exceção é `Err(LegacyRunning)`: migração pendente com a app
/// antiga aberta; quem chama deve avisar o usuário e sair, sem tocar nos dados.
pub fn migrate_legacy(explicit_data_dir: Option<&Path>) -> Result<(), LegacyRunning> {
    let Some(m) = Migration::system() else { return Ok(()) };
    migrate_with(&m, explicit_data_dir.is_some() || env_data_dir().is_some())
}

/// `data_dir` do `config.json` (já no lugar novo, ou ainda no antigo se a config não foi movida).
fn config_has_data_dir(m: &Migration) -> bool {
    [&m.new_config, &m.old_config].into_iter().find_map(|d| std::fs::read_to_string(d.join("config.json")).ok()).is_some_and(|s| {
        serde_json::from_str::<Config>(&s).ok().is_some_and(|c| c.data_dir.is_some())
    })
}

/// Sobrou dado antigo no diretório padrão antigo enquanto o novo também existe (nada foi migrado).
fn stranded(m: &Migration) -> bool {
    present(&m.new_data) && std::fs::read_dir(&m.old_data).is_ok_and(|mut d| d.next().is_some())
}

fn migrate_with(m: &Migration, custom_data_dir: bool) -> Result<(), LegacyRunning> {
    let default_dir = !custom_data_dir && !config_has_data_dir(m);
    // antes de mover qualquer coisa: com a app antiga aberta (o socket atende), mover a pasta debaixo dela
    // corromperia a gravação em curso, e seguir sem mover deixaria a migração permanentemente para trás
    if default_dir
        && present(&m.old_data)
        && !present(&m.new_data)
        && std::os::unix::net::UnixStream::connect(&m.old_socket).is_ok()
    {
        return Err(LegacyRunning { old_data: m.old_data.clone() });
    }
    match move_once(&m.old_config, &m.new_config) {
        Ok(true) => eprintln!("rstt: configuração migrada de {} para {}", m.old_config.display(), m.new_config.display()),
        Ok(false) => {}
        Err(e) => eprintln!("rstt: não consegui migrar a configuração de {}: {e}", m.old_config.display()),
    }
    if !default_dir {
        return Ok(());
    }
    match move_once(&m.old_data, &m.new_data) {
        Ok(true) => eprintln!("rstt: dados migrados de {} para {}", m.old_data.display(), m.new_data.display()),
        Ok(false) => {}
        Err(e) => {
            eprintln!("rstt: não consegui migrar os dados de {}: {e}", m.old_data.display());
            return Ok(());
        }
    }
    if stranded(m) {
        eprintln!(
            "rstt: aviso: {} ainda tem dados da versão anterior, mas {} já existe, então nada foi migrado; \
             mova o conteúdo à mão se precisar dele",
            m.old_data.display(),
            m.new_data.display()
        );
    }
    // idempotente; roda sempre para também curar uma migração interrompida entre o `rename` e o reparo
    if let Err(e) = repair_venv(&m.old_data, &m.new_data) {
        eprintln!("rstt: não consegui reparar o ambiente Python em {}: {e}", m.new_data.display());
    }
    Ok(())
}

/// O venv do uv guarda caminhos absolutos: `venv/bin/python` (symlink para o Python gerenciado), o symlink de
/// versão menor em `runtime/python/` e `home = ...` no `pyvenv.cfg`. Troca o prefixo `old_root` por `new_root`
/// nesses três lugares (nada mais no runtime tem caminho absoluto); sem o prefixo antigo, não faz nada.
fn repair_venv(old_root: &Path, new_root: &Path) -> std::io::Result<()> {
    let runtime = new_root.join("runtime");
    for dir in [runtime.join("venv").join("bin"), runtime.join("python")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let link = entry.path();
            let Ok(target) = std::fs::read_link(&link) else { continue };
            let Ok(rest) = target.strip_prefix(old_root) else { continue };
            // symlink novo ao lado + rename: nunca há um instante sem o link
            let mut tmp = link.clone().into_os_string();
            tmp.push(format!(".rstt-{}-{:?}", std::process::id(), std::thread::current().id()));
            let tmp = PathBuf::from(tmp);
            let _ = std::fs::remove_file(&tmp);
            std::os::unix::fs::symlink(new_root.join(rest), &tmp)?;
            std::fs::rename(&tmp, &link)?;
        }
    }
    let cfg = runtime.join("venv").join("pyvenv.cfg");
    if let Ok(text) = std::fs::read_to_string(&cfg) {
        let fixed = replace_root(&text, &old_root.to_string_lossy(), &new_root.to_string_lossy());
        if fixed != text {
            crate::fsx::write_atomic(&cfg, fixed.as_bytes()).map_err(|e| std::io::Error::other(e.to_string()))?;
        }
    }
    Ok(())
}

/// Troca `old` por `new` só onde `old` é um caminho inteiro (seguido de `/`, espaço, aspas ou fim), para não
/// pegar `<old>-outro`.
fn replace_root(text: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (i, _) in text.match_indices(old) {
        let end = i + old.len();
        if i < last || !text[end..].chars().next().is_none_or(|c| c == '/' || c.is_whitespace() || c == '"' || c == '\'') {
            continue;
        }
        out.push_str(&text[last..i]);
        out.push_str(new);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn migration(root: &Path) -> Migration {
        Migration {
            old_config: root.join("config/transcricoes"),
            new_config: root.join("config/rustranscript"),
            old_data: root.join("share/transcricoes"),
            new_data: root.join("share/rustranscript"),
            old_socket: root.join("run/transcricoes/ipc.sock"),
        }
    }

    /// Árvore antiga sintética: DB falso, config e um venv no formato do uv (symlinks e `home` absolutos).
    fn fake_old_tree(m: &Migration) {
        let py_dir = m.old_data.join("runtime/python/cpython-3.12.15-linux-x86_64-gnu");
        std::fs::create_dir_all(py_dir.join("bin")).unwrap();
        std::fs::write(py_dir.join("bin/python3.12"), b"#!/bin/sh\n").unwrap();
        symlink(&py_dir, m.old_data.join("runtime/python/cpython-3.12-linux-x86_64-gnu")).unwrap();
        let venv = m.old_data.join("runtime/venv");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        symlink(py_dir.join("bin/python3.12"), venv.join("bin/python")).unwrap();
        symlink("python", venv.join("bin/python3")).unwrap();
        std::fs::write(
            venv.join("pyvenv.cfg"),
            format!("home = {}/bin\nimplementation = CPython\nversion_info = 3.12.15\n", py_dir.display()),
        )
        .unwrap();
        std::fs::write(m.old_data.join("app.db"), b"db").unwrap();
        std::fs::create_dir_all(&m.old_config).unwrap();
        std::fs::write(m.old_config.join("config.json"), "{}").unwrap();
    }

    #[test]
    fn moves_config_and_data_and_repairs_the_venv() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        migrate_with(&m, false).unwrap();
        assert!(!present(&m.old_data) && !present(&m.old_config));
        assert_eq!(std::fs::read(m.new_data.join("app.db")).unwrap(), b"db");
        assert!(m.new_config.join("config.json").is_file());
        let py = m.new_data.join("runtime/venv/bin/python");
        assert_eq!(
            std::fs::read_link(&py).unwrap(),
            m.new_data.join("runtime/python/cpython-3.12.15-linux-x86_64-gnu/bin/python3.12")
        );
        assert!(py.exists(), "o symlink do venv tem de resolver");
        // symlink relativo fica como está
        assert_eq!(std::fs::read_link(m.new_data.join("runtime/venv/bin/python3")).unwrap(), Path::new("python"));
        let minor = m.new_data.join("runtime/python/cpython-3.12-linux-x86_64-gnu");
        assert!(minor.join("bin/python3.12").exists());
        let cfg = std::fs::read_to_string(m.new_data.join("runtime/venv/pyvenv.cfg")).unwrap();
        assert!(cfg.starts_with(&format!("home = {}/runtime/python/", m.new_data.display())), "{cfg}");
        assert!(!cfg.contains("transcricoes"), "{cfg}");
        assert!(cfg.contains("implementation = CPython"));
    }

    #[test]
    fn second_call_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        migrate_with(&m, false).unwrap();
        std::fs::write(m.new_data.join("novo.txt"), b"x").unwrap();
        migrate_with(&m, false).unwrap();
        assert!(m.new_data.join("novo.txt").is_file() && m.new_data.join("app.db").is_file());
        assert!(!present(&m.old_data));
    }

    #[test]
    fn no_move_when_new_exists() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        std::fs::create_dir_all(&m.new_data).unwrap();
        std::fs::create_dir_all(&m.new_config).unwrap();
        migrate_with(&m, false).unwrap();
        assert!(m.old_data.join("app.db").is_file() && m.old_config.join("config.json").is_file());
        assert_eq!(std::fs::read_dir(&m.new_data).unwrap().count(), 0);
    }

    #[test]
    fn no_data_move_with_explicit_dir_or_env() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        migrate_with(&m, true).unwrap();
        assert!(m.old_data.join("app.db").is_file() && !present(&m.new_data));
        // a config, porém, migra mesmo assim
        assert!(m.new_config.join("config.json").is_file());
    }

    #[test]
    fn no_data_move_when_config_has_custom_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        std::fs::write(m.old_config.join("config.json"), r#"{"data_dir": "/tmp/meus-dados"}"#).unwrap();
        migrate_with(&m, false).unwrap();
        // a config foi movida primeiro e só então lida
        assert!(m.new_config.join("config.json").is_file() && !present(&m.old_config));
        assert!(m.old_data.join("app.db").is_file() && !present(&m.new_data));
    }

    #[test]
    fn config_moves_before_data_decision() {
        // config com `data_dir: null` (sem dir próprio) não impede a migração dos dados
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        std::fs::write(m.old_config.join("config.json"), r#"{"data_dir": null}"#).unwrap();
        migrate_with(&m, false).unwrap();
        assert!(m.new_config.join("config.json").is_file() && m.new_data.join("app.db").is_file());
    }

    #[test]
    fn pending_migration_with_the_old_app_running_is_blocked_and_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        std::fs::create_dir_all(m.old_socket.parent().unwrap()).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&m.old_socket).unwrap();
        assert_eq!(migrate_with(&m, false), Err(LegacyRunning { old_data: m.old_data.clone() }));
        // nada se moveu (nem a config) e o diretório novo não foi criado
        assert!(m.old_data.join("app.db").is_file() && m.old_config.join("config.json").is_file());
        assert!(!present(&m.new_data) && !present(&m.new_config));
        // com o diretório de dados escolhido pelo usuário, a app antiga aberta não bloqueia
        assert_eq!(migrate_with(&m, true), Ok(()));
        // depois que a app antiga fecha (socket some), a migração acontece normalmente
        drop(_listener);
        assert_eq!(migrate_with(&m, false), Ok(()));
        assert!(m.new_data.join("app.db").is_file() && !present(&m.old_data));
    }

    #[test]
    fn old_app_running_does_not_block_when_nothing_is_pending() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        std::fs::create_dir_all(m.old_socket.parent().unwrap()).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&m.old_socket).unwrap();
        // sem diretório antigo: nada a migrar
        assert_eq!(migrate_with(&m, false), Ok(()));
        // com o novo já existente: não é migração pendente (só o aviso de dados encalhados)
        fake_old_tree(&m);
        std::fs::create_dir_all(&m.new_data).unwrap();
        assert_eq!(migrate_with(&m, false), Ok(()));
    }

    #[test]
    fn stranded_old_data_is_detected_only_when_both_exist_and_old_is_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        assert!(!stranded(&m), "nenhum existe");
        fake_old_tree(&m);
        assert!(!stranded(&m), "só o antigo existe: ainda vai migrar");
        std::fs::create_dir_all(&m.new_data).unwrap();
        assert!(stranded(&m), "os dois existem e o antigo tem dados");
        // a chamada só avisa: nada se move e não há erro
        assert_eq!(migrate_with(&m, false), Ok(()));
        assert!(m.old_data.join("app.db").is_file() && std::fs::read_dir(&m.new_data).unwrap().count() == 0);
        // antigo vazio: nada encalhado
        std::fs::remove_dir_all(&m.old_data).unwrap();
        std::fs::create_dir_all(&m.old_data).unwrap();
        assert!(!stranded(&m));
    }

    #[test]
    fn concurrent_starts_do_not_fail() {
        let dir = tempfile::tempdir().unwrap();
        let m = migration(dir.path());
        fake_old_tree(&m);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| migrate_with(&m, false));
            }
        });
        assert!(m.new_data.join("app.db").is_file() && !present(&m.old_data));
        assert!(m.new_data.join("runtime/venv/bin/python").exists());
    }

    #[test]
    fn replace_root_only_matches_whole_paths() {
        let t = "home = /a/old/bin\nx = /a/old-other/bin\ny = \"/a/old\"\nz = /a/old";
        assert_eq!(replace_root(t, "/a/old", "/b/new"), "home = /b/new/bin\nx = /a/old-other/bin\ny = \"/b/new\"\nz = /b/new");
    }
}
