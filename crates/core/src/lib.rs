//! Núcleo da app: bancos (`app.db` + um `library.db` por empresa/projeto), importação,
//! leitura, edição com histórico, busca e conversão de áudio. Sem dependência de UI.
pub mod app;
pub mod audio;
pub mod db;
pub mod error;
pub mod fsx;
pub mod glossary;
pub mod import;
pub mod library;
pub mod model;
pub mod parse;
pub mod paths;
pub mod player;
pub mod recording;
pub mod rules;
pub mod schema;
pub mod search;
pub mod storage;
pub mod text;
pub mod transcription;
pub mod transfer;

pub use app::App;
pub use error::{Error, Result};
pub use library::{ClientFilter, Library};
pub use model::Origin;
