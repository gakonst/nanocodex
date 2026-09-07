//! Single-threaded bridge to the agent host's context files.
use std::{future::Future, sync::Arc};

use js_sys::Promise;
use nanocodex::tools::context_management::{BackendFuture, HistoryNotesHost, StorageOperation};
use serde_json::Value;
use tokio::sync::oneshot;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = historyNotesCapability)]
    fn host_capability(thread_id: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = historyNotesRequest)]
    fn host_request(thread_id: &str, request: &str) -> Result<Promise, JsValue>;
}

pub(super) fn host() -> Arc<dyn HistoryNotesHost> {
    Arc::new(JavaScriptHistoryNotes)
}

struct JavaScriptHistoryNotes;

impl HistoryNotesHost for JavaScriptHistoryNotes {
    fn available(&self, thread_id: String) -> BackendFuture<Result<bool, String>> {
        spawn(
            async move {
                let promise = host_capability(&thread_id).map_err(js_error)?;
                JsFuture::from(promise)
                    .await
                    .map_err(js_error)?
                    .as_bool()
                    .ok_or_else(|| "Context storage returned invalid availability".into())
            },
            Err("Context storage host stopped".into()),
            |result| result,
        )
    }

    fn access(
        &self,
        thread_id: String,
        operation: StorageOperation,
    ) -> BackendFuture<Result<Value, String>> {
        spawn(
            async move {
                let request =
                    serde_json::to_string(&operation).map_err(|error| error.to_string())?;
                let promise = host_request(&thread_id, &request).map_err(js_error)?;
                let response = JsFuture::from(promise).await.map_err(js_error)?;
                serde_json::from_str(
                    &response
                        .as_string()
                        .ok_or("Context storage returned invalid JSON")?,
                )
                .map_err(|error| error.to_string())
            },
            Err("Context storage host stopped".into()),
            |result| result,
        )
    }
}

fn js_error(error: JsValue) -> String {
    js_sys::Reflect::get(&error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "Context storage failed".into())
}

// Only owned Rust values cross the Send boundary. JavaScript futures remain on
// their originating event loop; cancellation never retries a dropped operation.
fn spawn<T: Send + 'static, U: Send + 'static>(
    future: impl Future<Output = T> + 'static,
    closed: T,
    map: impl FnOnce(T) -> U + Send + 'static,
) -> BackendFuture<U> {
    let (mut sender, receiver) = oneshot::channel();
    spawn_local(async move {
        let result = tokio::select! {
            biased;
            () = sender.closed() => return,
            result = future => result,
        };
        let _ = sender.send(result);
    });
    Box::pin(async move { map(receiver.await.unwrap_or(closed)) })
}
