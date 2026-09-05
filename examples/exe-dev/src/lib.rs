//! A deliberately small native Nanocodex consumer for one private exe.dev VM.

pub mod sandbox;

use std::{
    convert::Infallible,
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use eyre::{Result, WrapErr, bail};
use futures_util::{Stream, stream};
use nanocodex::{
    AgentEvents, Nanocodex, OpenAi,
    agent::{events::AgentEvent, session::SessionSnapshot},
    oai::transport::ResponsesTransport,
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::AsyncWriteExt,
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};

const DEFAULT_LISTEN: &str = "0.0.0.0:9998";
const DEFAULT_INSTRUCTIONS: &str = "You are the coding agent for this project. Inspect the workspace carefully, preserve unrelated work, and verify changes before finishing.";
const PROMPT_BODY_LIMIT: usize = 64 * 1024;
const PROMPT_QUEUE_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 1_024;

/// Process configuration owned by the exe.dev application consumer.
#[derive(Clone)]
pub struct Config {
    /// Address exposed through exe.dev's private alternate-port proxy.
    pub listen: SocketAddr,
    /// Project root affected by model-callable workspace tools.
    pub workspace: PathBuf,
    /// Durable completed-turn snapshot.
    pub state_file: PathBuf,
    /// Stable model instructions used for both fresh and resumed sessions.
    pub instructions: String,
    api_key: String,
    api_base: Option<String>,
    websocket_url: Option<String>,
    transport: ResponsesTransport,
}

impl Config {
    /// Reads the small deployment policy from environment variables.
    pub fn from_env() -> Result<Self> {
        let listen = env::var("NANOCODEX_EXE_LISTEN")
            .unwrap_or_else(|_| DEFAULT_LISTEN.to_owned())
            .parse()
            .wrap_err("NANOCODEX_EXE_LISTEN must be a socket address")?;
        let workspace = match env::var_os("NANOCODEX_EXE_WORKSPACE") {
            Some(path) => PathBuf::from(path),
            None => env::current_dir().wrap_err("failed to resolve the current workspace")?,
        };
        let workspace = workspace
            .canonicalize()
            .wrap_err_with(|| format!("failed to resolve workspace {}", workspace.display()))?;
        if !workspace.is_dir() {
            bail!("workspace is not a directory: {}", workspace.display());
        }
        let state_file = env::var_os("NANOCODEX_EXE_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join(".nanocodex/exe-dev/session.json"));
        let instructions = env::var("NANOCODEX_EXE_INSTRUCTIONS")
            .unwrap_or_else(|_| DEFAULT_INSTRUCTIONS.to_owned());
        let api_key = env::var("OPENAI_API_KEY")
            .wrap_err("OPENAI_API_KEY is required (the exe.dev image supplies a harmless integration placeholder)")?;
        if api_key.is_empty() {
            bail!("OPENAI_API_KEY must not be empty");
        }

        let transport = env::var("NANOCODEX_EXE_TRANSPORT")
            .unwrap_or_else(|_| "websocket".to_owned())
            .parse::<ResponsesTransport>()
            .map_err(|error| eyre::eyre!(error))?;
        let api_base = nonempty_env("NANOCODEX_EXE_API_BASE");
        let websocket_url = nonempty_env("NANOCODEX_EXE_WEBSOCKET_URL");
        validate_endpoint_policy(transport, api_base.as_deref(), websocket_url.as_deref())?;

        Ok(Self {
            listen,
            workspace,
            state_file,
            instructions,
            api_key,
            api_base,
            websocket_url,
            transport,
        })
    }

    #[cfg(test)]
    fn for_test(workspace: PathBuf) -> Self {
        Self {
            listen: DEFAULT_LISTEN.parse().expect("valid test address"),
            state_file: workspace.join("session.json"),
            workspace,
            instructions: DEFAULT_INSTRUCTIONS.to_owned(),
            api_key: "test-key".to_owned(),
            api_base: None,
            websocket_url: None,
            transport: ResponsesTransport::WebSocket,
        }
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn validate_endpoint_policy(
    transport: ResponsesTransport,
    api_base: Option<&str>,
    websocket_url: Option<&str>,
) -> Result<()> {
    if websocket_url.is_some() && api_base.is_none() {
        bail!("NANOCODEX_EXE_WEBSOCKET_URL requires NANOCODEX_EXE_API_BASE");
    }
    if matches!(transport, ResponsesTransport::WebSocket)
        && api_base.is_some()
        && websocket_url.is_none()
    {
        bail!(
            "WebSocket transport with NANOCODEX_EXE_API_BASE also requires NANOCODEX_EXE_WEBSOCKET_URL"
        );
    }
    Ok(())
}

/// Builds one retained native session, resuming its last completed boundary when present.
pub fn build_agent(config: &Config) -> Result<(Nanocodex, AgentEvents)> {
    let mut openai = OpenAi::builder(config.api_key.clone()).transport(config.transport);
    if let Some(api_base) = &config.api_base {
        openai = openai.api_base_url(api_base);
    }
    if let Some(websocket_url) = &config.websocket_url {
        openai = openai.websocket_url(websocket_url);
    }
    let openai = openai.build().wrap_err("failed to configure OpenAI")?;

    let mut builder = Nanocodex::builder(openai)
        .instructions(config.instructions.clone())
        .workspace(&config.workspace);
    if let Some(snapshot) = load_snapshot(&config.state_file)? {
        builder = builder.resume(snapshot);
    }
    builder
        .build()
        .wrap_err("failed to build Nanocodex session")
}

fn load_snapshot(path: &Path) -> Result<Option<SessionSnapshot>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).wrap_err_with(|| format!("failed to read {}", path.display()));
        }
    };
    serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("failed to decode {}", path.display()))
        .map(Some)
}

