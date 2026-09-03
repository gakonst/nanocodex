//! Loopback-only managed-agent server used by durability and client tests.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr, eyre};
use futures_util::StreamExt as _;
use nanocodex::{
    DurableAgentExt as _, Nanocodex, NanocodexError, OpenAi, PromptRequest, Turn, TurnControl,
};
use nanocodex_durability::{DurableSession, SqliteStore};
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    sync::{Mutex, broadcast, watch},
    task::JoinHandle,
};

const DEFAULT_BIND: &str = "127.0.0.1:8788";
const DEFAULT_WEBSOCKET_URL: &str = "wss://api.openai.com/v1/responses";
const EVENT_PAGE: usize = 256;

#[derive(Args)]
pub(crate) struct ManagedServer {
    /// Literal loopback address used by the testing server.
    #[arg(long, default_value = DEFAULT_BIND)]
    bind: SocketAddr,
    /// SQLite database shared by the managed projection and durable agents.
    #[arg(long, value_name = "PATH")]
    sqlite: PathBuf,
    /// Fixed workspace visible to every local managed agent.
    #[arg(long, value_name = "PATH")]
    workspace: PathBuf,
    /// OpenAI Platform API key used only by the server-side agent.
    #[arg(long, env = "OPENAI_API_KEY", hide_env_values = true, value_parser = NonEmptyStringValueParser::new())]
    openai_api_key: String,
    /// Responses WebSocket endpoint (overridable for deterministic tests).
    #[arg(long, default_value = DEFAULT_WEBSOCKET_URL)]
    openai_websocket_url: String,
    /// Static ncx_live bearer accepted by this loopback-only server.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    bearer: String,
    /// Fault-injection pause between persisted cancellation intent and delivery.
    #[arg(long, default_value_t = 0, hide = true)]
    fault_cancel_delay_ms: u64,
    /// Fault-injection pause between nested and managed terminal projection.
    #[arg(long, default_value_t = 0, hide = true)]
    fault_terminal_delay_ms: u64,
    /// Fault-injection pause after catalog validation and before tool-host ready.
    #[arg(long, default_value_t = 0, hide = true)]
    fault_tool_ready_delay_ms: u64,
}

impl ManagedServer {
    pub(crate) async fn run(self) -> Result<()> {
        if !self.bind.ip().is_loopback() {
            return Err(eyre!(
                "managed-server testing mode requires a literal loopback bind"
            ));
        }
        validate_bearer(&self.bearer)?;
        let database = Database::open(self.sqlite.clone())?;
        let (changed, _) = broadcast::channel(64);
        let (shutdown, _) = watch::channel(false);
        let state = AppState {
            bind: self.bind,
            database,
            workspace: Arc::new(self.workspace),
            sqlite: Arc::new(self.sqlite),
            openai_api_key: Arc::from(self.openai_api_key),
            openai_websocket_url: Arc::from(self.openai_websocket_url),
            bearer: Arc::from(self.bearer),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            runtime_init: Arc::new(Mutex::new(())),
            turn_tasks: Arc::new(Mutex::new(Vec::new())),
            tool_hosts: Arc::new(Mutex::new(HashMap::new())),
            next_tool_host: Arc::new(AtomicU64::new(1)),
            fault_cancel_delay: Duration::from_millis(self.fault_cancel_delay_ms),
            fault_terminal_delay: Duration::from_millis(self.fault_terminal_delay_ms),
            fault_tool_ready_delay: Duration::from_millis(self.fault_tool_ready_delay_ms),
            changed,
            shutdown,
        };
        let app = router(state.clone());
        let listener = tokio::net::TcpListener::bind(self.bind)
            .await
            .wrap_err_with(|| format!("failed to bind managed-server to {}", self.bind))?;
        state
            .recover_all()
            .await
            .map_err(|error| eyre!(error.message))?;
        eprintln!("Loopback managed server: http://{}", self.bind);
        let shutdown = state.shutdown.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                wait_for_shutdown_signal().await;
                shutdown.send_replace(true);
            })
            .await
            .wrap_err("managed-server stopped unexpectedly")?;
        state.shutdown().await;
        Ok(())
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/agents", post(create_agent))
        .route("/v1/agents/{agent}", get(agent_state))
        .route("/v1/agents/{agent}/turns", post(submit_turn))
        .route("/v1/agents/{agent}/turns/{turn}", get(turn_state))
        .route("/v1/agents/{agent}/turns/{turn}/steer", post(steer_turn))
        .route("/v1/agents/{agent}/turns/{turn}/cancel", post(cancel_turn))
        .route("/v1/agents/{agent}/events", get(events))
        .route("/v1/agents/{agent}/events/history", get(event_history))
        .route("/v1/agents/{agent}/tool-host", get(tool_host))
        .with_state(state)
}

#[derive(Clone)]
struct AppState {
    bind: SocketAddr,
    database: Database,
    workspace: Arc<PathBuf>,
    sqlite: Arc<PathBuf>,
    openai_api_key: Arc<str>,
    openai_websocket_url: Arc<str>,
    bearer: Arc<str>,
    runtimes: Arc<Mutex<HashMap<String, Arc<AgentRuntime>>>>,
    runtime_init: Arc<Mutex<()>>,
    turn_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    tool_hosts: Arc<Mutex<HashMap<String, ToolHostLease>>>,
    next_tool_host: Arc<AtomicU64>,
    fault_cancel_delay: Duration,
    fault_terminal_delay: Duration,
    fault_tool_ready_delay: Duration,
    changed: broadcast::Sender<String>,
    shutdown: watch::Sender<bool>,
}

struct AgentRuntime {
    agent: Nanocodex,
    controls: Mutex<HashMap<String, TurnControl>>,
    submissions: Mutex<()>,
}

struct ToolHostLease {
    generation: u64,
    fence: watch::Sender<bool>,
}

