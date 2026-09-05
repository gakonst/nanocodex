#![allow(missing_docs)]

mod oauth;
mod support;
mod tool_macro;
mod tracing;

// Keep ToolRuntime calls from racing the temporary tracing subscribers in this
// consolidated integration-test binary without paying for another linked binary.
pub(crate) static TOOL_RUNTIME_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

const fn main() {}
