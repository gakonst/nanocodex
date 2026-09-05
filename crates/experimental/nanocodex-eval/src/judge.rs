//! Evaluator-owned OpenAI judge service for isolated benchmark verifiers.

use std::{
    collections::BTreeMap,
    io::Read as _,
    net::Ipv4Addr,
    str::FromStr as _,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header::CONTENT_ENCODING},
    response::{IntoResponse as _, Response},
    routing::{get, post},
};
use flate2::read::GzDecoder;
use nanocodex_agent::NanocodexBuilder;
use nanocodex_oai_api::Model;
use nanocodex_tools::Tools;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{Mutex as AsyncMutex, oneshot},
    task::{AbortHandle, JoinHandle},
};
use uuid::Uuid;

const GUEST_HOST: Ipv4Addr = Ipv4Addr::new(192, 168, 127, 254);
const MAX_JUDGE_REQUEST_BYTES: usize = 2 * 1024 * 1024;

/// A run-scoped judge endpoint backed by the evaluator's selected OpenAI auth.
pub struct JudgeRuntime {
    port: u16,
    token: Arc<str>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    workers: Arc<Mutex<Vec<AbortHandle>>>,
}

#[derive(Clone)]
struct JudgeState {
    builder: NanocodexBuilder,
    token: Arc<str>,
    jobs: Arc<AsyncMutex<BTreeMap<String, JudgeJob>>>,
    workers: Arc<Mutex<Vec<AbortHandle>>>,
}

#[derive(Debug, Deserialize)]
struct JudgeRequest {
    model: String,
    input: Value,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Value,
}

#[derive(Clone)]
struct JudgeAnswer {
    model: Model,
    message: String,
}

#[derive(Clone)]
struct JudgeFailure {
    status: StatusCode,
    message: String,
}

enum JudgeJob {
    Pending,
    Complete(Result<JudgeAnswer, JudgeFailure>),
}

#[derive(Debug, Serialize)]
struct JudgeError {
    error: JudgeErrorBody,
}

#[derive(Debug, Serialize)]
struct JudgeErrorBody {
    message: String,
}