impl AppState {
    fn authorize(&self, headers: &HeaderMap) -> ApiResult<()> {
        let authorized = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.strip_prefix("Bearer ") == Some(self.bearer.as_ref()));
        authorized.then_some(()).ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "invalid testing bearer",
            )
        })
    }

    async fn runtime(&self, agent_id: &str) -> ApiResult<Arc<AgentRuntime>> {
        {
            let runtimes = self.runtimes.lock().await;
            if let Some(runtime) = runtimes.get(agent_id) {
                return Ok(Arc::clone(runtime));
            }
        }
        let _initializing = self.runtime_init.lock().await;
        {
            let runtimes = self.runtimes.lock().await;
            if let Some(runtime) = runtimes.get(agent_id) {
                return Ok(Arc::clone(runtime));
            }
        }
        if !self.database.agent_exists(agent_id).await? {
            return Err(ApiError::not_found(
                "agent_not_found",
                "managed agent does not exist",
            ));
        }
        let connection = Connection::open(self.sqlite.as_ref()).map_err(ApiError::internal)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(ApiError::internal)?;
        let durable = DurableSession::open(
            SqliteStore::from_connection(connection).map_err(ApiError::internal)?,
            agent_id.to_owned(),
        )
        .await
        .map_err(ApiError::internal)?;
        let openai = OpenAi::builder(self.openai_api_key.to_string())
            .websocket_url(self.openai_websocket_url.to_string())
            .websocket_warmup(false)
            .build()
            .map_err(ApiError::internal)?;
        let builder = Nanocodex::builder(openai)
            .workspace(self.workspace.as_ref().clone())
            .durability(durable)
            .await
            .map_err(ApiError::internal)?;
        let (agent, events) = builder.build().map_err(ApiError::internal)?;
        drop(events);
        let runtime = Arc::new(AgentRuntime {
            agent,
            controls: Mutex::new(HashMap::new()),
            submissions: Mutex::new(()),
        });
        // Re-admit an interrupted exact operation so the durability policy can
        // replay, recover, or fail it closed after a process restart.
        for turn in self.database.nonterminal_turns(agent_id).await? {
            self.start_existing(Arc::clone(&runtime), turn).await?;
        }
        self.runtimes
            .lock()
            .await
            .insert(agent_id.to_owned(), Arc::clone(&runtime));
        Ok(runtime)
    }

    async fn recover_all(&self) -> ApiResult<()> {
        for agent in self.database.agents_with_nonterminal_turns().await? {
            self.runtime(&agent).await?;
        }
        Ok(())
    }

    async fn start_existing(&self, runtime: Arc<AgentRuntime>, row: TurnRow) -> ApiResult<()> {
        if runtime.controls.lock().await.contains_key(&row.turn_id) {
            return Ok(());
        }
        let turn = runtime
            .agent
            .prompt(PromptRequest::new(row.input.text()?).request_id(row.operation_id.clone()))
            .await
            .map_err(ApiError::internal)?;
        let control = turn.control();
        runtime
            .controls
            .lock()
            .await
            .insert(row.turn_id.clone(), control.clone());
        if row.state == "cancelling" {
            match control.cancel().await {
                Ok(()) | Err(NanocodexError::TurnNotCancellable) => {}
                Err(error) => return Err(ApiError::internal(error)),
            }
        }
        self.spawn_turn(runtime, row.turn_id, turn).await;
        Ok(())
    }

    async fn spawn_turn(&self, runtime: Arc<AgentRuntime>, turn_id: String, turn: Turn) {
        let state = self.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = state.drive_turn(&turn_id, turn).await {
                eprintln!("managed turn projection failed: {error}");
            }
            runtime.controls.lock().await.remove(&turn_id);
        });
        let mut tasks = self.turn_tasks.lock().await;
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    async fn drive_turn(&self, turn_id: &str, mut turn: Turn) -> ApiResult<()> {
        let mut cancelled = false;
        let mut terminal_event = None;
        while let Some(event) = turn.next().await {
            let terminal = event.kind.is_terminal();
            cancelled |= terminal && event.payload.get().contains("\"status\":\"cancelled\"");
            let body = json!({"type":"event", "event":event});
            if terminal {
                terminal_event = Some(body);
                break;
            }
            let event_key = format!("turn:{turn_id}:agent:{}:{}", event.request_id, event.seq);
            self.database
                .append_event(turn_id, "event", body, &event_key)
                .await?;
            self.notify_turn(turn_id).await?;
        }
        if let Some(terminal_event) = &terminal_event {
            self.database
                .store_pending_terminal(turn_id, terminal_event)
                .await?;
        }
        match turn.result().await {
            Ok(result) => {
                self.database
                    .finish_turn(
                        turn_id,
                        TurnFinish {
                            state: "completed",
                            kind: "turn_completed",
                            body: json!({
                                "type":"turn_completed", "id":turn_id, "final_message":result.final_message(),
                                "usage":null, "citations":[], "usage_error":null
                            }),
                            nested_terminal: terminal_event.as_ref(),
                            error: None,
                            transaction_delay: self.fault_terminal_delay,
                        },
                    )
                    .await?;
            }
            Err(_error) if cancelled => {
                self.database
                    .finish_turn(
                        turn_id,
                        TurnFinish {
                            state: "cancelled",
                            kind: "turn_cancelled",
                            body: json!({
                                "type":"turn_cancelled", "id":turn_id
                            }),
                            nested_terminal: terminal_event.as_ref(),
                            error: None,
                            transaction_delay: self.fault_terminal_delay,
                        },
                    )
                    .await?;
            }
            Err(error) => {
                let detail = error.to_string();
                self.database
                    .finish_turn(
                        turn_id,
                        TurnFinish {
                            state: "failed",
                            kind: "turn_failed",
                            body: json!({
                                "type":"turn_failed", "id":turn_id, "error":detail
                            }),
                            nested_terminal: terminal_event.as_ref(),
                            error: Some(&detail),
                            transaction_delay: self.fault_terminal_delay,
                        },
                    )
                    .await?;
            }
        }
        self.notify_turn(turn_id).await
    }

    async fn notify_turn(&self, turn_id: &str) -> ApiResult<()> {
        let agent = self.database.agent_for_turn(turn_id).await?;
        drop(self.changed.send(agent));
        Ok(())
    }

    async fn shutdown(&self) {
        let runtimes = self
            .runtimes
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for runtime in runtimes {
            drop(runtime.agent.shutdown().await);
        }
        let mut tasks = std::mem::take(&mut *self.turn_tasks.lock().await);
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(mut task) = tasks.pop() {
            if tokio::time::timeout_at(deadline.into(), &mut task)
                .await
                .is_err()
            {
                task.abort();
                drop(task.await);
                for task in tasks {
                    task.abort();
                    drop(task.await);
                }
                return;
            }
        }
    }
}

#[derive(Clone)]
struct Database(Arc<Mutex<Connection>>);

