//! Integração do AppImage com o menu de apps. AppImage não se integra sozinho: ao abrir a janela com
//! `$APPIMAGE` definido (o runtime põe ali o caminho do `.AppImage`), grava um `.desktop` em
//! `$XDG_DATA_HOME/applications` (Exec = esse caminho) e os ícones em `icons/hicolor`. Só escreve o
//! que mudou, nunca mexe em `.desktop` sem a nossa marca e nunca atrapalha a abertura (erro vai
//! para o stderr). `tary desktop remove` desfaz. No pacote do Arch o `APPIMAGE` não existe: nada acontece.
use std::path::{Path, PathBuf};

use core_lib::{Error, Result, fsx, paths};

/// Nome do arquivo (sem extensão) e do ícone.
const ID: &str = "transcriptary";
/// Chave que marca o `.desktop` como nosso: sem ela o arquivo é de outra instalação e fica como está.
const MARKER: &str = "X-Transcriptary-AppImage=true";
/// Base do `.desktop` (o mesmo do pacote do Arch); aqui só o `Exec=` é trocado.
const TEMPLATE: &str = include_str!("../../packaging/linux/transcriptary.desktop");
const ICONS: [(&str, &[u8]); 5] = [
    ("32x32", include_bytes!("../icons/32x32.png")),
    ("64x64", include_bytes!("../icons/64x64.png")),
    ("128x128", include_bytes!("../icons/128x128.png")),
    ("256x256", include_bytes!("../icons/128x128@2x.png")),
    ("512x512", include_bytes!("../icons/icon.png")),
];

/// Chamada no início da janela; em segundo plano, para não atrasar nada.
pub fn integrate_appimage() {
    let Some(appimage) = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()).map(PathBuf::from) else { return };
    let Some(home) = paths::xdg_data_home() else { return };
    std::thread::spawn(move || match install(&home, &appimage) {
        Ok(true) => update_database(&home.join("applications")),
        Ok(false) => {}
        Err(e) => eprintln!("desktop: {}", e.detail()),
    });
}

/// `update-desktop-database` atualiza o cache de tipos MIME; ausente ou com erro, tanto faz.
fn update_database(apps: &Path) {
    let child = std::process::Command::new("update-desktop-database")
        .arg(apps)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut c) = child {
        let _ = c.wait();
    }
}

fn desktop_file(data_home: &Path) -> PathBuf {
    data_home.join("applications").join(format!("{ID}.desktop"))
}

fn icon_file(data_home: &Path, size: &str) -> PathBuf {
    data_home.join("icons/hicolor").join(size).join("apps").join(format!("{ID}.png"))
}

