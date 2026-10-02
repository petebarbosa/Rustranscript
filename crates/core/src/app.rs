//! `app.db`: bibliotecas cadastradas (empresas/projetos + a inbox "Não classificadas"),
//! configurações e, nas próximas fases, glossário global e modelos.
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::library::Library;
use crate::model::LibraryInfo;
use crate::{Error, Result, db, schema};

pub const INBOX_DIR: &str = "inbox";

pub struct App {
    pub data_dir: PathBuf,
    pub db: Connection,
}

#[derive(Debug, Clone)]
pub struct LibraryRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub root: PathBuf,
}

impl LibraryRow {
    pub fn is_inbox(&self) -> bool {
        self.kind == "inbox"
    }
}

impl App {
    pub fn open(data_dir: &Path) -> Result<App> {
        std::fs::create_dir_all(data_dir)?;
        let data_dir = data_dir.canonicalize()?;
        let db = db::open(&data_dir.join("app.db"), schema::APP_MIGRATIONS)?;
        db.execute(
            "INSERT OR IGNORE INTO libraries (name, kind, path, created_at)
             SELECT '', 'inbox', '', ?1 WHERE NOT EXISTS (SELECT 1 FROM libraries WHERE kind = 'inbox')",
            [db::now()],
        )?;
        Ok(App { data_dir, db })
    }

    fn row(&self, id: i64, name: String, kind: String, path: String) -> LibraryRow {
        let root = if kind == "inbox" { self.data_dir.join(INBOX_DIR) } else { PathBuf::from(path) };
        LibraryRow { id, name, kind, root }
    }

