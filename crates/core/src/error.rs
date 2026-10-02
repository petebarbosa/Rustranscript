use std::fmt;

/// Erros do núcleo. `code()` é estável (vai para o JSON da CLI e para a UI, que traduz).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("ambiguous reference: {0}")]
    Ambiguous(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("database was created by a newer version (schema {found}, supported {supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("audio: {0}")]
    Audio(String),
    /// Erros da gravação (fase 3). O código vem junto e é estável: `already_recording`, `not_recording`,
    /// `device_not_found`, `device_open_failed`, `backend_unavailable`, `capture_failed`, `invalid_wav`,
    /// `empty_recording`, `not_implemented`... (lista em RECORDING_CONTRACT.md).
    #[error("recording ({0}): {1}")]
    Recording(&'static str, String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::Ambiguous(_) => "ambiguous",
            Error::Invalid(_) => "invalid",
            Error::Conflict(_) => "conflict",
            Error::SchemaTooNew { .. } => "schema_too_new",
            Error::Audio(_) => "audio",
            Error::Recording(code, _) => code,
            Error::Db(_) => "database",
            Error::Io(_) => "io",
            Error::Json(_) => "json",
        }
    }

    /// Detalhe sem o prefixo do tipo (a camada de apresentação traduz o prefixo pelo `code`).
    pub fn detail(&self) -> String {
        match self {
            Error::NotFound(s) | Error::Ambiguous(s) | Error::Invalid(s) | Error::Conflict(s) | Error::Audio(s) => {
                s.clone()
            }
            Error::Recording(_, s) => s.clone(),
            other => other.to_string(),
        }
    }

    pub fn invalid(msg: impl fmt::Display) -> Self {
        Error::Invalid(msg.to_string())
    }

    pub fn not_found(msg: impl fmt::Display) -> Self {
        Error::NotFound(msg.to_string())
    }

    pub fn recording(code: &'static str, msg: impl fmt::Display) -> Self {
        Error::Recording(code, msg.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<recorder::Error> for Error {
    fn from(e: recorder::Error) -> Self {
        match e {
            recorder::Error::Io(e) => Error::Io(e),
            recorder::Error::Json(e) => Error::Json(e),
            // `Session` (uso incorreto da API) vira `invalid`, como no resto do núcleo
            recorder::Error::Session(s) => Error::Invalid(s),
            other => Error::Recording(other.code(), other.detail()),
        }
    }
}
