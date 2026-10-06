//! Tipos de saída. Os nomes de campo (inglês, snake_case) são o contrato JSON da CLI e da UI.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct LibraryInfo {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub available: bool,
    pub call_count: i64,
    pub unassigned_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub id: i64,
    pub library_id: i64,
    pub name: String,
    pub slug: String,
    pub call_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallSummary {
    pub library_id: i64,
    pub id: i64,
    pub key: String,
    pub title: String,
    pub client_id: Option<i64>,
    pub client_name: Option<String>,
    pub started_at: String,
    pub duration_s: i64,
    pub words: usize,
    pub preview: String,
    pub edited_blocks: i64,
    pub versions: i64,
    pub has_audio: bool,
    /// `pending` | `running` | `done` | `failed` (ver `TRANSCRIPTION_*`). Chamada recém-gravada = `pending`,
    /// sem nenhuma versão (`versions = 0`, `words = 0`, `preview` vazio).
    pub transcription_state: String,
    /// Mensagem do último erro quando `failed`.
    pub transcription_error: Option<String>,
}

pub const TRANSCRIPTION_PENDING: &str = "pending";
pub const TRANSCRIPTION_RUNNING: &str = "running";
pub const TRANSCRIPTION_DONE: &str = "done";
pub const TRANSCRIPTION_FAILED: &str = "failed";

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptInfo {
    pub id: i64,
    pub version: i64,
    pub model: Option<String>,
    pub engine: Option<String>,
    pub source_file: Option<String>,
    pub created_at: String,
    pub is_active: bool,
    /// Tem o bruto da transcrição (`tx_*`): só então `rediarize`/`resegment` são possíveis. Importadas: `false`.
    pub has_raw: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeakerInfo {
    pub id: i64,
    pub track: String,
    pub label: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockInfo {
    pub id: i64,
    pub seq: i64,
    pub t_start: f64,
    pub t_end: f64,
    pub speaker_id: i64,
    pub text: String,
    pub original_text: String,
    pub edited: bool,
    /// Preenchido = bloco excluído (exclusão lógica; texto, `seq` e áudio continuam). Fora de `blocks` da chamada.
    pub deleted_at: Option<String>,
}

/// Resultado de `delete_blocks`/`restore_blocks`: o que mudou e o que já estava no estado pedido
/// (idempotente: excluir um bloco já excluído, ou restaurar um vivo, não é erro e não grava histórico).
#[derive(Debug, Clone, Serialize)]
pub struct BlocksChange {
    pub changed: Vec<BlockInfo>,
    pub unchanged: Vec<BlockInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub t: f64,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioInfo {
    pub mic_path: Option<String>,
    pub sys_path: Option<String>,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallDetail {
    #[serde(flatten)]
    pub summary: CallSummary,
    pub library_name: String,
    pub language: Option<String>,
    pub expected_speakers: Option<i64>,
    /// `None` quando a chamada ainda não tem transcrição (gravação recém-feita): `transcripts`,
    /// `speakers`, `blocks` e `chapters` vêm vazios e `transcription_state` explica o motivo.
    pub transcript_id: Option<i64>,
    pub transcripts: Vec<TranscriptInfo>,
    pub speakers: Vec<SpeakerInfo>,
    pub blocks: Vec<BlockInfo>,
    /// Blocos excluídos da versão (com `deleted_at`), para `rstt edit restore` achar o `seq`.
    pub deleted_blocks: Vec<BlockInfo>,
    pub chapters: Vec<Chapter>,
    pub audio: AudioInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub call_id: i64,
    pub entity: String,
    pub entity_id: i64,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub origin: String,
    pub at: String,
    pub undone_at: Option<String>,
    /// Alterações feitas juntas (p.ex. glossário aplicado à chamada) compartilham o `batch_id`
    /// e são desfeitas juntas. `None` = edição avulsa.
    pub batch_id: Option<i64>,
    /// Tipo do lote: `"glossary"`, `"delete"` ou `"restore"` (exclusão/restauração de blocos).
    pub batch_kind: Option<String>,
    /// Quantas entradas o lote tem (para "glossário aplicado (N blocos)").
    pub batch_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub library_id: i64,
    pub call_id: i64,
    pub call_key: String,
    pub call_title: String,
    pub started_at: String,
    /// `None` quando o acerto foi no título.
    pub block_id: Option<i64>,
    pub t_start: Option<f64>,
    /// Trecho com os termos entre `\u{2}` e `\u{3}` (a UI troca por destaque).
    pub snippet: String,
    pub rank: f64,
}

/// Quem fez a alteração (vai para `edit_history.origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Ui,
    Cli,
    Import,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Ui => "ui",
            Origin::Cli => "cli",
            Origin::Import => "import",
        }
    }
}

// ------------------------------------------------------------------ glossário

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    /// Palavra/expressão a favorecer na transcrição (vira o prompt do modelo).
    Term,
    /// "errado → certo", aplicada ao texto.
    Replace,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::Term => "term",
            RuleKind::Replace => "replace",
        }
    }

    pub fn parse(s: &str) -> Option<RuleKind> {
        match s {
            "term" => Some(RuleKind::Term),
            "replace" => Some(RuleKind::Replace),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Client,
}

/// Regra do glossário. Os ids de regras globais e de cliente vivem em espaços separados:
/// quem identifica uma regra é o par `(scope, id)` (e `library_id` para as de cliente).
#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub id: i64,
    pub scope: Scope,
    /// Só em regras de cliente.
    pub library_id: Option<i64>,
    pub client_id: Option<i64>,
    pub kind: RuleKind,
    pub pattern: String,
    pub replacement: Option<String>,
    pub case_sensitive: bool,
    pub created_at: String,
    /// `edit_history.id` da edição que originou a regra (sugestão aceita); vale na biblioteca
    /// do cliente ou, em regra global, em `source_library_id`.
    pub source_edit_id: Option<i64>,
    pub source_library_id: Option<i64>,
    /// Regra global escondida por uma de cliente com o mesmo padrão (não é aplicada na lista efetiva).
    pub overridden: bool,
}