impl Database {
    fn open(path: PathBuf) -> Result<Self> {
        let connection = Connection::open(&path).wrap_err_with(|| {
            format!("failed to open managed SQLite database {}", path.display())
        })?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS local_managed_agents (
               agent_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
               created_at REAL NOT NULL, updated_at REAL NOT NULL,
               completed_turns INTEGER NOT NULL DEFAULT 0,
               sse_connections INTEGER NOT NULL DEFAULT 0,
               tool_host_connections INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS local_managed_turns (
               turn_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, operation_id TEXT NOT NULL,
               input_json TEXT NOT NULL, state TEXT NOT NULL, accepted_cursor INTEGER NOT NULL,
               terminal_cursor INTEGER, created_at REAL NOT NULL, accepted_at REAL NOT NULL,
               updated_at REAL NOT NULL, attempt_count INTEGER NOT NULL DEFAULT 0,
               submission_count INTEGER NOT NULL DEFAULT 1,
               error TEXT, terminal_json TEXT, pending_terminal_json TEXT,
               UNIQUE(agent_id, operation_id),
               FOREIGN KEY(agent_id) REFERENCES local_managed_agents(agent_id)
             );
             CREATE TABLE IF NOT EXISTS local_managed_events (
               cursor INTEGER PRIMARY KEY AUTOINCREMENT, agent_id TEXT NOT NULL, turn_id TEXT,
               created_at REAL NOT NULL, kind TEXT NOT NULL, body_json TEXT NOT NULL,
               dedupe_key TEXT,
               FOREIGN KEY(agent_id) REFERENCES local_managed_agents(agent_id)
             );
             CREATE TABLE IF NOT EXISTS local_managed_cancel_intents (
               agent_id TEXT NOT NULL, turn_id TEXT NOT NULL, created_at REAL NOT NULL,
               PRIMARY KEY(agent_id, turn_id),
               FOREIGN KEY(agent_id) REFERENCES local_managed_agents(agent_id)
             );
             CREATE INDEX IF NOT EXISTS local_managed_events_agent_cursor
               ON local_managed_events(agent_id, cursor);
             CREATE UNIQUE INDEX IF NOT EXISTS local_managed_events_dedupe
               ON local_managed_events(agent_id, dedupe_key) WHERE dedupe_key IS NOT NULL;",
        )?;
        Ok(Self(Arc::new(Mutex::new(connection))))
    }

    async fn create_agent(&self, agent_id: &str) -> ApiResult<()> {
        let now = now();
        let mut db = self.0.lock().await;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(ApiError::internal)?;
        tx.execute("INSERT INTO local_managed_agents(agent_id,session_id,created_at,updated_at) VALUES(?1,?1,?2,?2)", params![agent_id, now]).map_err(ApiError::internal)?;
        append_event(
            &tx,
            agent_id,
            None,
            "agent_created",
            &json!({
                "type":"agent_created", "agent_id":agent_id, "capabilities":capabilities()
            }),
            Some("agent_created"),
        )?;
        tx.commit().map_err(ApiError::internal)
    }

    async fn agent_exists(&self, agent: &str) -> ApiResult<bool> {
        self.0
            .lock()
            .await
            .query_row(
                "SELECT 1 FROM local_managed_agents WHERE agent_id=?1",
                [agent],
                |_| Ok(()),
            )
            .optional()
            .map(|v| v.is_some())
            .map_err(ApiError::internal)
    }

