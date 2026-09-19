//! Private file-store backend for the pinned Codex memories API.
#![allow(missing_docs)]
mod backend;
mod local;
pub use backend::*;
pub use local::LocalMemoriesBackend;
pub const DEFAULT_READ_MAX_TOKENS: usize = 20_000;
pub const MAX_LIST_RESULTS: usize = 2_000;
pub const MAX_SEARCH_RESULTS: usize = 200;
