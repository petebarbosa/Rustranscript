//! Utilidades de arquivo.
use std::path::{Path, PathBuf};

use crate::Result;

pub fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    // sufixo do pid: dois processos gravando o mesmo arquivo (CLI + app) não pisam no temporário um do outro
    let mut tmp = tmp_sibling(path).into_os_string();
    tmp.push(format!(".{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = std::fs::write(&tmp, content).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

pub fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// Move um arquivo ou diretório; entre sistemas de arquivos diferentes, copia e apaga.
pub fn move_path(from: &Path, to: &Path) -> Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if to.exists() {
        return Err(crate::Error::Conflict(format!("destination exists: {}", to.display())));
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_recursive(from, to)?;
            if from.is_dir() {
                std::fs::remove_dir_all(from)?;
            } else {
                std::fs::remove_file(from)?;
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn copy_recursive(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// Remove diretórios vazios subindo até `stop` (exclusive).
pub fn prune_empty_dirs(mut dir: &Path, stop: &Path) {
    while dir != stop && dir.starts_with(stop) {
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
}