/// Judge service startup failed.
#[derive(Debug, thiserror::Error)]
pub enum JudgeRuntimeError {
    /// The loopback listener could not be created or inspected.
    #[error("judge runtime listener failed: {0}")]
    Listener(#[from] std::io::Error),
    /// The deliberately empty verifier tool registry could not be built.
    #[error("judge runtime tool policy failed: {0}")]
    Tools(#[from] nanocodex_tools::ToolsBuildError),
}

impl JudgeRuntime {
    /// Starts a no-tools judge service on host loopback.
    pub async fn start(builder: NanocodexBuilder) -> Result<Self, JudgeRuntimeError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let token: Arc<str> = Uuid::now_v7().simple().to_string().into();
        let workers = Arc::new(Mutex::new(Vec::new()));
        let state = JudgeState {
            builder: builder.tools(Tools::builder().without_defaults().build()?),
            token: Arc::clone(&token),
            jobs: Arc::new(AsyncMutex::new(BTreeMap::new())),
            workers: Arc::clone(&workers),
        };
        let application = Router::new()
            .route("/v1/responses", post(Self::respond))
            .route("/v1/responses/async", post(Self::submit))
            .route("/v1/responses/async/{id}", get(Self::poll))
            .route("/v1/chat/completions", post(Self::chat_completion))
            .with_state(state);
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = axum::serve(listener, application)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await;
            if let Err(error) = result {
                tracing::error!(%error, "judge runtime stopped unexpectedly");
            }
        });
        Ok(Self {
            port,
            token,
            shutdown: Some(shutdown),
            task,
            workers,
        })
    }

    /// Returns verifier-only values that route through the guest host gateway.
    #[must_use]
    pub fn verifier_environment(&self) -> BTreeMap<String, String> {
        let base_url = format!("http://{GUEST_HOST}:{}/v1", self.port);
        BTreeMap::from([
            ("NANOCODEX_JUDGE_BASE_URL".to_owned(), base_url.clone()),
            ("NANOCODEX_JUDGE_TOKEN".to_owned(), self.token.to_string()),
            ("EVAL_BASE_URL".to_owned(), base_url.clone()),
            ("EVAL_API_KEY".to_owned(), self.token.to_string()),
            ("EVAL_MODEL".to_owned(), "gpt-5.6-sol".to_owned()),
            ("OPENAI_BASE_URL".to_owned(), base_url.clone()),
            ("OPENAI_API_BASE".to_owned(), base_url),
            ("OPENAI_API_KEY".to_owned(), self.token.to_string()),
        ])
    }

    async fn chat_completion(
        State(state): State<JudgeState>,
        headers: HeaderMap,
        Json(request): Json<ChatCompletionRequest>,
    ) -> Response {
        if !state.authorized(&headers) {
            return Self::error(StatusCode::UNAUTHORIZED, "invalid judge token");
        }
        let answer = match state.answer(request.model, request.messages).await {
            Ok(answer) => answer,
            Err(error) => return Self::error(error.status, error.message),
        };
        Self::chat_answer(format!("chatcmpl_{}", Uuid::now_v7().simple()), answer)
    }

    async fn respond(
        State(state): State<JudgeState>,
        headers: HeaderMap,
        Json(request): Json<JudgeRequest>,
    ) -> Response {
        if !state.authorized(&headers) {
            return Self::error(StatusCode::UNAUTHORIZED, "invalid judge token");
        }
        let answer = match state.answer(request.model, request.input).await {
            Ok(answer) => answer,
            Err(error) => return Self::error(error.status, error.message),
        };
        Self::answer(format!("judge_{}", Uuid::now_v7().simple()), answer)
    }

    async fn submit(State(state): State<JudgeState>, headers: HeaderMap, body: Bytes) -> Response {
        if !state.authorized(&headers) {
            return Self::error(StatusCode::UNAUTHORIZED, "invalid judge token");
        }
        let request = match Self::decode_request(&headers, &body) {
            Ok(request) => request,
            Err(error) => return error,
        };
        let id = format!("judge_{}", Uuid::now_v7().simple());
        state
            .jobs
            .lock()
            .await
            .insert(id.clone(), JudgeJob::Pending);
        let worker_state = state.clone();
        let worker_id = id.clone();
        let worker = tokio::spawn(async move {
            let result = worker_state.answer(request.model, request.input).await;
            if let Err(error) = &result {
                tracing::warn!(
                    judge_job = %worker_id,
                    status = error.status.as_u16(),
                    message = %error.message,
                    "asynchronous judge job failed"
                );
            }
            worker_state
                .jobs
                .lock()
                .await
                .insert(worker_id, JudgeJob::Complete(result));
        });
        let registered = if let Ok(mut workers) = state.workers.lock() {
            workers.push(worker.abort_handle());
            true
        } else {
            false
        };
        if !registered {
            worker.abort();
            state.jobs.lock().await.remove(&id);
            return Self::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "judge worker registry is unavailable",
            );
        }
        (
            StatusCode::ACCEPTED,
            Json(json!({"id": id, "object": "response", "status": "in_progress"})),
        )
            .into_response()
    }

    async fn poll(
        State(state): State<JudgeState>,
        Path(id): Path<String>,
        headers: HeaderMap,
    ) -> Response {
        if !state.authorized(&headers) {
            return Self::error(StatusCode::UNAUTHORIZED, "invalid judge token");
        }
        let jobs = state.jobs.lock().await;
        match jobs.get(&id) {
            Some(JudgeJob::Pending) => Json(json!({
                "id": id,
                "object": "response",
                "status": "in_progress"
            }))
            .into_response(),
            Some(JudgeJob::Complete(Ok(answer))) => Self::answer(id, answer.clone()),
            Some(JudgeJob::Complete(Err(error))) => {
                Self::error(error.status, error.message.clone())
            }
            None => Self::error(StatusCode::NOT_FOUND, "unknown judge job"),
        }
    }

    fn answer(id: String, answer: JudgeAnswer) -> Response {
        Json(json!({
            "id": id,
            "object": "response",
            "status": "completed",
            "model": answer.model,
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": answer.message}]
            }],
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "total_tokens": 0
            }
        }))
        .into_response()
    }

    fn chat_answer(id: String, answer: JudgeAnswer) -> Response {
        Json(json!({
            "id": id,
            "object": "chat.completion",
            "created": 0,
            "model": answer.model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": answer.message},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0
            }
        }))
        .into_response()
    }

    fn prompt(input: Value) -> Result<(Option<String>, String), String> {
        match input {
            Value::String(prompt) if !prompt.trim().is_empty() => Ok((None, prompt)),
            Value::Array(messages) => {
                let mut instructions = Vec::new();
                let mut prompt = Vec::new();
                for message in messages {
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "judge input message has no role".to_owned())?;
                    let content = message
                        .get("content")
                        .ok_or_else(|| "judge input message has no content".to_owned())?;
                    let content = Self::text_content(content)?;
                    if role == "system" || role == "developer" {
                        instructions.push(content);
                    } else {
                        prompt.push(format!("{role}:\n{content}"));
                    }
                }
                if prompt.is_empty() {
                    return Err("judge input contains no user prompt".to_owned());
                }
                Ok((
                    (!instructions.is_empty()).then(|| instructions.join("\n\n")),
                    prompt.join("\n\n"),
                ))
            }
            _ => Err("judge input must be text or an array of text messages".to_owned()),
        }
    }

    fn text_content(content: &Value) -> Result<String, String> {
        if let Some(content) = content.as_str() {
            return Ok(content.to_owned());
        }
        let parts = content
            .as_array()
            .ok_or_else(|| "judge input message content must be text".to_owned())?;
        let text = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return Err("judge input message contains no text".to_owned());
        }
        Ok(text)
    }

    fn error(status: StatusCode, message: impl Into<String>) -> Response {
        (
            status,
            Json(JudgeError {
                error: JudgeErrorBody {
                    message: message.into(),
                },
            }),
        )
            .into_response()
    }

    fn decode_request(headers: &HeaderMap, body: &[u8]) -> Result<JudgeRequest, Response> {
        if body.len() > MAX_JUDGE_REQUEST_BYTES {
            return Err(Self::error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "judge request exceeds 2 MiB",
            ));
        }
        let decoded = match headers
            .get(CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
        {
            None => body.to_vec(),
            Some("gzip") => {
                let mut decoded = Vec::new();
                let limit = u64::try_from(MAX_JUDGE_REQUEST_BYTES)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1);
                if let Err(error) = GzDecoder::new(body).take(limit).read_to_end(&mut decoded) {
                    return Err(Self::error(
                        StatusCode::BAD_REQUEST,
                        format!("invalid gzip judge request: {error}"),
                    ));
                }
                if decoded.len() > MAX_JUDGE_REQUEST_BYTES {
                    return Err(Self::error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "decompressed judge request exceeds 2 MiB",
                    ));
                }
                decoded
            }
            Some(encoding) => {
                return Err(Self::error(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    format!("unsupported judge content encoding {encoding:?}"),
                ));
            }
        };
        serde_json::from_slice(&decoded).map_err(|error| {
            Self::error(
                StatusCode::BAD_REQUEST,
                format!("invalid judge request JSON: {error}"),
            )
        })
    }
}

