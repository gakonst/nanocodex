use serde::Serialize;

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: i32,
    pub message: String,
}
impl Error {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
    pub fn action(message: impl Into<String>) -> Self {
        Self::new(-10005, message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(-32601, message)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new(-32000, error.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::new(-32700, error.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;
