#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod wasm;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use wasm::{
    WasmBrowserVoice, WasmClaudeGrepRegex, WasmClaudeSubscription, WasmNanoclaude, WasmNanocodex,
    WasmTurn, WasmTurnResult,
};