impl JudgeState {
    async fn answer(
        &self,
        requested_model: String,
        input: Value,
    ) -> Result<JudgeAnswer, JudgeFailure> {
        let model = Model::from_str(&requested_model).map_err(|message| JudgeFailure {
            status: StatusCode::BAD_REQUEST,
            message,
        })?;
        let (instructions, prompt) =
            JudgeRuntime::prompt(input).map_err(|message| JudgeFailure {
                status: StatusCode::BAD_REQUEST,
                message,
            })?;
        let mut builder = self.builder.clone().model(model);
        if let Some(instructions) = instructions {
            builder = builder.instructions(instructions);
        }
        let (agent, events) = match builder.build() {
            Ok(agent) => agent,
            Err(error) => {
                return Err(JudgeFailure {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: format!("judge agent build failed: {error}"),
                });
            }
        };
        drop(events);
        let result = match agent.prompt(prompt).await {
            Ok(turn) => turn.result().await,
            Err(error) => Err(error),
        };
        let _ = agent.shutdown().await;
        let message = match result {
            Ok(result) => result.into_final_message(),
            Err(error) => {
                return Err(JudgeFailure {
                    status: StatusCode::BAD_GATEWAY,
                    message: format!("OpenAI judge failed: {error}"),
                });
            }
        };
        Ok(JudgeAnswer { model, message })
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            == Some(self.token.as_ref())
    }
}

