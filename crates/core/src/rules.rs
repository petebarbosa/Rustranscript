//! Glossário, parte com banco: regras globais (`app.db`) e de cliente (`library.db`), fusão
//! das duas camadas, aplicação a uma chamada (com lote no histórico), sugestões a partir de
//! edições, importação de listas de termos e termos do prompt. A lógica pura fica em `glossary.rs`.
use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Deserialize;

use crate::app::App;
use crate::glossary::{self, Engine, ReplaceRule, norm_key};
use crate::library::{self, Library};
use crate::model::*;
use crate::text::{self, MAX_BLOCK_CHARS};
use crate::{Error, Result, db};

pub const MAX_PATTERN_CHARS: usize = 200;
pub const MAX_REPLACEMENT_CHARS: usize = 500;
/// Tamanho máximo de um arquivo de termos importado.
pub const MAX_LIST_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Dados editáveis de uma regra.
#[derive(Debug, Clone, Deserialize)]
pub struct RuleInput {
    pub kind: RuleKind,
    pub pattern: String,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub case_sensitive: bool,
}

impl RuleInput {
    pub fn term(pattern: &str) -> RuleInput {
        RuleInput { kind: RuleKind::Term, pattern: pattern.into(), replacement: None, case_sensitive: false }
    }

    pub fn replace(pattern: &str, replacement: &str) -> RuleInput {
        RuleInput { kind: RuleKind::Replace, pattern: pattern.into(), replacement: Some(replacement.into()), case_sensitive: false }
    }

    /// Normaliza espaços e valida: padrão não vazio; `replace` precisa de uma substituição não
    /// vazia e diferente do padrão (a comparação respeita a caixa: "api" → "API" vale);
    /// `term` não tem substituição.
    pub fn validated(&self) -> Result<RuleInput> {
        let pattern = text::normalize_ws(&self.pattern);
        if pattern.is_empty() {
            return Err(Error::invalid("pattern is empty"));
        }
        if pattern.chars().count() > MAX_PATTERN_CHARS {
            return Err(Error::invalid(format!("pattern longer than {MAX_PATTERN_CHARS} characters")));
        }
        let replacement = match self.kind {
            RuleKind::Term => None,
            RuleKind::Replace => {
                let r = text::normalize_ws(self.replacement.as_deref().unwrap_or(""));
                if r.is_empty() {
                    return Err(Error::invalid("replacement is empty"));
                }
                if r == pattern {
                    return Err(Error::invalid("replacement is the same as the pattern"));
                }
                if r.chars().count() > MAX_REPLACEMENT_CHARS {
                    return Err(Error::invalid(format!("replacement longer than {MAX_REPLACEMENT_CHARS} characters")));
                }
                Some(r)
            }
        };
        Ok(RuleInput { kind: self.kind, pattern, replacement, case_sensitive: self.case_sensitive })
    }
}

/// De onde veio a regra: a edição (`edit_history.id`) que originou a sugestão aceita.
#[derive(Debug, Clone, Copy)]
pub struct RuleSource {
    pub library_id: i64,
    pub edit_id: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum ImportScope {
    Global,
    Client { library_id: i64, client_id: i64 },
}

// ------------------------------------------------------------------ acesso às tabelas

#[derive(Clone, Copy)]
enum Owner {
    Global,
    Client { library_id: i64, client_id: i64 },
}

const GLOBAL_COLS: &str = "id, kind, pattern, replacement, case_sensitive, created_at, source_edit_id, source_library_id";
const CLIENT_COLS: &str = "id, client_id, kind, pattern, replacement, case_sensitive, created_at, source_edit_id";

fn kind_of(s: String) -> RuleKind {
    RuleKind::parse(&s).unwrap_or(RuleKind::Term) // o CHECK da tabela só deixa 'term' e 'replace'
}

fn global_from_row(r: &Row) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: r.get(0)?,
        scope: Scope::Global,
        library_id: None,
        client_id: None,
        kind: kind_of(r.get(1)?),
        pattern: r.get(2)?,
        replacement: r.get(3)?,
        case_sensitive: r.get(4)?,
        created_at: r.get(5)?,
        source_edit_id: r.get(6)?,
        source_library_id: r.get(7)?,
        overridden: false,
    })
}