/// Running application tasks and the Nanocodex shutdown capability.
pub struct Runtime {
    agent: Nanocodex,
    prompt_task: JoinHandle<()>,
    event_task: JoinHandle<()>,
}

impl Runtime {
    /// Gracefully closes the model, tools, and event stream.
    pub async fn shutdown(self) -> Result<()> {
        let result = self.agent.shutdown().await;
        self.prompt_task.abort();
        self.event_task.abort();
        let _ = self.prompt_task.await;
        let _ = self.event_task.await;
        result.wrap_err("failed to shut down Nanocodex")
    }
}

#[derive(Clone)]
struct AppState {
    info: ServiceInfo,
    prompts: mpsc::Sender<PromptCommand>,
    events: broadcast::Sender<AgentEvent>,
}

#[derive(Clone, Serialize)]
struct ServiceInfo {
    status: &'static str,
    session_id: String,
    workspace: String,
}

struct PromptCommand {
    prompt: String,
    result: oneshot::Sender<std::result::Result<PromptResponse, String>>,
}

/// Successful completed-turn response.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PromptResponse {
    /// Complete final assistant message.
    pub final_message: String,
}

#[derive(Deserialize)]
struct PromptRequest {
    prompt: String,
}

/// Starts the application-owned prompt queue and event projection.
pub fn application(agent: Nanocodex, events: AgentEvents, config: &Config) -> (Router, Runtime) {
    let (prompt_tx, prompt_rx) = mpsc::channel(PROMPT_QUEUE_CAPACITY);
    let (event_tx, _) = broadcast::channel(EVENT_CAPACITY);
    let state = AppState {
        info: ServiceInfo {
            status: "ok",
            session_id: agent.session_id().to_string(),
            workspace: config.workspace.display().to_string(),
        },
        prompts: prompt_tx,
        events: event_tx.clone(),
    };
    let prompt_task = tokio::spawn(prompt_loop(
        agent.clone(),
        config.state_file.clone(),
        prompt_rx,
    ));
    let event_task = tokio::spawn(event_loop(events, event_tx));
    let router = router(state);
    (
        router,
        Runtime {
            agent,
            prompt_task,
            event_task,
        },
    )
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(health))
        .route("/api/prompt", post(prompt))
        .route("/api/events", get(event_stream))
        .layer(DefaultBodyLimit::max(PROMPT_BODY_LIMIT))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health(State(state): State<AppState>) -> Json<ServiceInfo> {
    Json(state.info)
}

