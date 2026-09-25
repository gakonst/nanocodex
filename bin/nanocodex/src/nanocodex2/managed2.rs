//! Opt-in text terminal for Managed2's deliberately smaller API. The legacy TUI
//! uses endpoints (and an account key) that this service does not implement.
use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

use futures_util::StreamExt;
use nanocodex_managed::ManagedError;
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest as _, http::header::AUTHORIZATION},
};
use url::Url;
use uuid::Uuid;

const DEFAULT_ORIGIN: &str = "https://nanocodex-managed2.gakonst.workers.dev";

#[derive(Deserialize)]
struct Created {
    agent_id: String,
    turn_id: String,
}
#[derive(Deserialize)]
struct Accepted {
    turn_id: String,
}
#[derive(Deserialize)]
pub(super) struct TurnStatus {
    pub(super) state: String,
    pub(super) message: Option<String>,
    pub(super) error: Option<String>,
}

/// Text-only Managed2 stream updates. Completed/Failed are authoritative turn
/// status observations; deltas are provisional and may be replayed on reconnect.
pub(super) enum WatchEvent {
    /// Full correlated agent event; transport leaves its protocol fields intact.
    AgentEvent(serde_json::Value),
    Delta(String),
    Completed(String),
    Failed(String),
}
#[derive(Deserialize)]
struct Frame {
    cursor: String,
    event: Option<serde_json::Value>,
}

#[derive(Clone)]
pub(super) struct Client {
    origin: Url,
    key: String,
    http: reqwest::Client,
}

fn error(message: impl Into<String>) -> ManagedError {
    ManagedError::Configuration(message.into())
}

impl Client {
    pub(super) fn from_environment() -> Result<Self, ManagedError> {
        let origin =
            std::env::var("NANOCODEX_MANAGED2_URL").unwrap_or_else(|_| DEFAULT_ORIGIN.to_owned());
        let origin = Url::parse(&origin).map_err(|_| error("invalid Managed2 URL"))?;
        let loopback = matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(origin.scheme() == "https" || origin.scheme() == "http" && loopback)
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
        {
            return Err(error(
                "Managed2 URL must be an HTTPS origin (loopback HTTP for development)",
            ));
        }
        let key = match std::env::var("NANOCODEX_MANAGED2_API_KEY") {
            Ok(key) => key,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(error("invalid Managed2 credential"));
            }
            Err(std::env::VarError::NotPresent) => {
                let home = std::env::var_os("HOME")
                    .ok_or_else(|| error("HOME is unset; set NANOCODEX_MANAGED2_API_KEY"))?;
                let path = PathBuf::from(home).join(".config/nanocodex/managed2-api-key");
                std::fs::read_to_string(path).map_err(|_| error("Managed2 key unavailable; set NANOCODEX_MANAGED2_API_KEY or ~/.config/nanocodex/managed2-api-key"))?.trim().to_owned()
            }
        };
        let Some(secret) = key.strip_prefix("ncx2_") else {
            return Err(error(
                "Managed2 requires a dedicated ncx2 key, not the account login key",
            ));
        };
        if secret.len() != 43
            || !secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(error("invalid Managed2 credential"));
        }
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(ManagedError::Transport)?;
        Ok(Self { origin, key, http })
    }

    fn url(&self, path: &str) -> Result<Url, ManagedError> {
        self.origin
            .join(path)
            .map_err(|_| error("invalid Managed2 route"))
    }

    pub(super) async fn submit(
        &self,
        agent: Option<&str>,
        input: &str,
        request_id: Uuid,
    ) -> Result<(String, String), ManagedError> {
        let path = agent.map_or_else(
            || "v1/agents".to_owned(),
            |id| format!("v1/agents/{id}/turns"),
        );
        let response = self
            .http
            .post(self.url(&path)?)
            .bearer_auth(&self.key)
            .header("idempotency-key", request_id.to_string())
            .json(&serde_json::json!({ "input": input }))
            .send()
            .await
            .map_err(ManagedError::Transport)?;
        if response.status() != reqwest::StatusCode::ACCEPTED {
            return Err(error(format!(
                "Managed2 admission HTTP {} (request ID {request_id}; do not retry with another ID if delivery is uncertain)",
                response.status()
            )));
        }
        if let Some(agent) = agent {
            let receipt: Accepted = response.json().await.map_err(ManagedError::Transport)?;
            Ok((agent.to_owned(), receipt.turn_id))
        } else {
            let receipt: Created = response.json().await.map_err(ManagedError::Transport)?;
            Ok((receipt.agent_id, receipt.turn_id))
        }
    }

    pub(super) async fn status(&self, agent: &str, turn: &str) -> Result<TurnStatus, ManagedError> {
        let response = self
            .http
            .get(self.url(&format!("v1/agents/{agent}/turns/{turn}"))?)
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(ManagedError::Transport)?;
        if !response.status().is_success() {
            return Err(error(format!(
                "Managed2 turn status HTTP {}",
                response.status()
            )));
        }
        response.json().await.map_err(ManagedError::Transport)
    }

    /// Watch one admitted external turn. The WebSocket cursor is advanced before
    /// emitting updates, so a reconnect resumes from the last observed frame.
    /// The durable turn status, rather than a stream terminal, ends the watch.
    pub(super) async fn watch_turn(
        &self,
        agent: &str,
        turn: &str,
        cursor: &mut String,
        updates: mpsc::UnboundedSender<WatchEvent>,
    ) -> Result<(), ManagedError> {
        let mut active_turn: Option<String> = None;
        loop {
            let mut endpoint = self.url(&format!("v1/agents/{agent}/events"))?;
            endpoint.query_pairs_mut().append_pair("cursor", cursor);
            endpoint
                .set_scheme(if endpoint.scheme() == "https" {
                    "wss"
                } else {
                    "ws"
                })
                .map_err(|()| error("invalid Managed2 WebSocket URL"))?;
            let mut request = endpoint
                .as_str()
                .into_client_request()
                .map_err(|_| error("invalid Managed2 WebSocket request"))?;
            let mut authorization = format!("Bearer {}", self.key)
                .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
                .map_err(|_| error("invalid Managed2 credential header"))?;
            authorization.set_sensitive(true);
            request.headers_mut().insert(AUTHORIZATION, authorization);
            let (mut socket, _) =
                tokio::time::timeout(Duration::from_secs(15), connect_async(request))
                    .await
                    .map_err(|_| error("Managed2 events connection timed out"))?
                    .map_err(|e| error(format!("Managed2 events connection failed: {e}")))?;
            let mut poll = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(1),
                Duration::from_secs(1),
            );
            loop {
                tokio::select! {
                    _ = poll.tick() => {
                        let state = self.status(agent, turn).await?;
                        if state.state == "completed" {
                            let _ = updates.send(WatchEvent::Completed(state.message.unwrap_or_default()));
                            return Ok(());
                        }
                        if state.state == "failed" {
                            let _ = updates.send(WatchEvent::Failed(state.error.unwrap_or_default()));
                            return Ok(());
                        }
                    }
                    frame = socket.next() => match frame {
                        Some(Ok(Message::Text(text))) => {
                            let frame: Frame = serde_json::from_str(&text).map_err(|_| error("invalid Managed2 event frame"))?;
                            *cursor = frame.cursor;
                            let Some(event) = frame.event else { break; }; // replay_paused fence
                            let payload = &event["payload"];
                            if event["type"] == "input.accepted" && payload["request_id"].as_str() == Some(turn) {
                                active_turn = payload["turn_id"].as_str().map(str::to_owned);
                            }
                            let correlated = active_turn.as_deref() == payload["turn_id"].as_str()
                                && active_turn.is_some() && event["type"] != "input.accepted";
                            if correlated && updates.send(WatchEvent::AgentEvent(event.clone())).is_err() {
                                return Ok(()); // The TUI was closed; do not keep a socket alive.
                            }
                            if correlated && event["type"] == "assistant.delta" && payload["phase"] == "final_answer"
                                && let Some(text) = payload["text"].as_str()
                                && updates.send(WatchEvent::Delta(text.to_owned())).is_err()
                            {
                                return Ok(());
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(e)) => return Err(error(format!("Managed2 events interrupted: {e}"))),
                        _ => {}
                    },
                }
            }
            // The cursor survives WebSocket restarts, including DO eviction.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn display_turn(
        &self,
        agent: &str,
        turn: &str,
        cursor: &mut String,
    ) -> Result<(), ManagedError> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (watch, output) = tokio::join!(self.watch_turn(agent, turn, cursor, tx), async move {
            let mut printed = String::new();
            while let Some(event) = rx.recv().await {
                match event {
                    WatchEvent::AgentEvent(_) => {}
                    WatchEvent::Delta(text) => {
                        print!("{text}");
                        io::stdout().flush().map_err(|e| error(e.to_string()))?;
                        printed.push_str(&text);
                    }
                    WatchEvent::Completed(message) => {
                        if printed != message {
                            print!("{}", message.strip_prefix(&printed).unwrap_or(&message));
                        }
                        println!();
                        io::stdout().flush().map_err(|e| error(e.to_string()))?;
                        return Ok(());
                    }
                    WatchEvent::Failed(message) => {
                        return Err(error(format!("Managed2 turn failed: {message}")));
                    }
                }
            }
            Ok(())
        });
        watch?;
        output
    }
}