/// Uma regra que agiu: quantas trocas fez.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub scope: Option<Scope>,
    pub rule_id: Option<i64>,
    pub pattern: String,
    pub replacement: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockChange {
    pub block_id: i64,
    pub seq: i64,
    pub before: String,
    pub after: String,
    pub rules: Vec<Hit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub call_id: i64,
    pub transcript_id: i64,
    pub dry_run: bool,
    pub blocks_changed: usize,
    /// Total de trocas (uma regra pode agir mais de uma vez no mesmo bloco).
    pub replacements: usize,
    /// Lote no histórico (desfeito de uma vez por `undo`); `None` em simulação ou sem mudanças.
    pub batch_id: Option<i64>,
    pub changes: Vec<BlockChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientRef {
    pub id: i64,
    pub name: String,
}

/// Sugestão de regra a partir de uma edição de bloco.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockSuggestion {
    pub pattern: String,
    pub replacement: String,
    /// Em quantos OUTROS blocos da mesma versão o padrão ainda aparece.
    pub occurrences_in_call: usize,
    /// Cliente da chamada (`null` na inbox/sem cliente: só dá para criar regra global).
    pub client: Option<ClientRef>,
}

/// Resultado de editar um bloco: o bloco (campos no nível de cima, como antes) + sugestões.
#[derive(Debug, Clone, Serialize)]
pub struct BlockEdit {
    #[serde(flatten)]
    pub block: BlockInfo,
    /// `edit_history.id` da edição (`null` se nada mudou ou em simulação); vira `source_edit_id`.
    pub edit_id: Option<i64>,
    pub suggestions: Vec<BlockSuggestion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportEntry {
    pub line: usize,
    pub kind: RuleKind,
    pub pattern: String,
    pub replacement: Option<String>,
    /// `added`, `duplicate` ou `invalid`
    pub status: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlossaryImportReport {
    pub dry_run: bool,
    pub added: usize,
    /// Duplicadas (já existentes ou repetidas no arquivo).
    pub skipped: usize,
    pub invalid: usize,
    pub entries: Vec<ImportEntry>,
}