async fn prompt(
    State(state): State<AppState>,
    Json(request): Json<PromptRequest>,
) -> std::result::Result<Json<PromptResponse>, ApiError> {
    if request.prompt.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "prompt must not be empty",
        ));
    }
    let (result, receiver) = oneshot::channel();
    state
        .prompts
        .send(PromptCommand {
            prompt: request.prompt,
            result,
        })
        .await
        .map_err(|_| ApiError::unavailable())?;
    receiver
        .await
        .map_err(|_| ApiError::unavailable())?
        .map(Json)
        .map_err(|message| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message))
}

async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let receiver = state.events.subscribe();
    let stream = stream::unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Ok(event) => {
                let data = serde_json::to_string(&event).unwrap_or_else(|error| {
                    serde_json::json!({"error": error.to_string()}).to_string()
                });
                let item = Event::default()
                    .event("agent")
                    .id(event.seq.to_string())
                    .data(data);
                Some((Ok(item), receiver))
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                let item = Event::default()
                    .event("lagged")
                    .data(format!(r#"{{"skipped":{skipped}}}"#));
                Some((Ok(item), receiver))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn prompt_loop(
    agent: Nanocodex,
    state_file: PathBuf,
    mut commands: mpsc::Receiver<PromptCommand>,
) {
    while let Some(command) = commands.recv().await {
        let result = run_prompt(&agent, &state_file, command.prompt).await;
        let result = result.map_err(|error| format!("{error:#}"));
        let _ = command.result.send(result);
    }
}

async fn run_prompt(
    agent: &Nanocodex,
    state_file: &Path,
    prompt: String,
) -> Result<PromptResponse> {
    let result = agent
        .prompt(prompt)
        .await
        .wrap_err("agent rejected the prompt")?
        .result()
        .await
        .wrap_err("agent turn failed")?;
    let snapshot = result
        .snapshot()
        .ok_or_else(|| eyre::eyre!("the local agent did not retain a snapshot"))?;
    persist_snapshot(state_file, &snapshot).await?;
    Ok(PromptResponse {
        final_message: result.into_final_message(),
    })
}

async fn persist_snapshot(path: &Path, snapshot: &SessionSnapshot) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("snapshot path has no parent: {}", path.display()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    let bytes = serde_json::to_vec(snapshot).wrap_err("failed to encode session snapshot")?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .await
        .wrap_err_with(|| format!("failed to open {}", temporary.display()))?;
    file.write_all(&bytes)
        .await
        .wrap_err_with(|| format!("failed to write {}", temporary.display()))?;
    file.sync_all()
        .await
        .wrap_err_with(|| format!("failed to sync {}", temporary.display()))?;
    drop(file);
    tokio::fs::rename(&temporary, path)
        .await
        .wrap_err_with(|| format!("failed to publish {}", path.display()))?;
    Ok(())
}

async fn event_loop(mut events: AgentEvents, sender: broadcast::Sender<AgentEvent>) {
    while let Some(event) = events.recv().await {
        let _ = sender.send(event);
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn unavailable() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "agent is unavailable")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: &self.message,
            }),
        )
            .into_response()
    }
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Nanocodex on exe.dev</title>
<style>
  :root { color-scheme: dark; font: 16px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
  body { max-width: 900px; margin: 0 auto; padding: 32px 20px; background: #0b0d10; color: #e7e9ec; }
  h1 { font: 600 28px/1.2 system-ui, sans-serif; }
  textarea, button { box-sizing: border-box; border: 1px solid #353a43; border-radius: 8px; background: #15181d; color: inherit; }
  textarea { width: 100%; min-height: 120px; padding: 12px; resize: vertical; }
  button { margin-top: 10px; padding: 9px 14px; cursor: pointer; }
  button:disabled { opacity: .5; cursor: wait; }
  pre { min-height: 180px; padding: 16px; border: 1px solid #272b32; border-radius: 8px; white-space: pre-wrap; background: #101216; }
  #status { color: #99a1ad; }
</style>
<h1>Nanocodex on exe.dev</h1>
<p id="status">Connecting to the private agent…</p>
<form id="prompt-form">
  <textarea id="prompt" placeholder="Ask Nanocodex to inspect or change this project"></textarea>
  <button id="submit" type="submit">Run</button>
</form>
<h2>Result</h2>
<pre id="result"></pre>
<h2>Events</h2>
<pre id="events"></pre>
<script>
const status = document.querySelector('#status');
const result = document.querySelector('#result');
const events = document.querySelector('#events');
const submit = document.querySelector('#submit');
fetch('/healthz').then(r => r.json()).then(info => {
  status.textContent = `session ${info.session_id} · ${info.workspace}`;
}).catch(error => { status.textContent = `health check failed: ${error}`; });
const source = new EventSource('/api/events');
source.addEventListener('agent', message => {
  const event = JSON.parse(message.data);
  events.textContent += `${event.seq} ${event.type}\n`;
  events.scrollTop = events.scrollHeight;
});
source.addEventListener('lagged', message => {
  events.textContent += `event stream lagged: ${message.data}\n`;
});
document.querySelector('#prompt-form').addEventListener('submit', async event => {
  event.preventDefault();
  const prompt = document.querySelector('#prompt').value;
  submit.disabled = true;
  result.textContent = 'Working…';
  try {
    const response = await fetch('/api/prompt', {
      method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({prompt})
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
    result.textContent = body.final_message;
  } catch (error) {
    result.textContent = String(error);
  } finally {
    submit.disabled = false;
  }
});
</script>
</html>
"#;

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> (AppState, mpsc::Receiver<PromptCommand>) {
        let (prompts, receiver) = mpsc::channel(1);
        let (events, _) = broadcast::channel(4);
        (
            AppState {
                info: ServiceInfo {
                    status: "ok",
                    session_id: "session-test".to_owned(),
                    workspace: "/workspace".to_owned(),
                },
                prompts,
                events,
            },
            receiver,
        )
    }

    #[tokio::test]
    async fn health_exposes_only_structural_session_information() {
        let (state, _receiver) = test_state();
        let response = router(state)
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "status": "ok",
                "session_id": "session-test",
                "workspace": "/workspace"
            })
        );
    }

    #[tokio::test]
    async fn prompt_endpoint_waits_for_the_owned_session_result() {
        let (state, mut receiver) = test_state();
        let responder = tokio::spawn(async move {
            let command = receiver.recv().await.unwrap();
            assert_eq!(command.prompt, "inspect the project");
            command
                .result
                .send(Ok(PromptResponse {
                    final_message: "done".to_owned(),
                }))
                .unwrap();
        });
        let response = router(state)
            .oneshot(
                Request::post("/api/prompt")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prompt":"inspect the project"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        responder.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"final_message": "done"})
        );
    }

    #[tokio::test]
    async fn empty_prompts_do_not_enter_the_agent_queue() {
        let (state, mut receiver) = test_state();
        let response = router(state)
            .oneshot(
                Request::post("/api/prompt")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prompt":"  "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn test_config_keeps_state_inside_the_selected_workspace() {
        let directory = tempdir().unwrap();
        let config = Config::for_test(directory.path().to_path_buf());
        assert_eq!(config.state_file, directory.path().join("session.json"));
    }

    #[test]
    fn custom_model_endpoints_are_atomic_policy() {
        assert!(validate_endpoint_policy(ResponsesTransport::WebSocket, None, None).is_ok());
        assert!(
            validate_endpoint_policy(
                ResponsesTransport::WebSocket,
                Some("https://llm.int.exe.xyz/v1"),
                Some("wss://llm.int.exe.xyz/v1/responses")
            )
            .is_ok()
        );
        assert!(
            validate_endpoint_policy(
                ResponsesTransport::WebSocket,
                Some("https://example.invalid/v1"),
                None
            )
            .is_err()
        );
        assert!(
            validate_endpoint_policy(
                ResponsesTransport::Https,
                Some("https://llm.int.exe.xyz/v1"),
                None
            )
            .is_ok()
        );
        assert!(
            validate_endpoint_policy(
                ResponsesTransport::Https,
                None,
                Some("wss://example.invalid/v1/responses")
            )
            .is_err()
        );
    }
}