    async fn agents_with_nonterminal_turns(&self) -> ApiResult<Vec<String>> {
        let db = self.0.lock().await;
        let mut statement = db
            .prepare(
                "SELECT DISTINCT agent_id FROM local_managed_turns WHERE state IN ('accepted','cancelling') ORDER BY agent_id",
            )
            .map_err(ApiError::internal)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(ApiError::internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(ApiError::internal)
    }

    async fn note_sse_connection(&self, agent: &str) -> ApiResult<()> {
        self.0
            .lock()
            .await
            .execute(
                "UPDATE local_managed_agents SET sse_connections=sse_connections+1 WHERE agent_id=?1",
                [agent],
            )
            .map(|_| ())
            .map_err(ApiError::internal)
    }

    async fn note_tool_host_connection(&self, agent: &str) -> ApiResult<()> {
        self.0
            .lock()
            .await
            .execute(
                "UPDATE local_managed_agents SET tool_host_connections=tool_host_connections+1 WHERE agent_id=?1",
                [agent],
            )
            .map(|_| ())
            .map_err(ApiError::internal)
    }

    async fn reserve_turn(
        &self,
        agent: &str,
        requested_turn: Option<&str>,
        operation: &str,
        input: &PromptInput,
    ) -> ApiResult<TurnReservation> {
        let now = now();
        let input_json = serde_json::to_string(input).map_err(ApiError::internal)?;
        let turn = {
            let mut db = self.0.lock().await;
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(ApiError::internal)?;
            if let Some(existing) = read_turn(&tx, "SELECT turn_id,agent_id,operation_id,input_json,state,accepted_cursor,terminal_cursor,created_at,accepted_at,updated_at,attempt_count,error,terminal_json FROM local_managed_turns WHERE agent_id=?1 AND operation_id=?2", params![agent,operation]).optional().map_err(ApiError::internal)? {
                if existing.input != *input
                    || requested_turn.is_some_and(|turn| existing.turn_id != turn)
                {
                    return Err(ApiError::conflict(
                        "idempotency_conflict",
                        "Idempotency-Key is already bound to another turn or input",
                    ));
                }
                tx.execute(
                    "UPDATE local_managed_turns SET submission_count=submission_count+1 WHERE turn_id=?1",
                    [&existing.turn_id],
                )
                .map_err(ApiError::internal)?;
                tx.commit().map_err(ApiError::internal)?;
                return Ok(TurnReservation::Existing(existing));
            }
            let turn = requested_turn
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
            if tx
                .query_row(
                    "SELECT 1 FROM local_managed_turns WHERE turn_id=?1",
                    [&turn],
                    |_| Ok(()),
                )
                .optional()
                .map_err(ApiError::internal)?
                .is_some()
            {
                return Err(ApiError::conflict(
                    "turn_id_conflict",
                    "turn ID is already bound to another operation",
                ));
            }
            let pre_cancelled = tx
                .query_row(
                    "SELECT 1 FROM local_managed_cancel_intents WHERE agent_id=?1 AND turn_id=?2",
                    params![agent, turn],
                    |_| Ok(()),
                )
                .optional()
                .map_err(ApiError::internal)?
                .is_some();
            let initial_state = if pre_cancelled {
                "cancelling"
            } else {
                "accepted"
            };
            tx.execute("INSERT INTO local_managed_turns(turn_id,agent_id,operation_id,input_json,state,accepted_cursor,created_at,accepted_at,updated_at,attempt_count) VALUES(?1,?2,?3,?4,?5,0,?6,?6,?6,1)", params![turn,agent,operation,input_json,initial_state,now]).map_err(ApiError::internal)?;
            let dedupe_key = format!("turn:{turn}:accepted");
            let cursor = append_event(
                &tx,
                agent,
                Some(&turn),
                "turn_accepted",
                &json!({
                    "type":"turn_accepted", "id":turn, "input":input, "replayed":false
                }),
                Some(&dedupe_key),
            )?;
            tx.execute(
                "UPDATE local_managed_turns SET accepted_cursor=?2 WHERE turn_id=?1",
                params![turn, cursor],
            )
            .map_err(ApiError::internal)?;
            if pre_cancelled {
                let dedupe_key = format!("turn:{turn}:cancelling");
                append_event(
                    &tx,
                    agent,
                    Some(&turn),
                    "turn_cancelling",
                    &json!({"type":"turn_cancelling","id":turn,"error":null,"retry_at":null}),
                    Some(&dedupe_key),
                )?;
                let terminal = json!({"type":"turn_cancelled", "id":turn});
                let terminal_key = format!("turn:{turn}:terminal");
                let terminal_cursor = append_event(
                    &tx,
                    agent,
                    Some(&turn),
                    "turn_cancelled",
                    &terminal,
                    Some(&terminal_key),
                )?;
                tx.execute(
                    "UPDATE local_managed_turns SET state='cancelled',terminal_cursor=?2,updated_at=?3,terminal_json=?4 WHERE turn_id=?1",
                    params![turn, terminal_cursor, now, serde_json::to_string(&terminal).map_err(ApiError::internal)?],
                )
                .map_err(ApiError::internal)?;
                tx.execute(
                    "DELETE FROM local_managed_cancel_intents WHERE agent_id=?1 AND turn_id=?2",
                    params![agent, turn],
                )
                .map_err(ApiError::internal)?;
            }
            tx.execute(
                "UPDATE local_managed_agents SET updated_at=?2 WHERE agent_id=?1",
                params![agent, now],
            )
            .map_err(ApiError::internal)?;
            tx.commit().map_err(ApiError::internal)?;
            turn
        };
        self.turn(&turn).await.map(TurnReservation::New)
    }

    async fn turn(&self, turn: &str) -> ApiResult<TurnRow> {
        let db = self.0.lock().await;
        read_turn(&db, "SELECT turn_id,agent_id,operation_id,input_json,state,accepted_cursor,terminal_cursor,created_at,accepted_at,updated_at,attempt_count,error,terminal_json FROM local_managed_turns WHERE turn_id=?1", [turn]).optional().map_err(ApiError::internal)?.ok_or_else(|| ApiError::not_found("turn_not_found","managed turn does not exist"))
    }

    async fn nonterminal_turns(&self, agent: &str) -> ApiResult<Vec<TurnRow>> {
        let db = self.0.lock().await;
        let mut statement = db.prepare("SELECT turn_id,agent_id,operation_id,input_json,state,accepted_cursor,terminal_cursor,created_at,accepted_at,updated_at,attempt_count,error,terminal_json FROM local_managed_turns WHERE agent_id=?1 AND state IN ('accepted','cancelling') ORDER BY created_at").map_err(ApiError::internal)?;
        statement
            .query_map([agent], map_turn)
            .map_err(ApiError::internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(ApiError::internal)
    }

    async fn append_event(
        &self,
        turn: &str,
        kind: &str,
        body: Value,
        dedupe_key: &str,
    ) -> ApiResult<()> {
        let agent = self.agent_for_turn(turn).await?;
        let db = self.0.lock().await;
        append_event(&db, &agent, Some(turn), kind, &body, Some(dedupe_key)).map(|_| ())
    }

    async fn store_pending_terminal(&self, turn: &str, body: &Value) -> ApiResult<()> {
        self.0
            .lock()
            .await
            .execute(
                "UPDATE local_managed_turns SET pending_terminal_json=?2 WHERE turn_id=?1 AND state IN ('accepted','cancelling')",
                params![turn, serde_json::to_string(body).map_err(ApiError::internal)?],
            )
            .map(|_| ())
            .map_err(ApiError::internal)
    }

    async fn finish_turn(&self, turn: &str, finish: TurnFinish<'_>) -> ApiResult<()> {
        let TurnFinish {
            state,
            kind,
            body,
            nested_terminal,
            error,
            transaction_delay,
        } = finish;
        let now = now();
        let mut db = self.0.lock().await;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(ApiError::internal)?;
        let agent: String = tx
            .query_row(
                "SELECT agent_id FROM local_managed_turns WHERE turn_id=?1",
                [turn],
                |row| row.get(0),
            )
            .map_err(ApiError::internal)?;
        let current: String = tx
            .query_row(
                "SELECT state FROM local_managed_turns WHERE turn_id=?1",
                [turn],
                |row| row.get(0),
            )
            .map_err(ApiError::internal)?;
        if !matches!(current.as_str(), "accepted" | "cancelling") {
            return Ok(());
        }
        if let Some(nested_terminal) = nested_terminal {
            let nested_key = format!("turn:{turn}:agent:terminal");
            append_event(
                &tx,
                &agent,
                Some(turn),
                "event",
                nested_terminal,
                Some(&nested_key),
            )?;
        }
        let dedupe_key = format!("turn:{turn}:terminal");
        let cursor = append_event(&tx, &agent, Some(turn), kind, &body, Some(&dedupe_key))?;
        if !transaction_delay.is_zero() {
            std::thread::sleep(transaction_delay);
        }
        let transitioned = tx.execute("UPDATE local_managed_turns SET state=?2,terminal_cursor=?3,updated_at=?4,error=?5,terminal_json=?6,pending_terminal_json=NULL WHERE turn_id=?1 AND state IN ('accepted','cancelling')", params![turn,state,cursor,now,error,serde_json::to_string(&body).map_err(ApiError::internal)?]).map_err(ApiError::internal)?;
        if transitioned != 1 {
            return Ok(());
        }
        let completed = i64::from(state == "completed");
        tx.execute("UPDATE local_managed_agents SET updated_at=?2,completed_turns=completed_turns+?3 WHERE agent_id=?1", params![agent,now,completed]).map_err(ApiError::internal)?;
        tx.commit().map_err(ApiError::internal)
    }

    async fn mark_cancelling(
        &self,
        requested_agent: &str,
        turn: &str,
    ) -> ApiResult<CancellationReservation> {
        let now = now();
        let mut db = self.0.lock().await;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(ApiError::internal)?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT agent_id,state FROM local_managed_turns WHERE turn_id=?1",
                [turn],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(ApiError::internal)?;
        if let Some((agent, state)) = existing {
            if agent != requested_agent || !matches!(state.as_str(), "accepted" | "cancelling") {
                return Err(ApiError::conflict(
                    "turn_not_active",
                    "managed turn is not active",
                ));
            }
            if state == "accepted" {
                let dedupe_key = format!("turn:{turn}:cancelling");
                append_event(
                    &tx,
                    &agent,
                    Some(turn),
                    "turn_cancelling",
                    &json!({"type":"turn_cancelling","id":turn,"error":null,"retry_at":null}),
                    Some(&dedupe_key),
                )?;
                tx.execute(
                    "UPDATE local_managed_turns SET state='cancelling',updated_at=?2 WHERE turn_id=?1",
                    params![turn, now],
                )
                .map_err(ApiError::internal)?;
            }
            tx.commit().map_err(ApiError::internal)?;
            return Ok(CancellationReservation::Active);
        }
        if tx
            .query_row(
                "SELECT 1 FROM local_managed_agents WHERE agent_id=?1",
                [requested_agent],
                |_| Ok(()),
            )
            .optional()
            .map_err(ApiError::internal)?
            .is_none()
        {
            return Err(ApiError::not_found(
                "agent_not_found",
                "managed agent does not exist",
            ));
        }
        tx.execute(
            "DELETE FROM local_managed_cancel_intents WHERE created_at < ?1",
            [now - 600.0],
        )
        .map_err(ApiError::internal)?;
        let intents: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM local_managed_cancel_intents WHERE agent_id=?1",
                [requested_agent],
                |row| row.get(0),
            )
            .map_err(ApiError::internal)?;
        if intents >= 256 {
            return Err(ApiError::conflict(
                "cancellation_capacity",
                "too many pending cancellation intents",
            ));
        }
        tx.execute(
            "INSERT OR IGNORE INTO local_managed_cancel_intents(agent_id,turn_id,created_at) VALUES(?1,?2,?3)",
            params![requested_agent, turn, now],
        )
        .map_err(ApiError::internal)?;
        tx.commit().map_err(ApiError::internal)?;
        Ok(CancellationReservation::Pending)
    }

    async fn agent_for_turn(&self, turn: &str) -> ApiResult<String> {
        self.0
            .lock()
            .await
            .query_row(
                "SELECT agent_id FROM local_managed_turns WHERE turn_id=?1",
                [turn],
                |row| row.get(0),
            )
            .map_err(ApiError::internal)
    }

    async fn state(&self, agent: &str, loaded: bool, connected_clients: usize) -> ApiResult<Value> {
        let db = self.0.lock().await;
        let (completed, last): (i64, f64) = db
            .query_row(
                "SELECT completed_turns,updated_at FROM local_managed_agents WHERE agent_id=?1",
                [agent],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| ApiError::not_found("agent_not_found", "managed agent does not exist"))?;
        let active = db.prepare("SELECT turn_id,input_json FROM local_managed_turns WHERE agent_id=?1 AND state IN ('accepted','cancelling') ORDER BY created_at").map_err(ApiError::internal)?.query_map([agent],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(ApiError::internal)?.collect::<rusqlite::Result<Vec<_>>>().map_err(ApiError::internal)?;
        let ids = active.iter().map(|v| v.0.clone()).collect::<Vec<_>>();
        let details = active
            .into_iter()
            .filter_map(|(id, input)| {
                serde_json::from_str::<Value>(&input)
                    .ok()
                    .map(|input| json!({"id":id,"input":input}))
            })
            .collect::<Vec<_>>();
        let latest: i64 = db
            .query_row(
                "SELECT COALESCE(MAX(cursor),0) FROM local_managed_events WHERE agent_id=?1",
                [agent],
                |r| r.get(0),
            )
            .map_err(ApiError::internal)?;
        Ok(
            json!({"agent_id":agent,"session_id":agent,"has_snapshot":completed>0,"completed_turns":completed,"last_active":last,"active_turns":ids,"active_turn_details":details,"agent_loaded":loaded,"connected_clients":connected_clients,"capabilities":capabilities(),"latest_event_cursor":latest.to_string(),"stream_error":null}),
        )
    }

    async fn events_after(
        &self,
        agent: &str,
        after: i64,
        limit: usize,
    ) -> ApiResult<Vec<StoredEvent>> {
        let db = self.0.lock().await;
        let mut stmt=db.prepare("SELECT cursor,turn_id,created_at,kind,body_json FROM local_managed_events WHERE agent_id=?1 AND cursor>?2 ORDER BY cursor LIMIT ?3").map_err(ApiError::internal)?;
        stmt.query_map(params![agent, after, limit as i64], map_event)
            .map_err(ApiError::internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(ApiError::internal)
    }

    async fn history(
        &self,
        agent: &str,
        before: Option<i64>,
        limit: usize,
    ) -> ApiResult<(Vec<StoredEvent>, bool, String)> {
        let db = self.0.lock().await;
        let before = before.unwrap_or(i64::MAX);
        let mut stmt=db.prepare("SELECT cursor,turn_id,created_at,kind,body_json FROM (SELECT cursor,turn_id,created_at,kind,body_json FROM local_managed_events WHERE agent_id=?1 AND cursor<?2 ORDER BY cursor DESC LIMIT ?3) ORDER BY cursor").map_err(ApiError::internal)?;
        let rows = stmt
            .query_map(params![agent, before, limit as i64], map_event)
            .map_err(ApiError::internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(ApiError::internal)?;
        let latest: i64 = db
            .query_row(
                "SELECT COALESCE(MAX(cursor),0) FROM local_managed_events WHERE agent_id=?1",
                [agent],
                |r| r.get(0),
            )
            .map_err(ApiError::internal)?;
        let has_more = rows.first().is_some_and(|first| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_managed_events WHERE agent_id=?1 AND cursor<?2)",
                params![agent, first.cursor],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
        });
        Ok((rows, has_more, latest.to_string()))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
enum PromptInput {
    Text(String),
    Content(Vec<Value>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCatalog {
    #[serde(rename = "type")]
    kind: String,
    tools: Vec<ToolCatalogEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCatalogEntry {
    provider: String,
    remote_name: String,
    definition: ToolCatalogDefinition,
    parallel_safe: bool,
    summary: Option<String>,
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ToolCatalogDefinition {
    Function {
        name: String,
        description: String,
        strict: bool,
        parameters: Value,
        output_schema: Option<Value>,
    },
    Custom {
        name: String,
        description: String,
        format: ToolCatalogGrammar,
    },
}

impl ToolCatalogDefinition {
    fn name(&self) -> &str {
        match self {
            Self::Function { name, .. } | Self::Custom { name, .. } => name,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCatalogGrammar {
    #[serde(rename = "type")]
    kind: String,
    syntax: String,
    definition: String,
}
impl PromptInput {
    fn text(&self) -> ApiResult<String> {
        match self {
            Self::Text(v) if !v.trim().is_empty() => Ok(v.clone()),
            Self::Text(_) => Err(ApiError::bad("invalid_input", "prompt must not be empty")),
            Self::Content(_) => Err(ApiError::bad(
                "unsupported_input",
                "local testing server currently accepts text prompts only",
            )),
        }
    }
}

#[derive(Deserialize)]
struct Submission {
    #[allow(dead_code)]
    id: Option<String>,
    input: PromptInput,
}
#[derive(Deserialize)]
struct Steer {
    input: PromptInput,
}
#[derive(Deserialize)]
struct EventQuery {
    cursor: Option<String>,
}
#[derive(Deserialize)]
struct HistoryQuery {
    before: Option<String>,
    limit: Option<usize>,
}
enum TurnReservation {
    New(TurnRow),
    Existing(TurnRow),
}
enum CancellationReservation {
    Active,
    Pending,
}
struct TurnFinish<'a> {
    state: &'a str,
    kind: &'a str,
    body: Value,
    nested_terminal: Option<&'a Value>,
    error: Option<&'a str>,
    transaction_delay: Duration,
}
#[derive(Clone)]
struct TurnRow {
    turn_id: String,
    agent_id: String,
    operation_id: String,
    input: PromptInput,
    state: String,
    accepted_cursor: i64,
    terminal_cursor: Option<i64>,
    created_at: f64,
    accepted_at: f64,
    updated_at: f64,
    attempt_count: i64,
    error: Option<String>,
    terminal: Option<Value>,
}
impl TurnRow {
    fn view(&self) -> Value {
        json!({"turn_id":self.turn_id,"state":self.state,"input":self.input,"accepted_cursor":self.accepted_cursor.to_string(),"terminal_cursor":self.terminal_cursor.map(|v|v.to_string()),"created_at":self.created_at,"accepted_at":self.accepted_at,"updated_at":self.updated_at,"attempt_count":self.attempt_count,"retry_at":null,"error":self.error,"terminal":self.terminal})
    }
}
struct StoredEvent {
    cursor: i64,
    turn_id: Option<String>,
    created_at: f64,
    kind: String,
    body: Value,
}
impl StoredEvent {
    fn envelope(&self) -> Value {
        let mut body = self.body.clone();
        if let Some(map) = body.as_object_mut() {
            map.insert("cursor".into(), self.cursor.to_string().into());
            map.insert("created_at".into(), self.created_at.into());
            map.insert(
                "turn_id".into(),
                self.turn_id.clone().map_or(Value::Null, Value::String),
            );
        }
        body
    }
}

async fn create_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    state.authorize(&headers)?;
    let id = uuid::Uuid::now_v7().to_string();
    state.database.create_agent(&id).await?;
    drop(state.changed.send(id.clone()));
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"agent_id":id,"session_id":id,"events_url":format!("http://{}/v1/agents/{id}/events",state.bind),"websocket_url":format!("ws://{}/v1/agents/{id}/ws",state.bind)}),
        ),
    ))
}
async fn agent_state(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    state.authorize(&headers)?;
    validate_id(&agent)?;
    let loaded = state.runtimes.lock().await.contains_key(&agent);
    let connected = usize::from(state.tool_hosts.lock().await.contains_key(&agent));
    Ok(Json(state.database.state(&agent, loaded, connected).await?))
}
async fn submit_turn(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Submission>,
) -> ApiResult<impl IntoResponse> {
    state.authorize(&headers)?;
    validate_id(&agent)?;
    let operation = idempotency(&headers)?;
    let input = body.input;
    input.text()?;
    if let Some(turn_id) = body.id.as_deref() {
        validate_id(turn_id)?;
    }
    let reservation = state
        .database
        .reserve_turn(&agent, body.id.as_deref(), &operation, &input)
        .await?;
    let (row, status) = match reservation {
        TurnReservation::New(row) => (row, StatusCode::ACCEPTED),
        TurnReservation::Existing(row) => (row, StatusCode::OK),
    };
    if row.state == "cancelled" {
        drop(state.changed.send(agent));
        return Ok((status, Json(row.view())));
    }
    let runtime = state.runtime(&agent).await?;
    let guard = runtime.submissions.lock().await;
    if matches!(row.state.as_str(), "accepted" | "cancelling") {
        state
            .start_existing(Arc::clone(&runtime), row.clone())
            .await?;
    }
    drop(state.changed.send(agent.clone()));
    drop(guard);
    Ok((status, Json(row.view())))
}
async fn turn_state(
    State(state): State<AppState>,
    Path((agent, turn)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    state.authorize(&headers)?;
    let row = state.database.turn(&turn).await?;
    if row.agent_id != agent {
        return Err(ApiError::not_found(
            "turn_not_found",
            "managed turn does not exist",
        ));
    }
    Ok(Json(row.view()))
}
async fn steer_turn(
    State(state): State<AppState>,
    Path((agent, turn)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Steer>,
) -> ApiResult<Json<Value>> {
    state.authorize(&headers)?;
    let text = body.input.text()?;
    let runtime = state.runtime(&agent).await?;
    let control = runtime
        .controls
        .lock()
        .await
        .get(&turn)
        .cloned()
        .ok_or_else(|| ApiError::conflict("turn_not_active", "managed turn is not active"))?;
    control.steer(text).await.map_err(ApiError::internal)?;
    Ok(Json(json!({"turn_id":turn,"state":"steered"})))
}
async fn cancel_turn(
    State(state): State<AppState>,
    Path((agent, turn)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    state.authorize(&headers)?;
    validate_id(&agent)?;
    validate_id(&turn)?;
    let reservation = state.database.mark_cancelling(&agent, &turn).await?;
    drop(state.changed.send(agent.clone()));
    if matches!(reservation, CancellationReservation::Pending) {
        return Ok(Json(json!({"turn_id":turn,"state":"cancelling"})));
    }
    let runtime = state.runtime(&agent).await?;
    let control = runtime.controls.lock().await.get(&turn).cloned();
    if !state.fault_cancel_delay.is_zero() {
        tokio::time::sleep(state.fault_cancel_delay).await;
    }
    if let Some(control) = control {
        match control.cancel().await {
            Ok(()) | Err(NanocodexError::TurnNotCancellable) => {}
            Err(error) => return Err(ApiError::internal(error)),
        }
    } else if matches!(
        state.database.turn(&turn).await?.state.as_str(),
        "accepted" | "cancelling"
    ) {
        return Err(ApiError::internal(
            "active managed turn has no runtime cancellation control",
        ));
    }
    Ok(Json(json!({"turn_id":turn,"state":"cancelling"})))
}
async fn event_history(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    state.authorize(&headers)?;
    if !state.database.agent_exists(&agent).await? {
        return Err(ApiError::not_found(
            "agent_not_found",
            "managed agent does not exist",
        ));
    }
    let limit = query.limit.unwrap_or(128);
    if !(1..=256).contains(&limit) {
        return Err(ApiError::bad(
            "invalid_limit",
            "history limit must be from 1 through 256",
        ));
    }
    let before = query.before.as_deref().map(parse_cursor).transpose()?;
    let (rows, has_more, latest) = state.database.history(&agent, before, limit).await?;
    Ok(Json(
        json!({"data":rows.iter().map(StoredEvent::envelope).collect::<Vec<_>>(),"has_more":has_more,"latest_cursor":latest}),
    ))
}
async fn events(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> ApiResult<impl IntoResponse> {
    state.authorize(&headers)?;
    if !state.database.agent_exists(&agent).await? {
        return Err(ApiError::not_found(
            "agent_not_found",
            "managed agent does not exist",
        ));
    }
    state.database.note_sse_connection(&agent).await?;
    let mut cursor = match query.cursor.as_deref().unwrap_or("latest") {
        "latest" => state
            .database
            .history(&agent, None, 1)
            .await?
            .2
            .parse()
            .unwrap_or(0),
        value => parse_cursor(value)?,
    };
    let mut changed = state.changed.subscribe();
    let mut shutdown = state.shutdown.subscribe();
    let database = state.database.clone();
    let stream_agent = agent.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<StoredEvent>(EVENT_PAGE);
    tokio::spawn(async move {
        if *shutdown.borrow() {
            return;
        }
        loop {
            match database
                .events_after(&stream_agent, cursor, EVENT_PAGE)
                .await
            {
                Ok(rows) if !rows.is_empty() => {
                    for row in rows {
                        cursor = row.cursor;
                        if *shutdown.borrow() {
                            return;
                        }
                        tokio::select! {
                            result = tx.send(row) => {
                                if result.is_err() {
                                    return;
                                }
                            }
                            _ = shutdown.changed() => return,
                        }
                    }
                }
                Ok(_) => {
                    tokio::select! {
                        _ = tx.closed() => return,
                        _ = shutdown.changed() => return,
                        changed = changed.recv() => match changed {
                            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return,
                        },
                    }
                }
                Err(_) => return,
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|row| {
            let event = Event::default()
                .id(row.cursor.to_string())
                .event(row.kind.clone())
                .data(row.envelope().to_string());
            (Ok::<_, Infallible>(event), rx)
        })
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
async fn tool_host(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    state.authorize(&headers)?;
    if !state.database.agent_exists(&agent).await? {
        return Err(ApiError::not_found(
            "agent_not_found",
            "managed agent does not exist",
        ));
    }
    let shutdown = state.shutdown.subscribe();
    if *shutdown.borrow() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_shutting_down",
            "managed testing server is shutting down",
        ));
    }
    let generation = state.next_tool_host.fetch_add(1, Ordering::Relaxed);
    let (fence, fenced) = watch::channel(false);
    let tool_state = state.clone();
    Ok(upgrade
        .on_upgrade(move |socket| {
            serve_tool_host(
                socket, shutdown, fenced, fence, tool_state, agent, generation,
            )
        })
        .into_response())
}
async fn serve_tool_host(
    socket: WebSocket,
    shutdown: watch::Receiver<bool>,
    fenced: watch::Receiver<bool>,
    fence: watch::Sender<bool>,
    state: AppState,
    agent: String,
    generation: u64,
) {
    hold_tool_host(
        socket,
        shutdown,
        fenced,
        fence,
        state.clone(),
        agent.clone(),
        generation,
    )
    .await;
    let mut hosts = state.tool_hosts.lock().await;
    if hosts
        .get(&agent)
        .is_some_and(|lease| lease.generation == generation)
    {
        hosts.remove(&agent);
    }
}
async fn hold_tool_host(
    mut socket: WebSocket,
    mut shutdown: watch::Receiver<bool>,
    mut fenced: watch::Receiver<bool>,
    fence: watch::Sender<bool>,
    state: AppState,
    agent: String,
    generation: u64,
) {
    if *shutdown.borrow() {
        return;
    }
    let first = tokio::select! {
        _ = shutdown.changed() => return,
        _ = fenced.changed() => return,
        message = socket.recv() => message,
    };
    let Some(Ok(Message::Text(catalog))) = first else {
        return;
    };
    if !valid_tool_catalog(&catalog) {
        return;
    }
    if state
        .database
        .note_tool_host_connection(&agent)
        .await
        .is_err()
    {
        return;
    }
    if !state.fault_tool_ready_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(state.fault_tool_ready_delay) => {}
            _ = shutdown.changed() => return,
            _ = fenced.changed() => return,
        }
    }
    tokio::select! {
        result = socket.send(Message::Text(json!({"type":"ready"}).to_string().into())) => {
            if result.is_err() {
                return;
            }
        }
        _ = shutdown.changed() => return,
        _ = fenced.changed() => return,
    }
    let replaced = state
        .tool_hosts
        .lock()
        .await
        .insert(agent.clone(), ToolHostLease { generation, fence });
    if let Some(replaced) = replaced {
        let _ = replaced.fence.send(true);
    }
    loop {
        let message = tokio::select! {
            _ = shutdown.changed() => return,
            _ = fenced.changed() => return,
            message = socket.recv() => message,
        };
        let Some(Ok(message)) = message else {
            return;
        };
        match message {
            Message::Text(text) => {
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let reply = match value.get("type").and_then(Value::as_str) {
                    Some("ping") => Some((
                        json!({"type":"pong","nonce":value.get("nonce").cloned().unwrap_or(Value::Null)}),
                        false,
                    )),
                    Some("drain") => Some((json!({"type":"draining"}), true)),
                    _ => None,
                };
                if let Some((reply, draining)) = reply {
                    tokio::select! {
                        result = socket.send(Message::Text(reply.to_string().into())) => {
                            if result.is_err() {
                                return;
                            }
                        }
                        _ = shutdown.changed() => return,
                        _ = fenced.changed() => return,
                    }
                    if draining {
                        return;
                    }
                }
            }
            Message::Close(_) => return,
            _ => {}
        }
    }
}

fn valid_tool_catalog(encoded: &str) -> bool {
    if encoded.len() > 256 * 1024 {
        return false;
    }
    let Ok(catalog) = serde_json::from_str::<ToolCatalog>(encoded) else {
        return false;
    };
    if catalog.kind != "catalog" || catalog.tools.len() > 256 {
        return false;
    }
    let mut names = HashSet::new();
    let mut identities = HashSet::new();
    for entry in catalog.tools {
        if validate_id(&entry.provider).is_err()
            || validate_id(&entry.remote_name).is_err()
            || !(1..=120_000).contains(&entry.timeout_ms)
            || entry
                .summary
                .as_ref()
                .is_some_and(|summary| summary.is_empty() || summary.len() > 2 * 1024)
        {
            return false;
        }
        let name = entry.definition.name().to_owned();
        if validate_id(&name).is_err()
            || matches!(name.as_str(), "exec" | "tool_search" | "wait")
            || !names.insert(name)
            || !identities.insert((entry.provider, entry.remote_name))
        {
            return false;
        }
        let definition_valid = match entry.definition {
            ToolCatalogDefinition::Function {
                description,
                strict,
                parameters,
                output_schema,
                ..
            } => {
                let _ = strict;
                !description.is_empty()
                    && description.len() <= 8 * 1024
                    && parameters.is_object()
                    && serde_json::to_vec(&parameters).is_ok_and(|value| value.len() <= 64 * 1024)
                    && output_schema.is_none_or(|schema| {
                        schema.is_object()
                            && serde_json::to_vec(&schema)
                                .is_ok_and(|value| value.len() <= 64 * 1024)
                    })
            }
            ToolCatalogDefinition::Custom {
                description,
                format,
                ..
            } => {
                !description.is_empty()
                    && description.len() <= 8 * 1024
                    && format.kind == "grammar"
                    && validate_id(&format.syntax).is_ok()
                    && !format.definition.is_empty()
                    && format.definition.len() <= 64 * 1024
            }
        };
        if !definition_valid {
            return false;
        }
        let _ = entry.parallel_safe;
    }
    true
}

fn append_event(
    db: &Connection,
    agent: &str,
    turn: Option<&str>,
    kind: &str,
    body: &Value,
    dedupe_key: Option<&str>,
) -> ApiResult<i64> {
    let inserted = db.execute("INSERT OR IGNORE INTO local_managed_events(agent_id,turn_id,created_at,kind,body_json,dedupe_key) VALUES(?1,?2,?3,?4,?5,?6)",params![agent,turn,now(),kind,serde_json::to_string(body).map_err(ApiError::internal)?,dedupe_key]).map_err(ApiError::internal)?;
    if inserted == 1 {
        return Ok(db.last_insert_rowid());
    }
    let Some(dedupe_key) = dedupe_key else {
        return Err(ApiError::internal("event insert was unexpectedly ignored"));
    };
    db.query_row(
        "SELECT cursor FROM local_managed_events WHERE agent_id=?1 AND dedupe_key=?2",
        params![agent, dedupe_key],
        |row| row.get(0),
    )
    .map_err(ApiError::internal)
}
fn map_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnRow> {
    let input: String = row.get(3)?;
    let terminal: Option<String> = row.get(12)?;
    Ok(TurnRow {
        turn_id: row.get(0)?,
        agent_id: row.get(1)?,
        operation_id: row.get(2)?,
        input: serde_json::from_str(&input).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        state: row.get(4)?,
        accepted_cursor: row.get(5)?,
        terminal_cursor: row.get(6)?,
        created_at: row.get(7)?,
        accepted_at: row.get(8)?,
        updated_at: row.get(9)?,
        attempt_count: row.get(10)?,
        error: row.get(11)?,
        terminal: terminal
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
    })
}
fn read_turn<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> rusqlite::Result<TurnRow> {
    db.query_row(sql, params, map_turn)
}
fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    let body: String = row.get(4)?;
    Ok(StoredEvent {
        cursor: row.get(0)?,
        turn_id: row.get(1)?,
        created_at: row.get(2)?,
        kind: row.get(3)?,
        body: serde_json::from_str(&body).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}
#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            if let Some(terminate) = &mut terminate {
                terminate.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {}
    }
}
#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    drop(tokio::signal::ctrl_c().await);
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |v| v.as_secs_f64())
}
fn capabilities() -> Value {
    json!({"durable_turns":true,"resumable_events":true,"live_steer":true,"live_cancel":true,"workspace":"private-hosted-tools-v1","execution_environments":true})
}
fn parse_cursor(value: &str) -> ApiResult<i64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|v| v.is_ascii_digit())
    {
        return Err(ApiError::bad(
            "invalid_cursor",
            "event cursor must be canonical unsigned decimal",
        ));
    }
    value
        .parse()
        .map_err(|_| ApiError::bad("invalid_cursor", "event cursor is too large"))
}
fn idempotency(headers: &HeaderMap) -> ApiResult<String> {
    let value = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::bad("missing_idempotency_key", "Idempotency-Key is required"))?;
    if value.is_empty() || value.len() > 256 || !value.bytes().all(|v| (0x21..=0x7e).contains(&v)) {
        return Err(ApiError::bad(
            "invalid_idempotency_key",
            "Idempotency-Key must be 1-256 visible ASCII characters",
        ));
    }
    Ok(value.to_owned())
}
fn validate_id(value: &str) -> ApiResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'.' | b'_' | b':' | b'-'))
    {
        return Err(ApiError::bad(
            "invalid_id",
            "managed ID must be 1-128 safe ASCII characters",
        ));
    }
    Ok(())
}
fn validate_bearer(value: &str) -> Result<()> {
    let Some(rest) = value.strip_prefix("ncx_live_") else {
        return Err(eyre!("--bearer must be an ncx_live key"));
    };
    let Some((id, secret)) = rest.split_once('_') else {
        return Err(eyre!("--bearer must be an ncx_live key"));
    };
    let safe = |v: &str| {
        v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    if id.len() != 12 || secret.len() != 43 || !safe(id) || !safe(secret) {
        return Err(eyre!("--bearer must be an ncx_live key"));
    }
    Ok(())
}

type ApiResult<T> = std::result::Result<T, ApiError>;
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}
impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    fn bad(code: &'static str, message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
    fn not_found(code: &'static str, message: &'static str) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }
    fn conflict(code: &'static str, message: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "local_server_error",
            error.to_string(),
        )
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":self.code,"message":self.message})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bearer_and_cursor_validation_are_exact() {
        assert!(
            validate_bearer(&format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))).is_ok()
        );
        assert!(validate_bearer("sk-test").is_err());
        assert!(matches!(parse_cursor("0"), Ok(0)));
        assert!(parse_cursor("01").is_err());
    }
}