pub(super) async fn run(
    agent: Option<String>,
    prompt: Option<String>,
    idempotency_key: Option<String>,
) -> Result<(), ManagedError> {
    let client = Client::from_environment()?;
    let mut agent = agent
        .map(|id| {
            Uuid::parse_str(&id)
                .map(|id| id.to_string())
                .map_err(|_| error("Managed2 attach requires an agent UUID"))
        })
        .transpose()?;
    let mut cursor = "0".to_owned();
    eprintln!(
        "Managed2 preview: text prompts only; no local tools, voice, model settings, or legacy TUI commands. /exit quits."
    );
    if let Some(prompt) = prompt {
        let request_id = idempotency_key
            .map(|key| Uuid::parse_str(&key).map_err(|_| error("idempotency key must be a UUID")))
            .transpose()?
            .unwrap_or_else(Uuid::new_v4);
        eprintln!("Managed2 request ID: {request_id}");
        let (id, turn) = client.submit(agent.as_deref(), &prompt, request_id).await?;
        eprintln!("Managed2 agent: {id}  turn: {turn}");
        return client.display_turn(&id, &turn, &mut cursor).await;
    }
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    loop {
        print!("> ");
        io::stdout().flush().map_err(|e| error(e.to_string()))?;
        let Some(input) = lines.next_line().await.map_err(|e| error(e.to_string()))? else {
            println!();
            return Ok(());
        };
        if input == "/exit" || input == "/quit" {
            return Ok(());
        }
        if input == "/id" {
            eprintln!(
                "Managed2 agent: {}",
                agent.as_deref().unwrap_or("not created")
            );
            continue;
        }
        if input.trim().is_empty() {
            continue;
        }
        let request_id = Uuid::new_v4();
        eprintln!("Managed2 request ID: {request_id}");
        let (id, turn) = client.submit(agent.as_deref(), &input, request_id).await?;
        if agent.is_none() {
            eprintln!("Managed2 agent: {id}");
            agent = Some(id.clone());
        }
        client.display_turn(&id, &turn, &mut cursor).await?;
    }
}