    pub fn library_rows(&self) -> Result<Vec<LibraryRow>> {
        let mut stmt = self
            .db
            .prepare("SELECT id, name, kind, path FROM libraries ORDER BY kind = 'company', name COLLATE NOCASE")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().map(|(id, n, k, p)| self.row(id, n, k, p)).collect())
    }

    pub fn library_row(&self, id: i64) -> Result<LibraryRow> {
        self.library_rows()?
            .into_iter()
            .find(|l| l.id == id)
            .ok_or_else(|| Error::not_found(format!("library {id}")))
    }

    pub fn inbox_id(&self) -> Result<i64> {
        Ok(self.db.query_row("SELECT id FROM libraries WHERE kind = 'inbox'", [], |r| r.get(0))?)
    }

    pub fn open_library(&self, id: i64) -> Result<Library> {
        Library::open(self.library_row(id)?)
    }

    /// Resolve uma biblioteca por id numérico ou pelo nome (sem diferenciar caixa).
    pub fn find_library(&self, reference: &str) -> Result<LibraryRow> {
        let rows = self.library_rows()?;
        if let Ok(id) = reference.parse::<i64>()
            && let Some(r) = rows.iter().find(|r| r.id == id)
        {
            return Ok(r.clone());
        }
        let wanted = reference.to_lowercase();
        let hits: Vec<_> = rows.into_iter().filter(|r| !r.is_inbox() && r.name.to_lowercase() == wanted).collect();
        match hits.len() {
            0 => Err(Error::not_found(format!("library {reference}"))),
            1 => Ok(hits.into_iter().next().unwrap()),
            _ => Err(Error::Ambiguous(format!("library {reference}"))),
        }
    }

    pub fn libraries(&self) -> Result<Vec<LibraryInfo>> {
        let mut out = Vec::new();
        for row in self.library_rows()? {
            let available = row.root.join(crate::library::DB_FILE).is_file() || row.is_inbox();
            let (call_count, unassigned_count) = if available {
                let lib = Library::open(row.clone())?;
                lib.counts()?
            } else {
                (0, 0)
            };
            out.push(LibraryInfo {
                id: row.id,
                name: row.name.clone(),
                kind: row.kind.clone(),
                path: row.root.display().to_string(),
                available,
                call_count,
                unassigned_count,
            });
        }
        Ok(out)
    }

    /// Cadastra uma empresa/projeto. Se a pasta já tiver um `library.db` (backup copiado,
    /// outra máquina), ele é adotado como está.
    pub fn add_library(&self, name: &str, path: &Path) -> Result<LibraryRow> {
        let name = crate::text::normalize_ws(name);
        if name.is_empty() {
            return Err(Error::invalid("library name is empty"));
        }
        std::fs::create_dir_all(path)?;
        let root = path.canonicalize()?;
        if root.starts_with(&self.data_dir) {
            return Err(Error::invalid("library folder must be outside the app data folder"));
        }
        let exists: Option<i64> = self
            .db
            .query_row("SELECT id FROM libraries WHERE kind = 'company' AND path = ?1", [root.to_string_lossy()], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_some() {
            return Err(Error::Conflict(format!("library already registered: {}", root.display())));
        }
        self.db.execute(
            "INSERT INTO libraries (name, kind, path, created_at) VALUES (?1, 'company', ?2, ?3)",
            params![name, root.to_string_lossy(), db::now()],
        )?;
        let row = self.library_row(self.db.last_insert_rowid())?;
        Library::open(row.clone())?; // cria library.db
        Ok(row)
    }

    pub fn rename_library(&self, id: i64, name: &str) -> Result<()> {
        let name = crate::text::normalize_ws(name);
        if name.is_empty() {
            return Err(Error::invalid("library name is empty"));
        }
        let n = self.db.execute("UPDATE libraries SET name = ?1 WHERE id = ?2 AND kind = 'company'", params![name, id])?;
        if n == 0 { Err(Error::not_found(format!("library {id}"))) } else { Ok(()) }
    }

    /// Só descadastra; a pasta e o `library.db` continuam no disco.
    pub fn remove_library(&self, id: i64) -> Result<()> {
        let n = self.db.execute("DELETE FROM libraries WHERE id = ?1 AND kind = 'company'", [id])?;
        if n == 0 { Err(Error::not_found(format!("library {id}"))) } else { Ok(()) }
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.db.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, v],
            )?,
            None => self.db.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    pub fn settings(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let mut stmt = self.db.prepare("SELECT key, value FROM settings ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut map = serde_json::Map::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, serde_json::Value::String(v));
        }
        Ok(map)
    }

    /// Localiza uma chamada em qualquer biblioteca. Aceita a chave (`call_2026-10-01_08-21-52`),
    /// o nome do arquivo antigo com slug/versão, ou `<biblioteca>:<id>`.
    pub fn find_call(&self, reference: &str) -> Result<(i64, i64)> {
        if let Some((lib, id)) = reference.split_once(':')
            && let (Ok(lib), Ok(id)) = (lib.parse::<i64>(), id.parse::<i64>())
        {
            let library = self.open_library(lib)?;
            library.call_exists(id)?;
            return Ok((lib, id));
        }
        // 1º a chave exata (`call_..._2` é a chave de uma gravação do mesmo segundo, não um slug); só se
        // ninguém tiver essa chave, o nome do arquivo antigo (slug/versão) vira a chave base.
        let mut candidates = vec![reference.to_string()];
        if let Some(stem) = crate::parse::parse_stem(reference)
            && stem.key != reference
        {
            candidates.push(stem.key);
        }
        let libs = self
            .library_rows()?
            .into_iter()
            .filter(|row| row.root.join(crate::library::DB_FILE).is_file())
            .map(|row| Ok((row.id, Library::open(row)?)))
            .collect::<Result<Vec<_>>>()?;
        let mut hits = Vec::new();
        for key in &candidates {
            for (lib_id, lib) in &libs {
                if let Some(id) = lib.call_id_by_key(key)? {
                    hits.push((*lib_id, id));
                }
            }
            if !hits.is_empty() {
                break;
            }
        }
        match hits.len() {
            0 => Err(Error::not_found(format!("call {reference}"))),
            1 => Ok(hits[0]),
            _ => Err(Error::Ambiguous(format!("call {reference}"))),
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        db::checkpoint(&self.db);
    }
}
