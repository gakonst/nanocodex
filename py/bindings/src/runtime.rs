use std::sync::{Arc, OnceLock};

use pyo3::PyResult;
use tokio::runtime::Runtime;

use crate::error::runtime_error;

static RUNTIME: OnceLock<Result<Arc<Runtime>, String>> = OnceLock::new();
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Shares transport setup while each agent retains independent Tower and socket state.
pub(crate) fn shared_http_client() -> reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new).clone()
}

pub(crate) fn runtime() -> PyResult<Arc<Runtime>> {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                // Model and transport work is I/O-bound. Keep one small
                // process-wide executor instead of scaling threads with CPUs
                // or independently constructed Python agents.
                .worker_threads(2)
                .thread_name("nanocodex-python")
                .build()
                .map(Arc::new)
                .map_err(|error| error.to_string())
        })
        .clone()
        .map_err(runtime_error)
}