/// Nosso (tem a marca) ou de outro: `None` se o arquivo não existe.
fn is_ours(path: &Path) -> Result<Option<bool>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).lines().any(|l| l.trim() == MARKER))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Valor de `Exec=`: entre aspas, com `"`, `` ` ``, `$` e `\` escapados (regra do Exec) e depois a
/// barra invertida dobrada (regra de string do arquivo, aplicada antes da outra: `\` vira quatro).
/// `%` literal é `%%` (senão é código de campo).
fn exec_value(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Conteúdo do `.desktop` para esse AppImage. `None` se o caminho não serve (relativo ou com
/// caractere de controle, que não cabe numa linha do arquivo).
fn desktop_entry(appimage: &Path) -> Option<String> {
    let path = appimage.to_str()?;
    if !appimage.is_absolute() || path.chars().any(char::is_control) {
        return None;
    }
    let mut out = String::new();
    for line in TEMPLATE.lines() {
        if line.starts_with("Exec=") {
            out.push_str(&format!("Exec={}\n", exec_value(path)));
            // some do menu se o arquivo for apagado; TryExec é string comum (só a barra é escapada)
            out.push_str(&format!("TryExec={}\n", path.replace('\\', "\\\\")));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(MARKER);
    out.push('\n');
    Some(out)
}

/// Grava só o que difere do que já está em disco. `Ok(true)` se algo foi escrito.
fn write_if_changed(path: &Path, content: &[u8]) -> Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == content) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    fsx::write_atomic(path, content)?;
    Ok(true)
}

/// Cria ou atualiza o `.desktop` e os ícones sob `data_home`. `Ok(false)` se nada mudou (ou se o
/// `.desktop` existente não é nosso).
fn install(data_home: &Path, appimage: &Path) -> Result<bool> {
    let entry = desktop_entry(appimage).ok_or_else(|| Error::Invalid(format!("APPIMAGE: {}", appimage.display())))?;
    let desktop = desktop_file(data_home);
    if is_ours(&desktop)? == Some(false) {
        return Ok(false);
    }
    let mut written = false;
    for (size, bytes) in ICONS {
        written |= write_if_changed(&icon_file(data_home, size), bytes)?;
    }
    written |= write_if_changed(&desktop, entry.as_bytes())?;
    Ok(written)
}

/// Apaga o que `install` gravou, se o `.desktop` for nosso; devolve os arquivos removidos.
pub fn remove() -> Result<Vec<PathBuf>> {
    let home = paths::xdg_data_home().ok_or_else(|| Error::NotFound("$XDG_DATA_HOME".into()))?;
    let removed = remove_from(&home)?;
    if !removed.is_empty() {
        update_database(&home.join("applications"));
    }
    Ok(removed)
}

fn remove_from(data_home: &Path) -> Result<Vec<PathBuf>> {
    let desktop = desktop_file(data_home);
    if is_ours(&desktop)? != Some(true) {
        return Ok(vec![]);
    }
    let mut removed = vec![];
    for path in std::iter::once(desktop).chain(ICONS.iter().map(|(size, _)| icon_file(data_home, size))) {
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lê de volta o `Exec=` como o desktop lê: escape de string (`\\` → `\`) e depois a regra de aspas.
    fn parse_exec(value: &str) -> String {
        let mut unescaped = String::new();
        let mut it = value.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                unescaped.push(it.next().unwrap());
            } else {
                unescaped.push(c);
            }
        }
        let inner = unescaped.strip_prefix('"').unwrap().strip_suffix('"').unwrap();
        let mut out = String::new();
        let mut it = inner.chars();
        while let Some(c) = it.next() {
            match c {
                '\\' => out.push(it.next().unwrap()),
                '%' => {
                    assert_eq!(it.next(), Some('%'));
                    out.push('%');
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn key<'a>(entry: &'a str, k: &str) -> &'a str {
        entry.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap()
    }

    #[test]
    fn template_has_what_the_entry_needs() {
        for k in ["Name", "GenericName", "Comment", "Comment[pt_BR]", "Comment[es]", "Categories", "Keywords", "Icon", "Terminal", "StartupWMClass"] {
            assert!(TEMPLATE.lines().any(|l| l.starts_with(&format!("{k}="))), "falta {k} no modelo");
        }
        assert_eq!(TEMPLATE.lines().filter(|l| l.starts_with("Exec=")).count(), 1);
    }

    #[test]
    fn entry_quotes_and_escapes_the_path() {
        let path = r#"/home/ana/My Apps/a"b$c`d\e%f.AppImage"#;
        let entry = desktop_entry(Path::new(path)).unwrap();
        assert_eq!(parse_exec(key(&entry, "Exec")), path);
        assert_eq!(key(&entry, "Exec"), r#""/home/ana/My Apps/a\\"b\\$c\\`d\\\\e%%f.AppImage""#);
        assert_eq!(key(&entry, "TryExec"), r#"/home/ana/My Apps/a"b$c`d\\e%f.AppImage"#);
        assert_eq!(key(&entry, "Icon"), "transcriptary");
        assert_eq!(key(&entry, "Terminal"), "false");
        assert_eq!(entry.lines().last(), Some(MARKER));
        assert_eq!(entry.lines().filter(|l| l.starts_with("Exec=")).count(), 1);
    }

    #[test]
    fn entry_rejects_relative_and_control_paths() {
        assert!(desktop_entry(Path::new("Transcriptary.AppImage")).is_none());
        assert!(desktop_entry(Path::new("/tmp/a\nb.AppImage")).is_none());
    }

    #[test]
    fn install_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let app = Path::new("/opt/Transcriptary.AppImage");
        assert!(install(home.path(), app).unwrap());
        assert!(desktop_file(home.path()).is_file());
        for (size, bytes) in ICONS {
            assert_eq!(std::fs::read(icon_file(home.path(), size)).unwrap(), bytes);
        }
        assert!(!install(home.path(), app).unwrap());
        // um ícone alterado é refeito, e só ele
        std::fs::write(icon_file(home.path(), "64x64"), b"x").unwrap();
        assert!(install(home.path(), app).unwrap());
        assert!(!install(home.path(), app).unwrap());
    }

    #[test]
    fn install_leaves_foreign_desktop_alone() {
        let home = tempfile::tempdir().unwrap();
        let desktop = desktop_file(home.path());
        std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        std::fs::write(&desktop, "[Desktop Entry]\nName=Meu\n").unwrap();
        assert!(!install(home.path(), Path::new("/opt/T.AppImage")).unwrap());
        assert_eq!(std::fs::read_to_string(&desktop).unwrap(), "[Desktop Entry]\nName=Meu\n");
        assert!(!icon_file(home.path(), "32x32").exists());
        assert!(remove_from(home.path()).unwrap().is_empty());
        assert!(desktop.is_file());
    }

    #[test]
    fn install_rewrites_when_the_path_changes() {
        let home = tempfile::tempdir().unwrap();
        assert!(install(home.path(), Path::new("/a/T.AppImage")).unwrap());
        assert!(install(home.path(), Path::new("/b/T.AppImage")).unwrap());
        let entry = std::fs::read_to_string(desktop_file(home.path())).unwrap();
        assert_eq!(key(&entry, "Exec"), "\"/b/T.AppImage\"");
        assert_eq!(key(&entry, "TryExec"), "/b/T.AppImage");
    }

    #[test]
    fn remove_deletes_only_ours() {
        let home = tempfile::tempdir().unwrap();
        assert!(remove_from(home.path()).unwrap().is_empty());
        install(home.path(), Path::new("/a/T.AppImage")).unwrap();
        let removed = remove_from(home.path()).unwrap();
        assert_eq!(removed.len(), 1 + ICONS.len());
        assert!(!desktop_file(home.path()).exists());
        assert!(ICONS.iter().all(|(size, _)| !icon_file(home.path(), size).exists()));
        assert!(remove_from(home.path()).unwrap().is_empty());
    }
}
