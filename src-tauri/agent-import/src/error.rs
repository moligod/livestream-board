use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Database(String),
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
impl AppError {
    pub fn io(path: impl AsRef<Path>, error: std::io::Error) -> Self {
        Self::Message(format!("{}: {error}", path.as_ref().display()))
    }
}