impl Drop for JudgeRuntime {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Ok(workers) = self.workers.lock() {
            for worker in workers.iter() {
                worker.abort();
            }
        }
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::to_bytes,
        http::{HeaderValue, header::AUTHORIZATION},
    };
    use flate2::{Compression, write::GzEncoder};
    use nanocodex_agent::{Nanocodex, OpenAi};
    use std::io::Write as _;

    use super::*;

    #[test]
    fn prompt_accepts_responses_text_shapes() {
        let (instructions, prompt) = JudgeRuntime::prompt(json!([
            {"role": "system", "content": "Grade precisely."},
            {
                "role": "user",
                "content": [{"type": "input_text", "text": "candidate answer"}]
            }
        ]))
        .unwrap();

        assert_eq!(instructions.as_deref(), Some("Grade precisely."));
        assert_eq!(prompt, "user:\ncandidate answer");
    }

    #[tokio::test]
    async fn verifier_environment_exposes_responses_and_openai_compatible_judges() {
        let (shutdown, _receiver) = oneshot::channel();
        let runtime = JudgeRuntime {
            port: 43123,
            token: Arc::from("judge-token"),
            shutdown: Some(shutdown),
            task: tokio::spawn(std::future::pending()),
            workers: Arc::new(Mutex::new(Vec::new())),
        };

        let environment = runtime.verifier_environment();

        assert_eq!(
            environment
                .get("NANOCODEX_JUDGE_BASE_URL")
                .map(String::as_str),
            Some("http://192.168.127.254:43123/v1")
        );
        assert_eq!(
            environment["EVAL_BASE_URL"],
            "http://192.168.127.254:43123/v1"
        );
        assert_eq!(environment["EVAL_API_KEY"], "judge-token");
        assert_eq!(environment["EVAL_MODEL"], "gpt-5.6-sol");
        assert_eq!(environment["OPENAI_API_KEY"], "judge-token");
        assert_eq!(
            environment["OPENAI_BASE_URL"],
            "http://192.168.127.254:43123/v1"
        );
        assert_eq!(environment.len(), 8);
    }

    #[tokio::test]
    async fn chat_completion_answer_uses_openai_compatible_shape() {
        let response = JudgeRuntime::chat_answer(
            "chatcmpl_test".to_owned(),
            JudgeAnswer {
                model: Model::from_str("gpt-5.6-sol").unwrap(),
                message: "judge result".to_owned(),
            },
        );

        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let document: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(document["object"], "chat.completion");
        assert_eq!(document["choices"][0]["message"]["content"], "judge result");
    }

    #[tokio::test]
    async fn completed_async_answer_remains_available_after_transport_retries() {
        let jobs = Arc::new(AsyncMutex::new(BTreeMap::from([(
            "judge-test".to_owned(),
            JudgeJob::Complete(Ok(JudgeAnswer {
                model: Model::from_str("gpt-5.6-sol").unwrap(),
                message: "stable answer".to_owned(),
            })),
        )])));
        let state = JudgeState {
            builder: Nanocodex::builder(OpenAi::new("test-key").unwrap()),
            token: Arc::from("judge-token"),
            jobs,
            workers: Arc::new(Mutex::new(Vec::new())),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer judge-token"),
        );

        for _ in 0..2 {
            let response = JudgeRuntime::poll(
                State(state.clone()),
                Path("judge-test".to_owned()),
                headers.clone(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
            let document: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(document["status"], "completed");
            assert_eq!(document["output"][0]["content"][0]["text"], "stable answer");
        }
    }

    #[test]
    fn async_request_accepts_bounded_gzip_payloads() {
        let body = br#"{"model":"gpt-5.6-sol","input":"grade this"}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(body).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));

        let request = JudgeRuntime::decode_request(&headers, &compressed).unwrap();

        assert_eq!(request.model, "gpt-5.6-sol");
        assert_eq!(request.input, "grade this");
    }
}
