//! Erros do crate. `code()` é estável: o núcleo o repassa para a CLI/UI, que traduzem pelo código.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Sem servidor de áudio / backend não suportado nesta plataforma.
    #[error("audio backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("could not open device: {0}")]
    OpenFailed(String),
    /// Falha de leitura no meio da captura (dispositivo sumiu etc.).
    #[error("capture failed: {0}")]
    Capture(String),
    #[error("invalid wav: {0}")]
    InvalidWav(String),
    /// Uso incorreto da API (pasta já tem gravação, nada para gravar, sessão já parada...).
    #[error("recording session: {0}")]
    Session(String),
    /// Stub ainda não implementado (some quando o agente A terminar o crate).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::BackendUnavailable(_) => "backend_unavailable",
            Error::DeviceNotFound(_) => "device_not_found",
            Error::OpenFailed(_) => "device_open_failed",
            Error::Capture(_) => "capture_failed",
            Error::InvalidWav(_) => "invalid_wav",
            Error::Session(_) => "invalid",
            Error::NotImplemented(_) => "not_implemented",
            Error::Io(_) => "io",
            Error::Json(_) => "json",
        }
    }

    /// Detalhe sem o prefixo do tipo (a apresentação traduz o prefixo pelo `code`).
    pub fn detail(&self) -> String {
        match self {
            Error::BackendUnavailable(s)
            | Error::DeviceNotFound(s)
            | Error::OpenFailed(s)
            | Error::Capture(s)
            | Error::InvalidWav(s)
            | Error::Session(s) => s.clone(),
            Error::NotImplemented(s) => (*s).to_string(),
            other => other.to_string(),
        }
    }
}
