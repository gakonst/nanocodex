use std::path::PathBuf;

use nanocodex_oai_api::auth::OpenAiAuth;

/// Connection inputs for the built-in `OpenAI` web-search tool.
pub struct WebSearchConfig {
    /// Complete search endpoint URL.
    pub endpoint: String,
    /// Authentication source shared with the `OpenAI` API.
    pub auth: OpenAiAuth,
}

/// Connection and persistence inputs for the built-in image-generation tool.
pub struct ImageGenerationConfig {
    /// Base `OpenAI` API URL; image endpoint paths are appended internally.
    pub api_base_url: String,
    /// Authentication source shared with the `OpenAI` API.
    pub auth: OpenAiAuth,
    /// Root under which generated images are retained by session and call.
    pub save_root: PathBuf,
}