fn client_from_row(library_id: i64, r: &Row) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: r.get(0)?,
        scope: Scope::Client,
        library_id: Some(library_id),
        client_id: Some(r.get(1)?),
        kind: kind_of(r.get(2)?),
        pattern: r.get(3)?,
        replacement: r.get(4)?,
        case_sensitive: r.get(5)?,
        created_at: r.get(6)?,
        source_edit_id: r.get(7)?,
        source_library_id: None,
        overridden: false,
    })
}

pub(crate) fn global_rules_in(conn: &Connection) -> Result<Vec<Rule>> {
    let mut stmt = conn.prepare(&format!("SELECT {GLOBAL_COLS} FROM glossary_global ORDER BY id"))?;
    Ok(stmt.query_map([], global_from_row)?.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn client_rules_in(conn: &Connection, library_id: i64, client_id: i64) -> Result<Vec<Rule>> {
    let mut stmt = conn.prepare(&format!("SELECT {CLIENT_COLS} FROM glossary_client WHERE client_id = ?1 ORDER BY id"))?;
    Ok(stmt.query_map([client_id], |r| client_from_row(library_id, r))?.collect::<rusqlite::Result<_>>()?)
}

fn owner_rules(conn: &Connection, owner: Owner) -> Result<Vec<Rule>> {
    match owner {
        Owner::Global => global_rules_in(conn),
        Owner::Client { library_id, client_id } => client_rules_in(conn, library_id, client_id),
    }
}

fn fetch_rule(conn: &Connection, owner: Owner, id: i64) -> Result<Rule> {
    let found = match owner {
        Owner::Global => conn
            .query_row(&format!("SELECT {GLOBAL_COLS} FROM glossary_global WHERE id = ?1"), [id], global_from_row)
            .optional()?,
        Owner::Client { library_id, .. } => conn
            .query_row(&format!("SELECT {CLIENT_COLS} FROM glossary_client WHERE id = ?1"), [id], |r| client_from_row(library_id, r))
            .optional()?,
    };
    found.ok_or_else(|| Error::not_found(format!("rule {id}")))
}

/// Duas regras são a mesma quando têm o mesmo tipo e o mesmo padrão (sem caixa, espaços normalizados).
fn same_identity(r: &Rule, kind: RuleKind, pattern: &str) -> bool {
    r.kind == kind && norm_key(&r.pattern) == norm_key(pattern)
}

fn conflict(r: &Rule) -> Error {
    Error::Conflict(format!("rule already exists: {} ({})", r.pattern, r.kind.as_str()))
}

fn insert_raw(conn: &Connection, owner: Owner, input: &RuleInput, source: Option<RuleSource>) -> Result<Rule> {
    let (kind, cs, now) = (input.kind.as_str(), input.case_sensitive, db::now());
    match owner {
        Owner::Global => conn.execute(
            "INSERT INTO glossary_global (kind, pattern, replacement, case_sensitive, created_at, source_edit_id, source_library_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![kind, input.pattern, input.replacement, cs, now, source.map(|s| s.edit_id), source.map(|s| s.library_id)],
        )?,
        Owner::Client { client_id, .. } => conn.execute(
            "INSERT INTO glossary_client (client_id, kind, pattern, replacement, case_sensitive, created_at, source_edit_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![client_id, kind, input.pattern, input.replacement, cs, now, source.map(|s| s.edit_id)],
        )?,
    };
    fetch_rule(conn, owner, conn.last_insert_rowid())
}

fn insert_rule(conn: &Connection, owner: Owner, input: &RuleInput, source: Option<RuleSource>) -> Result<Rule> {
    let input = input.validated()?;
    if let Some(dup) = owner_rules(conn, owner)?.iter().find(|r| same_identity(r, input.kind, &input.pattern)) {
        return Err(conflict(dup));
    }
    insert_raw(conn, owner, &input, source)
}

fn update_rule(conn: &Connection, owner: Owner, id: i64, input: &RuleInput) -> Result<Rule> {
    let input = input.validated()?;
    fetch_rule(conn, owner, id)?;
    if let Some(dup) = owner_rules(conn, owner)?.iter().find(|r| r.id != id && same_identity(r, input.kind, &input.pattern)) {
        return Err(conflict(dup));
    }
    let table = if matches!(owner, Owner::Global) { "glossary_global" } else { "glossary_client" };
    conn.execute(
        &format!("UPDATE {table} SET kind = ?1, pattern = ?2, replacement = ?3, case_sensitive = ?4 WHERE id = ?5"),
        params![input.kind.as_str(), input.pattern, input.replacement, input.case_sensitive, id],
    )?;
    fetch_rule(conn, owner, id)
}

fn delete_rule(conn: &Connection, owner: Owner, id: i64) -> Result<Rule> {
    let rule = fetch_rule(conn, owner, id)?;
    let table = if matches!(owner, Owner::Global) { "glossary_global" } else { "glossary_client" };
    conn.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?;
    Ok(rule)
}

// ------------------------------------------------------------------ fusão das camadas

/// Regras do cliente primeiro, depois as globais. Uma global some (`overridden`) quando o
/// cliente tem regra do mesmo tipo e padrão (sem caixa, espaços normalizados): o cliente
/// sobrescreve. Termos e substituições não se sobrescrevem entre si.
pub fn merge(global: Vec<Rule>, client: Vec<Rule>) -> Vec<Rule> {
    let keys: HashSet<(RuleKind, String)> = client.iter().map(|r| (r.kind, norm_key(&r.pattern))).collect();
    let mut out = client;
    out.extend(global.into_iter().map(|mut r| {
        r.overridden = keys.contains(&(r.kind, norm_key(&r.pattern)));
        r
    }));
    out
}

/// Só as regras `replace` em vigor, prontas para o motor.
pub fn replace_rules(rules: &[Rule]) -> Vec<ReplaceRule> {
    rules
        .iter()
        .filter(|r| r.kind == RuleKind::Replace && !r.overridden)
        .filter_map(|r| {
            Some(ReplaceRule {
                scope: Some(r.scope),
                id: Some(r.id),
                pattern: r.pattern.clone(),
                replacement: r.replacement.clone()?,
                case_sensitive: r.case_sensitive,
            })
        })
        .collect()
}

// ------------------------------------------------------------------ aplicação a uma transcrição

/// Aplica as regras `replace` aos blocos de uma versão, dentro da transação do chamador. As
/// trocas passam pelo mesmo caminho de `set_block_text` (`edited_at`, FTS, histórico) e entram
/// num lote só, para o `undo` desfazer tudo junto.
pub(crate) fn apply_in_tx(tx: &Connection, call_id: i64, transcript_id: i64, engine: &Engine, origin: Origin) -> Result<ApplyReport> {
    let mut report = ApplyReport { call_id, transcript_id, dry_run: false, blocks_changed: 0, replacements: 0, batch_id: None, changes: vec![] };
    if engine.is_empty() {
        return Ok(report);
    }
    let blocks: Vec<(i64, i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, seq, text FROM blocks WHERE transcript_id = ?1 ORDER BY seq")?;
        stmt.query_map([transcript_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut batch = None;
    for (block_id, seq, before) in blocks {
        let applied = engine.apply(&before);
        if applied.text == before || applied.text.chars().count() > MAX_BLOCK_CHARS {
            continue;
        }
        let b = match batch {
            Some(b) => b,
            None => {
                let b = library::new_batch(tx, "glossary")?;
                batch = Some(b);
                b
            }
        };
        library::write_block_text(tx, call_id, block_id, &applied.text, origin, Some(b))?;
        report.replacements += applied.replacements();
        report.changes.push(BlockChange { block_id, seq, before, after: applied.text, rules: applied.hits });
    }
    report.blocks_changed = report.changes.len();
    report.batch_id = batch.map(|b| b.id);
    Ok(report)
}

// ------------------------------------------------------------------ App: camada global e fusão

impl App {
    pub fn global_rules(&self) -> Result<Vec<Rule>> {
        global_rules_in(&self.db)
    }

    pub fn add_global_rule(&self, input: &RuleInput, source: Option<RuleSource>) -> Result<Rule> {
        insert_rule(&self.db, Owner::Global, input, source)
    }

    pub fn update_global_rule(&self, id: i64, input: &RuleInput) -> Result<Rule> {
        update_rule(&self.db, Owner::Global, id, input)
    }

    /// Devolve a regra removida.
    pub fn remove_global_rule(&self, id: i64) -> Result<Rule> {
        delete_rule(&self.db, Owner::Global, id)
    }

    /// Regras visíveis num contexto: as do cliente (se a chamada tem cliente) + as globais,
    /// estas marcadas com `overridden` quando o cliente tem a sua. Sem cliente (inbox), só as globais.
    pub fn merged_rules(&self, lib: &Library, client_id: Option<i64>) -> Result<Vec<Rule>> {
        let client = match client_id {
            Some(c) if !lib.row.is_inbox() => lib.client_rules(c)?,
            _ => vec![],
        };
        Ok(merge(self.global_rules()?, client))
    }

    /// `merged_rules` sem as globais escondidas.
    pub fn effective_rules(&self, lib: &Library, client_id: Option<i64>) -> Result<Vec<Rule>> {
        Ok(self.merged_rules(lib, client_id)?.into_iter().filter(|r| !r.overridden).collect())
    }

    /// Cliente → global: cria a regra global e remove a cópia do cliente. Se já existe uma global
    /// idêntica, só remove a cópia; se existe com outra substituição, é conflito. As duas pontas
    /// estão em bancos diferentes, então a global entra primeiro e é desfeita se a remoção falhar.
    pub fn promote_rule(&self, lib: &Library, rule_id: i64) -> Result<Rule> {
        let rule = lib.client_rule(rule_id)?;
        let input = RuleInput { kind: rule.kind, pattern: rule.pattern.clone(), replacement: rule.replacement.clone(), case_sensitive: rule.case_sensitive };
        let existing = self.global_rules()?.into_iter().find(|g| same_identity(g, rule.kind, &rule.pattern));
        let (global, created) = match existing {
            Some(g) if g.replacement == rule.replacement && g.case_sensitive == rule.case_sensitive => (g, false),
            Some(g) => return Err(conflict(&g)),
            None => {
                let source = rule.source_edit_id.map(|edit_id| RuleSource { library_id: lib.id(), edit_id });
                (self.add_global_rule(&input, source)?, true)
            }
        };
        if let Err(e) = lib.remove_client_rule(rule_id) {
            if created {
                let _ = self.remove_global_rule(global.id);
            }
            return Err(e);
        }
        Ok(global)
    }

    /// Aplica o glossário em vigor (global + cliente da chamada) a uma versão da chamada
    /// (padrão: a ativa). Com `dry_run` só mostra as mudanças.
    pub fn apply_glossary(
        &self,
        lib: &mut Library,
        call_id: i64,
        transcript_id: Option<i64>,
        origin: Origin,
        dry_run: bool,
    ) -> Result<ApplyReport> {
        let client_id = lib.call_client_id(call_id)?;
        let rules = self.effective_rules(lib, client_id)?;
        lib.apply_glossary_rules(&rules, call_id, transcript_id, origin, dry_run)
    }

    /// Termos (`term`) em vigor para o prompt do modelo: os do cliente primeiro, sem repetir, até
    /// o orçamento de ~224 tokens (ver `glossary::estimate_tokens`).
    pub fn prompt_terms(&self, lib: &Library, client_id: Option<i64>) -> Result<Vec<String>> {
        let terms: Vec<String> = self
            .effective_rules(lib, client_id)?
            .into_iter()
            .filter(|r| r.kind == RuleKind::Term)
            .map(|r| r.pattern)
            .collect();
        Ok(glossary::select_prompt_terms(&terms, glossary::PROMPT_TOKEN_BUDGET))
    }

    /// Edita o texto de um bloco e devolve também as sugestões de regra que a edição inspira.
    pub fn edit_block_text(&self, lib: &mut Library, block_id: i64, new_text: &str, origin: Origin, dry_run: bool) -> Result<BlockEdit> {
        let (block, edit_id, old) = lib.set_block_text_full(block_id, new_text, origin, dry_run)?;
        let suggestions = if old != block.text { self.block_edit_suggestions(lib, block_id, &old, &block.text)? } else { vec![] };
        Ok(BlockEdit { block, edit_id: edit_id.filter(|_| !dry_run), suggestions })
    }

    /// Sugestões de `suggest_from_edit(old, new)` que ainda não têm regra de `replace` (em
    /// qualquer camada, inclusive global escondida), com a contagem de OUTROS blocos da mesma
    /// versão em que o padrão ainda aparece e o cliente da chamada.
    pub fn block_edit_suggestions(&self, lib: &Library, block_id: i64, old: &str, new: &str) -> Result<Vec<BlockSuggestion>> {
        let found = glossary::suggest_from_edit(old, new);
        if found.is_empty() {
            return Ok(vec![]);
        }
        let (call_id, transcript_id): (i64, i64) = lib
            .conn
            .query_row(
                "SELECT t.call_id, t.id FROM blocks b JOIN transcripts t ON t.id = b.transcript_id WHERE b.id = ?1",
                [block_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error::not_found(format!("block {block_id}")))?;
        let client_id = lib.call_client_id(call_id)?.filter(|_| !lib.row.is_inbox());
        let rules = self.merged_rules(lib, client_id)?;
        let client = match client_id {
            Some(id) => {
                let name: String = lib.conn.query_row("SELECT name FROM clients WHERE id = ?1", [id], |r| r.get(0))?;
                Some(ClientRef { id, name })
            }
            None => None,
        };
        let others: Vec<String> = {
            let mut stmt = lib.conn.prepare("SELECT text FROM blocks WHERE transcript_id = ?1 AND id != ?2")?;
            stmt.query_map(params![transcript_id, block_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        Ok(found
            .into_iter()
            .filter(|s| !rules.iter().any(|r| same_identity(r, RuleKind::Replace, &s.pattern)))
            .map(|s| BlockSuggestion {
                occurrences_in_call: others.iter().filter(|t| glossary::count_matches(t, &s.pattern, false) > 0).count(),
                pattern: s.pattern,
                replacement: s.replacement,
                client: client.clone(),
            })
            .collect())
    }

    /// Importa uma lista de termos em UTF-8 (ver `glossary::parse_list`). `kind_hint` força o
    /// tipo esperado: `Term` recusa linhas `errado -> certo`; `Replace` recusa linhas sem seta;
    /// `None` aceita as duas. Duplicatas (já existentes ou repetidas no arquivo) são puladas.
    pub fn import_glossary_file(&self, path: &Path, scope: ImportScope, kind_hint: Option<RuleKind>, dry_run: bool) -> Result<GlossaryImportReport> {
        let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::not_found(path.display()),
            _ => Error::Io(e),
        })?;
        if meta.len() > MAX_LIST_FILE_BYTES {
            return Err(Error::invalid(format!("file larger than {} MB", MAX_LIST_FILE_BYTES / 1024 / 1024)));
        }
        let content = String::from_utf8(std::fs::read(path)?).map_err(|_| Error::invalid("file is not valid UTF-8"))?;
        self.import_glossary_text(&content, scope, kind_hint, dry_run)
    }

    pub fn import_glossary_text(&self, content: &str, scope: ImportScope, kind_hint: Option<RuleKind>, dry_run: bool) -> Result<GlossaryImportReport> {
        let lib;
        let (conn, owner) = match scope {
            ImportScope::Global => (&self.db, Owner::Global),
            ImportScope::Client { library_id, client_id } => {
                lib = self.open_library(library_id)?;
                if lib.row.is_inbox() {
                    return Err(Error::invalid("unclassified calls have no clients"));
                }
                lib.find_client(&client_id.to_string())?;
                (&lib.conn, Owner::Client { library_id, client_id })
            }
        };
        let mut seen: HashSet<(RuleKind, String)> = owner_rules(conn, owner)?.iter().map(|r| (r.kind, norm_key(&r.pattern))).collect();
        let tx = conn.unchecked_transaction()?;
        let mut report = GlossaryImportReport { dry_run, added: 0, skipped: 0, invalid: 0, entries: vec![] };
        for l in glossary::parse_list(content) {
            let kind = glossary::line_kind(&l);
            let mut entry = ImportEntry { line: l.line, kind, pattern: l.pattern.clone(), replacement: l.replacement.clone(), status: "added".into(), reason: None };
            let problem = if l.invalid {
                Some("empty_side".to_string())
            } else if kind_hint.is_some_and(|h| h != kind) {
                Some(if kind == RuleKind::Replace { "not_a_term" } else { "not_a_replacement" }.to_string())
            } else {
                let input = RuleInput { kind, pattern: l.pattern, replacement: l.replacement, case_sensitive: false };
                match input.validated() {
                    Err(e) => Some(e.detail()),
                    Ok(input) => {
                        if !seen.insert((kind, norm_key(&input.pattern))) {
                            entry.status = "duplicate".into();
                        } else if !dry_run {
                            insert_raw(&tx, owner, &input, None)?;
                        }
                        None
                    }
                }
            };
            if let Some(reason) = problem {
                entry.status = "invalid".into();
                entry.reason = Some(reason);
            }
            match entry.status.as_str() {
                "added" => report.added += 1,
                "duplicate" => report.skipped += 1,
                _ => report.invalid += 1,
            }
            report.entries.push(entry);
        }
        tx.commit()?;
        Ok(report)
    }
}

// ------------------------------------------------------------------ Library: camada do cliente

impl Library {
    /// Regras de um cliente desta biblioteca (só as dele, sem as globais).
    pub fn client_rules(&self, client_id: i64) -> Result<Vec<Rule>> {
        client_rules_in(&self.conn, self.id(), client_id)
    }

    pub fn client_rule(&self, id: i64) -> Result<Rule> {
        fetch_rule(&self.conn, Owner::Client { library_id: self.id(), client_id: 0 }, id)
    }

    fn client_owner(&self, client_id: i64) -> Result<Owner> {
        self.conn
            .query_row("SELECT 1 FROM clients WHERE id = ?1", [client_id], |_| Ok(()))
            .optional()?
            .ok_or_else(|| Error::not_found(format!("client {client_id}")))?;
        Ok(Owner::Client { library_id: self.id(), client_id })
    }

    /// `source_edit_id`: edição de origem quando a regra nasce de uma sugestão.
    pub fn add_client_rule(&self, client_id: i64, input: &RuleInput, source_edit_id: Option<i64>) -> Result<Rule> {
        let owner = self.client_owner(client_id)?;
        insert_rule(&self.conn, owner, input, source_edit_id.map(|edit_id| RuleSource { library_id: self.id(), edit_id }))
    }

    pub fn update_client_rule(&self, id: i64, input: &RuleInput) -> Result<Rule> {
        let client_id = self.client_rule(id)?.client_id.unwrap_or_default();
        update_rule(&self.conn, Owner::Client { library_id: self.id(), client_id }, id, input)
    }

    /// Devolve a regra removida.
    pub fn remove_client_rule(&self, id: i64) -> Result<Rule> {
        delete_rule(&self.conn, Owner::Client { library_id: self.id(), client_id: 0 }, id)
    }

    pub fn call_client_id(&self, call_id: i64) -> Result<Option<i64>> {
        self.conn
            .query_row("SELECT client_id FROM calls WHERE id = ?1", [call_id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::not_found(format!("call {}:{call_id}", self.id())))
    }

    /// Aplica regras já resolvidas (ver `App::effective_rules`) a uma versão da chamada.
    /// Tudo vira um lote no histórico (origem `ui`/`cli`: `undo` desfaz o lote inteiro).
    pub fn apply_glossary_rules(
        &mut self,
        rules: &[Rule],
        call_id: i64,
        transcript_id: Option<i64>,
        origin: Origin,
        dry_run: bool,
    ) -> Result<ApplyReport> {
        self.call_exists(call_id)?;
        let tid = match transcript_id {
            Some(t) => {
                self.conn
                    .query_row("SELECT 1 FROM transcripts WHERE id = ?1 AND call_id = ?2", params![t, call_id], |_| Ok(()))
                    .optional()?
                    .ok_or_else(|| Error::not_found(format!("transcript {t}")))?;
                t
            }
            None => self.active_transcript_id(call_id)?.ok_or_else(|| Error::not_found(format!("transcript of call {call_id}")))?,
        };
        let engine = Engine::new(&replace_rules(rules))?;
        let mut report = self.edit(dry_run, |tx| apply_in_tx(tx, call_id, tid, &engine, origin))?;
        report.dry_run = dry_run;
        if dry_run {
            report.batch_id = None;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, scope: Scope, kind: RuleKind, pattern: &str) -> Rule {
        Rule {
            id,
            scope,
            library_id: None,
            client_id: None,
            kind,
            pattern: pattern.into(),
            replacement: Some("x".into()),
            case_sensitive: false,
            created_at: String::new(),
            source_edit_id: None,
            source_library_id: None,
            overridden: false,
        }
    }

    #[test]
    fn client_overrides_global_by_kind_and_normalized_pattern() {
        let global = vec![
            rule(1, Scope::Global, RuleKind::Replace, "Gate  Wei"),
            rule(2, Scope::Global, RuleKind::Replace, "outra"),
            rule(3, Scope::Global, RuleKind::Term, "gate wei"),
        ];
        let client = vec![rule(1, Scope::Client, RuleKind::Replace, "gate wei")];
        let m = merge(global, client);
        let view: Vec<_> = m.iter().map(|r| (r.scope, r.id, r.overridden)).collect();
        assert_eq!(
            view,
            [(Scope::Client, 1, false), (Scope::Global, 1, true), (Scope::Global, 2, false), (Scope::Global, 3, false)]
        );
        let eff = replace_rules(&m);
        assert_eq!(eff.len(), 2, "a global escondida não vai para o motor");
        assert_eq!(eff[0].scope, Some(Scope::Client));
    }

    #[test]
    fn validation_rules() {
        assert!(RuleInput::term("  ").validated().is_err());
        assert!(RuleInput::replace("a", "").validated().is_err());
        assert!(RuleInput::replace("a", "a").validated().is_err());
        assert!(RuleInput::replace("a", " a ").validated().is_err(), "igual após normalizar");
        assert!(RuleInput::replace("api", "API").validated().is_ok(), "correção de caixa vale");
        let v = RuleInput { kind: RuleKind::Term, pattern: " Gate   Wei ".into(), replacement: Some("lixo".into()), case_sensitive: true }.validated().unwrap();
        assert_eq!((v.pattern.as_str(), v.replacement), ("Gate Wei", None));
        assert!(RuleInput::term(&"x".repeat(MAX_PATTERN_CHARS + 1)).validated().is_err());
    }
}
