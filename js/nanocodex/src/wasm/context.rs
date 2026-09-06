//! Single-threaded host bridge for authenticated history/notes operations.
use std::{future::Future, sync::Arc};

use js_sys::Promise;
use nanocodex::{
    Model,
    oai::auth::{OpenAiAuth, OpenAiAuthMode},
    tools::context_management::{
        BackendFuture, ContextManagement, HistoryNotesHost, HistoryNotesRequest,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = historyNotesCapability)]
    fn host_capability(thread_id: &str, base_url: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = historyNotesRequest)]
    fn host_request(
        thread_id: &str,
        request: &str,
        bearer: &str,
        account: Option<&str>,
        fedramp: bool,
    ) -> Result<Promise, JsValue>;
}

pub(super) fn host() -> Arc<dyn HistoryNotesHost> {
    Arc::new(JavaScriptHistoryNotes)
}

struct JavaScriptHistoryNotes;

impl HistoryNotesHost for JavaScriptHistoryNotes {
    fn eligible(
        &self,
        auth: OpenAiAuth,
        base_url: String,
        thread_id: String,
    ) -> BackendFuture<bool> {
        spawn(
            async move {
                let promise = host_capability(&thread_id, &base_url).ok()?;
                let capability = JsFuture::from(promise).await.ok()?.as_string()?;
                Some(match capability.as_str() {
                    "direct" => ContextManagement::eligible(Model::Astra, &auth, &base_url).await,
                    // The private broker has already resolved the subscription gate.
                    "host_managed" => {
                        auth.mode() == OpenAiAuthMode::ApiKey
                            && auth.snapshot().await.ok()?.bearer() == "host-managed"
                    }
                    _ => false,
                })
            },
            None,
            |eligible| eligible.unwrap_or(false),
        )
    }

    fn call(
        &self,
        auth: OpenAiAuth,
        request: HistoryNotesRequest,
    ) -> BackendFuture<Result<Value, String>> {
        spawn(
            async move { perform(auth, request).await },
            Err("Authenticated history/notes host stopped.".into()),
            |result| result,
        )
    }
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

#[derive(Deserialize)]
struct BackendResponse {
    status: u16,
    body: Value,
}

async fn perform(auth: OpenAiAuth, request: HistoryNotesRequest) -> Result<Value, String> {
    let encoded = json!({
        "baseUrl": request.base_url,
        "path": request.path,
        "body": request.arguments,
        "budget": request.budget,
    })
    .to_string();
    let mut snapshot = auth
        .snapshot()
        .await
        .map_err(|_| "Could not resolve backend authentication.")?;
    for attempt in 0..2 {
        let promise = host_request(
            &request.thread_id,
            &encoded,
            snapshot.bearer(),
            snapshot.account_id(),
            snapshot.is_fedramp(),
        )
        .map_err(|_| "The authenticated backend request failed.")?;
        let response = JsFuture::from(promise)
            .await
            .map_err(|_| "The authenticated backend request failed.")?;
        let response: BackendResponse = response
            .as_string()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or("The backend returned an invalid response.")?;
        if response.status == 401 && attempt == 0 {
            auth.recover_unauthorized(&snapshot)
                .await
                .map_err(|_| "Could not refresh backend authentication.")?;
            snapshot = auth
                .snapshot()
                .await
                .map_err(|_| "Could not resolve backend authentication.")?;
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Err("The authenticated backend request failed.".into());
        }
        return Ok(response.body);
    }
    unreachable!("final attempt returns")
}
