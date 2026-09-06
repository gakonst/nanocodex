// Adapted from openai/codex ac192cd793, ext/history-notes/src/backend.rs.
// Copyright OpenAI. Licensed under Apache-2.0.
use std::time::Duration;

use nanocodex_oai_api::auth::OpenAiAuth;
use serde_json::{Value, json};

#[derive(Clone)]
pub(super) struct Backend {
    pub(super) client: reqwest::Client,
    pub(super) auth: OpenAiAuth,
    pub(super) base_url: String,
    pub(super) session_id: String,
    pub(super) agent_name: String,
}

impl Backend {
    pub(super) async fn call(
        &self,
        path: &str,
        mut arguments: Value,
        budget: Value,
    ) -> Result<Value, String> {
        let object = arguments
            .as_object_mut()
            .ok_or("History tool arguments must be a JSON object")?;
        object.insert(
            "context".into(),
            json!({"session_id": self.session_id, "current_agent_name": self.agent_name}),
        );
        let mut auth = self.auth.snapshot().await.map_err(
            |_| "Unable to perform operation: Could not resolve backend authentication.",
        )?;
        for attempt in 0..2 {
            let mut request = self
                .client
                .post(format!("{}/{}", self.base_url.trim_end_matches('/'), path))
                .bearer_auth(auth.bearer())
                .header("originator", "nanocodex")
                .header(
                    "user-agent",
                    concat!("nanocodex/", env!("CARGO_PKG_VERSION")),
                )
                .header("x-openai-tool-output-truncation-policy", budget.to_string())
                .timeout(Duration::from_secs(35))
                .json(&arguments);
            if let Some(account) = auth.account_id() {
                request = request.header("ChatGPT-Account-ID", account);
            }
            if auth.is_fedramp() {
                request = request.header("X-OpenAI-Fedramp", "true");
            }
            if matches!(
                path,
                "alpha/history/v2/search_contents"
                    | "alpha/notes/v2/search_contents"
                    | "alpha/notes/v2/append_to_file"
                    | "alpha/notes/v2/write_file"
            ) {
                request = request.header("x-openai-encrypted-tool-arguments", "true");
            }
            let response = request
                .send()
                .await
                .map_err(|_| "Unable to perform operation: The backend request failed.")?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                self.auth.recover_unauthorized(&auth).await.map_err(
                    |_| "Unable to perform operation: Could not refresh backend authentication.",
                )?;
                auth = self.auth.snapshot().await.map_err(
                    |_| "Unable to perform operation: Could not resolve backend authentication.",
                )?;
                continue;
            }
            if !response.status().is_success() {
                return Err("Unable to perform operation: The backend request failed.".into());
            }
            return response.json().await.map_err(|_| {
                "Unable to perform operation: The backend returned invalid JSON.".into()
            });
        }
        unreachable!("final attempt returns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn notes_requests_bind_context_and_preserve_encrypted_output() {
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() < end + 4 + length {
                        continue;
                    }
                    assert!(headers.starts_with("post /alpha/notes/v2/write_file "));
                    assert!(headers.contains("authorization: bearer test-token"));
                    assert!(headers.contains("originator: nanocodex"));
                    assert!(headers.contains("x-openai-encrypted-tool-arguments: true"));
                    assert!(headers.contains("x-openai-tool-output-truncation-policy: {\"limit\":10000,\"mode\":\"tokens\"}"));
                    let body: Value = serde_json::from_slice(&request[end + 4..]).unwrap();
                    assert_eq!(
                        body,
                        json!({"path":"progress", "content":"checkpoint", "context":{"session_id":"session", "current_agent_name":"/root"}})
                    );
                    let response = r#"{"encrypted_output":"opaque"}"#;
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
                    break;
                }
            }
        });
        let backend = Backend {
            client: reqwest::Client::new(),
            auth: OpenAiAuth::api_key("test-token"),
            base_url,
            session_id: "session".into(),
            agent_name: "/root".into(),
        };
        let response = backend.call("alpha/notes/v2/write_file", json!({"path":"progress", "content":"checkpoint", "context":{"session_id":"spoof"}}), json!({"mode":"tokens","limit":10000})).await.unwrap();
        assert_eq!(response, json!({"encrypted_output":"opaque"}));
        server.await.unwrap();
    }
}
