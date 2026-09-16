//! Private, versioned control of a running terminal. No agent or renderer ownership.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{Server, connect, list};

pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1024 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_REPLAY_BYTES: usize = 16 * 1024 * 1024;
const MAX_REQUESTS: usize = 65536;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// The application must resolve admission before completing this command.
pub struct Command {
    pub request: Request,
    pub reply: oneshot::Sender<Value>,
}

impl Command {
    pub fn finish(self, value: Value) {
        let _ = self.reply.send(value);
    }
    pub fn reject(self, code: &str) {
        self.finish(rejected(code));
    }
}

pub fn rejected(code: &str) -> Value {
    json!({"status":"rejected", "code":code})
}
pub fn accepted(value: Value) -> Value {
    json!({"status":"accepted", "result":value})
}
pub fn unknown(message: impl ToString) -> Value {
    json!({"status":"unknown", "message":message.to_string()})
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Conversation {
    pub session_id: String,
    pub root_session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub origin: String,
    pub role: String,
    pub rollout_path: Option<PathBuf>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Registration {
    pub protocol_version: u32,
    pub instance_id: String,
    pub pid: u32,
    pub started_at_unix_ms: u128,
    pub backend: String,
    pub socket_path: PathBuf,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_token: String,
    pub active_generation: String,
    pub active_session_id: Option<String>,
    pub conversation: Option<Conversation>,
}

struct Entry {
    request: Request,
    result: Value,
}
struct Inner {
    registration: Registration,
    state: Value,
    conversations: HashMap<String, Conversation>,
    seq: u64,
    events: VecDeque<(Value, usize)>,
    bytes: usize,
    ledger: HashMap<String, Entry>,
    ledger_bytes: usize,
    projection: Vec<Value>,
    projection_bytes: usize,
    snapshots: VecDeque<(String, Vec<Value>)>,
    projection_truncated: bool,
    settings_override: Option<Value>,
    managed_cursors: HashMap<String, u64>,
    active_turns: HashMap<String, Vec<String>>,
}

/// Small shared projection. Mutations are delivered to the owning UI loop.
#[derive(Clone)]
pub struct Bridge {
    inner: Arc<Mutex<Inner>>,
    changed: watch::Sender<u64>,
    registration_changed: watch::Sender<Registration>,
    commands: mpsc::Sender<Command>,
}

impl Bridge {
    pub(crate) fn new(registration: Registration, commands: mpsc::Sender<Command>) -> Self {
        let (changed, _) = watch::channel(0);
        let (registration_changed, _) = watch::channel(registration.clone());
        Self {
            inner: Arc::new(Mutex::new(Inner {
                registration,
                state: json!({"connection":"connecting"}),
                conversations: HashMap::new(),
                seq: 0,
                events: VecDeque::new(),
                bytes: 0,
                ledger: HashMap::new(),
                ledger_bytes: 0,
                projection: Vec::new(),
                projection_bytes: 0,
                snapshots: VecDeque::new(),
                projection_truncated: false,
                settings_override: None,
                managed_cursors: HashMap::new(),
                active_turns: HashMap::new(),
            })),
            changed,
            registration_changed,
            commands,
        }
    }

    pub fn registration(&self) -> Registration {
        self.inner.lock().unwrap().registration.clone()
    }

    pub fn conversation(&self, value: Conversation) {
        let mut inner = self.inner.lock().unwrap();
        if inner.conversations.get(&value.session_id) == Some(&value) {
            return;
        }
        inner
            .conversations
            .insert(value.session_id.clone(), value.clone());
        if inner.registration.active_session_id.as_deref() == Some(&value.session_id) {
            inner.registration.conversation = Some(value);
            self.registration_changed
                .send_replace(inner.registration.clone());
        }
    }

    /// Called at the UI ordering boundary, including every focus change.
    pub fn state(&self, session: Option<&str>, mut value: Value) {
        let mut inner = self.inner.lock().unwrap();
        if inner.registration.active_session_id.as_deref() != session {
            inner.settings_override = None;
        }
        if let Some(patch) = inner.settings_override.clone() {
            if patch
                .as_object()
                .unwrap()
                .iter()
                .all(|(k, v)| value["settings"][k] == *v)
            {
                inner.settings_override = None;
            } else {
                for (key, setting) in patch.as_object().unwrap() {
                    value["settings"][key] = setting.clone();
                }
            }
        }
        if inner.registration.active_session_id.as_deref() != session {
            inner.settings_override = None;
            let previous = inner.registration.active_session_id.clone();
            inner.registration.active_session_id = session.map(str::to_owned);
            let generation = inner.registration.active_generation.parse::<u64>().unwrap() + 1;
            inner.registration.active_generation = generation.to_string();
            inner.registration.conversation =
                session.and_then(|id| inner.conversations.get(id).cloned());
            self.registration_changed
                .send_replace(inner.registration.clone());
            Self::append(
                &mut inner,
                "conversation.active_changed",
                json!({"previous_session_id":previous,
                "session_id":session, "active_generation":generation.to_string()}),
            );
        }
        let draft_changed = inner.state.get("composer") != value.get("composer");
        let revision = inner.state["draft_revision"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            + u64::from(draft_changed);
        value["draft_revision"] = json!(revision.to_string());
        let settings_changed = inner.state.get("settings") != value.get("settings");
        let settings_revision = inner.state["settings_revision"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            + u64::from(settings_changed);
        value["settings_revision"] = json!(settings_revision.to_string());
        if inner.state != value {
            inner.state = value;
            // Do not retain a user's draft in replay history.
            Self::append(
                &mut inner,
                "state.changed",
                json!({"draft_revision":revision.to_string(),
                "settings_revision":settings_revision.to_string()}),
            );
        }
        self.changed.send_replace(inner.seq);
    }

    /// Publishes authoritative settings before acknowledging the command, even if UI rendering lags.
    pub fn settings_committed(&self, patch: Value) {
        let mut inner = self.inner.lock().unwrap();
        for (key, value) in patch.as_object().unwrap() {
            inner.state["settings"][key] = value.clone();
        }
        let revision = inner.state["settings_revision"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        inner.state["settings_revision"] = json!(revision.to_string());
        inner.settings_override = Some(patch);
        Self::append(
            &mut inner,
            "settings.changed",
            json!({"settings_revision":revision.to_string()}),
        );
        self.changed.send_replace(inner.seq);
    }

    pub fn snapshot(&self) -> Value {
        let mut inner = self.inner.lock().unwrap();
        Self::snapshot_inner(&mut inner)
    }

    fn snapshot_inner(inner: &mut Inner) -> Value {
        let token = format!("{}:{}", inner.registration.instance_id, inner.seq);
        if !inner.snapshots.iter().any(|(id, _)| id == &token) {
            inner
                .snapshots
                .push_back((token.clone(), inner.projection.clone()));
            while inner.snapshots.len() > 4 {
                inner.snapshots.pop_front();
            }
        }
        json!({"instance_id":inner.registration.instance_id, "active_session_id":inner.registration.active_session_id,
            "active_generation":inner.registration.active_generation, "seq":inner.seq.to_string(),
            "state":inner.state, "conversations":inner.conversations,"active_turns":inner.active_turns,
            "snapshot_token":token,"live_history_truncated":inner.projection_truncated,
            "capabilities":{"questions_read":false,"replay":"process","request_deduplication":"process",
                "max_frame_bytes":MAX_FRAME,"max_replay_bytes":MAX_REPLAY_BYTES}})
    }

    /// Validate at dispatch, not when bytes first arrive on the socket.
    pub fn validate(&self, request: &Request) -> Result<(), &'static str> {
        let inner = self.inner.lock().unwrap();
        Self::validate_inner(&inner, request)
    }

    fn validate_inner(inner: &Inner, request: &Request) -> Result<(), &'static str> {
        let p = &request.params;
        if p["expected_instance_id"].as_str() != Some(&inner.registration.instance_id) {
            return Err("instance_changed");
        }
        if p["expected_session_id"].as_str().is_none()
            || p["expected_session_id"].as_str() != inner.registration.active_session_id.as_deref()
        {
            return Err("session_changed");
        }
        if p["expected_active_generation"].as_str() != Some(&inner.registration.active_generation) {
            return Err("session_changed");
        }
        if inner.state["connection"] != "ready" {
            return Err("session_loading");
        }
        if inner.state["ui_blocked"] == true {
            return Err("ui_blocked");
        }
        if request.method == "settings.set"
            && p["expected_settings_revision"] != inner.state["settings_revision"]
        {
            return Err("settings_changed");
        }
        Ok(())
    }

    pub fn publish(&self, kind: &str, data: Value) {
        let mut inner = self.inner.lock().unwrap();
        if kind == "managed.event" {
            if let (Some(session), Some(cursor)) = (
                data["session_id"].as_str(),
                data["cursor"].as_str().and_then(|v| v.parse::<u64>().ok()),
            ) {
                if inner
                    .managed_cursors
                    .get(session)
                    .is_some_and(|last| *last >= cursor)
                {
                    return;
                }
                inner.managed_cursors.insert(session.to_owned(), cursor);
            }
        }
        if kind == "managed.event" && data["agent_id"].is_number() {
            if let (Some(session), Some(root)) = (
                data["event"]["request_id"].as_str(),
                data["session_id"].as_str(),
            ) {
                inner
                    .conversations
                    .entry(session.to_owned())
                    .or_insert_with(|| Conversation {
                        session_id: session.into(),
                        root_session_id: Some(root.into()),
                        parent_session_id: None,
                        origin: "spawn".into(),
                        role: "subagent".into(),
                        rollout_path: None,
                    });
            }
        }
        Self::project(&mut inner, kind, &data);
        Self::append(&mut inner, kind, data);
        self.changed.send_replace(inner.seq);
    }

    fn project(inner: &mut Inner, kind: &str, data: &Value) {
        let event = if kind == "agent.event" {
            data
        } else if kind == "managed.event" {
            &data["event"]
        } else {
            return;
        };
        let session = event["request_id"].as_str().unwrap_or("");
        let payload = &event["payload"];
        let turn = data["turn_id"]
            .as_str()
            .or_else(|| payload["turn_id"].as_str())
            .unwrap_or("");
        let item = payload["item_id"]
            .as_str()
            .or_else(|| payload["call_id"].as_str())
            .unwrap_or("");
        let category = event["type"].as_str().unwrap_or("");
        if !matches!(
            category,
            "assistant.delta"
                | "assistant.message"
                | "tool.call"
                | "tool.result"
                | "run.started"
                | "run.completed"
                | "run.failed"
        ) {
            return;
        }
        if category == "run.started" {
            let turns = inner.active_turns.entry(session.to_owned()).or_default();
            if !turns.iter().any(|id| id == turn) {
                turns.push(turn.to_owned());
            }
        } else if matches!(category, "run.completed" | "run.failed") {
            if let Some(turns) = inner.active_turns.get_mut(session) {
                turns.retain(|id| id != turn);
            }
        }
        let family = if category.starts_with("assistant.") {
            "assistant"
        } else if category.starts_with("run.") {
            "run"
        } else {
            category
        };
        let key = format!(
            "{session}:{turn}:{family}:{item}:{}",
            payload["model_call_index"]
        );
        let index = inner.projection.iter().position(|v| v["id"] == key);
        let mut record =
            json!({"id":key,"session_id":session,"turn_id":turn,"type":category,"payload":payload});
        if category == "assistant.delta" {
            let old = index
                .and_then(|i| inner.projection[i]["payload"]["text"].as_str())
                .unwrap_or("");
            let text = format!("{old}{}", payload["text"].as_str().unwrap_or(""));
            if text.len() > MAX_FRAME / 2 {
                inner.projection_truncated = true;
                return;
            }
            record["payload"]["text"] = json!(text);
        }
        if record.to_string().len() > MAX_FRAME / 2 {
            inner.projection_truncated = true;
            return;
        }
        inner.projection_bytes += record.to_string().len();
        if let Some(index) = index {
            inner.projection_bytes -= inner.projection[index].to_string().len();
            inner.projection[index] = record;
        } else {
            inner.projection.push(record);
        }
        while inner.projection.len() > 512 || inner.projection_bytes > MAX_REPLAY_BYTES {
            inner.projection_bytes -= inner.projection.remove(0).to_string().len();
            inner.projection_truncated = true;
        }
    }

    fn append(inner: &mut Inner, kind: &str, data: Value) {
        inner.seq += 1;
        let event = json!({"type":kind,"instance_id":inner.registration.instance_id,
            "seq":inner.seq.to_string(),"active_generation":inner.registration.active_generation,"data":data});
        let bytes = event.to_string().len();
        // Oversized events are explicit gaps, never a silently truncated payload.
        if bytes >= MAX_FRAME {
            inner.events.clear();
            inner.bytes = 0;
            return;
        }
        inner.bytes += bytes;
        inner.events.push_back((event, bytes));
        while inner.events.len() > MAX_EVENTS || inner.bytes > MAX_REPLAY_BYTES {
            inner.bytes -= inner.events.pop_front().unwrap().1;
        }
    }

    pub fn replay(&self, after: u64) -> Result<Vec<Value>, Value> {
        let mut inner = self.inner.lock().unwrap();
        let first = inner
            .events
            .front()
            .and_then(|(v, _)| v["seq"].as_str()?.parse::<u64>().ok())
            .unwrap_or(inner.seq + 1);
        if after > inner.seq || after.saturating_add(1) < first {
            return Err(
                json!({"code":"replay_gap", "snapshot":Self::snapshot_inner(&mut inner),
                "earliest_seq":first.to_string(),"latest_seq":inner.seq.to_string()}),
            );
        }
        Ok(inner
            .events
            .iter()
            .filter(|(v, _)| v["seq"].as_str().unwrap().parse::<u64>().unwrap() > after)
            .map(|(v, _)| v.clone())
            .collect())
    }

    async fn dispatch(&self, request: Request) -> Value {
        if request.id.is_empty() || request.id.len() > 128 {
            return rejected("invalid_request_id");
        }
        match request.method.as_str() {
            "state.get" => return self.snapshot(),
            "history.live" => {
                let inner = self.inner.lock().unwrap();
                let token = request.params["snapshot_token"].as_str().unwrap_or("");
                let Some((_, records)) = inner.snapshots.iter().find(|(id, _)| id == token) else {
                    return rejected("snapshot_expired");
                };
                let offset = request.params["offset"].as_u64().unwrap_or(0) as usize;
                let limit = request.params["limit"].as_u64().unwrap_or(32).clamp(1, 128) as usize;
                let mut page = Vec::new();
                let mut bytes = 0;
                for record in records.iter().skip(offset).take(limit) {
                    bytes += record.to_string().len();
                    if bytes > MAX_FRAME * 3 / 4 {
                        break;
                    }
                    page.push(record);
                }
                return json!({"records":page,"next_offset":offset+page.len(),"has_more":offset+page.len()<records.len()});
            }
            "request.get" => {
                let inner = self.inner.lock().unwrap();
                return request.params["request_id"]
                    .as_str()
                    .and_then(|id| inner.ledger.get(id))
                    .map(|entry| entry.result.clone())
                    .unwrap_or_else(|| unknown("request not retained by this instance"));
            }
            "prompt" | "steer" | "cancel" | "settings.set" => {}
            "models.list" | "history.list" | "command.status" => {
                return self.forward(request).await;
            }
            _ => return rejected("unsupported_method"),
        }
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(entry) = inner.ledger.get(&request.id) {
                return if entry.request.method == request.method
                    && entry.request.params == request.params
                {
                    entry.result.clone()
                } else {
                    rejected("request_id_conflict")
                };
            }
            if let Err(code) = Self::validate_inner(&inner, &request) {
                return rejected(code);
            }
            let request_bytes = serde_json::to_vec(&request).unwrap().len();
            if inner.ledger.len() >= MAX_REQUESTS
                || inner.ledger_bytes + request_bytes > MAX_REPLAY_BYTES
            {
                return rejected("request_capacity");
            }
            inner.ledger_bytes += request_bytes;
            inner.ledger.insert(
                request.id.clone(),
                Entry {
                    request: request.clone(),
                    result: json!({"status":"pending"}),
                },
            );
        }
        // This task is owned by the server, not the client's connection lifetime.
        let result = self.forward(request.clone()).await;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.ledger_bytes += result.to_string().len();
            inner.ledger.get_mut(&request.id).unwrap().result = result.clone();
        }
        self.publish(
            "request.resolved",
            json!({"request_id":request.id,"receipt":result}),
        );
        result
    }

    async fn forward(&self, request: Request) -> Value {
        let (reply, receive) = oneshot::channel();
        if self.commands.try_send(Command { request, reply }).is_err() {
            return rejected("command_queue_full");
        }
        receive
            .await
            .unwrap_or_else(|_| unknown("TUI stopped before acknowledging command"))
    }
}

pub fn registry_dir() -> io::Result<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
        .ok_or_else(|| io::Error::other("set CODEX_HOME to discover terminal sessions"))?;
    Ok(home.join("nanocodex/tui/instances"))
}

#[derive(clap::Args)]
pub struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}
#[derive(clap::Subcommand)]
enum CliCommand {
    /// List private registrations without authentication tokens.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Relay JSONL to a running TUI, including through SSH.
    Connect {
        instance_id: String,
        #[arg(long, required = true)]
        stdio: bool,
    },
}
impl Cli {
    pub async fn run(self) -> io::Result<()> {
        #[cfg(unix)]
        match self.command {
            CliCommand::List { .. } => {
                println!("{}", serde_json::to_string(&list()?)?);
                Ok(())
            }
            CliCommand::Connect { instance_id, .. } => connect(&instance_id).await,
        }
        #[cfg(not(unix))]
        Err(io::Error::other("TUI control currently requires Unix"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bridge() -> (Bridge, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel(32);
        (
            Bridge::new(
                Registration {
                    protocol_version: VERSION,
                    instance_id: "instance".into(),
                    pid: 1,
                    started_at_unix_ms: 0,
                    backend: "test".into(),
                    socket_path: "/unused".into(),
                    auth_token: "secret".into(),
                    active_generation: "0".into(),
                    active_session_id: None,
                    conversation: None,
                },
                tx,
            ),
            rx,
        )
    }
    fn request() -> Request {
        Request {
            id: "one".into(),
            method: "prompt".into(),
            params: json!({
        "expected_instance_id":"instance","expected_session_id":"session","expected_active_generation":"1",
        "input":{"text":"literal /cancel"}}),
        }
    }

    #[tokio::test]
    async fn concurrent_duplicates_and_lost_reply_do_not_resubmit() {
        let (bridge, mut commands) = bridge();
        bridge.state(Some("session"), json!({"connection":"ready"}));
        let owner = bridge.clone();
        let original = tokio::spawn(async move { owner.dispatch(request()).await });
        let command = commands.recv().await.unwrap();
        assert_eq!(bridge.dispatch(request()).await["status"], "pending");
        let mut conflict = request();
        conflict.params["input"]["text"] = json!("different");
        assert_eq!(
            bridge.dispatch(conflict).await["code"],
            "request_id_conflict"
        );
        command.finish(accepted(json!({"turn_id":"turn"})));
        let receipt = original.await.unwrap();
        assert_eq!(bridge.dispatch(request()).await, receipt);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn switching_away_and_back_fences_old_commands_and_preserves_draft() {
        let (bridge, _) = bridge();
        let state = json!({"connection":"ready","composer":{"text":"my draft","cursor":3,"attachments":["image"]}});
        bridge.state(Some("session"), state.clone());
        assert!(bridge.validate(&request()).is_ok());
        bridge.state(Some("side"), state.clone());
        bridge.state(Some("session"), state.clone());
        assert_eq!(bridge.validate(&request()), Err("session_changed"));
        assert_eq!(bridge.snapshot()["state"]["composer"], state["composer"]);
        assert!(
            !serde_json::to_string(&bridge.replay(0).unwrap())
                .unwrap()
                .contains("my draft")
        );
    }

    #[tokio::test]
    async fn expired_replay_recovers_partial_text_at_an_immutable_snapshot() {
        let (bridge, _) = bridge();
        let event = |text| {
            json!({"request_id":"session","type":"assistant.delta", "payload":{
            "turn_id":"turn","item_id":"message","model_call_index":0,"text":text}})
        };
        bridge.publish("agent.event", event("hello "));
        bridge.publish("agent.event", event("world"));
        for n in 0..MAX_EVENTS {
            bridge.publish("tick", json!(n));
        }
        let gap = bridge.replay(0).unwrap_err();
        let snapshot = &gap["snapshot"];
        bridge.publish("agent.event", event("!"));
        let page = bridge
            .dispatch(Request {
                id: "read".into(),
                method: "history.live".into(),
                params: json!({"snapshot_token":snapshot["snapshot_token"]}),
            })
            .await;
        assert_eq!(page["records"][0]["payload"]["text"], "hello world");
        let suffix = bridge
            .replay(snapshot["seq"].as_str().unwrap().parse().unwrap())
            .unwrap();
        assert_eq!(suffix.len(), 1);
        assert_eq!(suffix[0]["data"]["payload"]["text"], "!");
    }

    #[tokio::test]
    async fn blocked_ui_rejects_before_enqueuing_and_settings_ack_fences_old_revision() {
        let (bridge, mut commands) = bridge();
        bridge.state(
            Some("session"),
            json!({"connection":"ready","ui_blocked":true,"settings":{"effort":"low"}}),
        );
        assert_eq!(bridge.dispatch(request()).await["code"], "ui_blocked");
        assert!(commands.try_recv().is_err());
        bridge.state(
            Some("session"),
            json!({"connection":"ready","settings":{"effort":"low"}}),
        );
        let mut change = request();
        change.method = "settings.set".into();
        change.params["expected_settings_revision"] =
            bridge.snapshot()["state"]["settings_revision"].clone();
        bridge.settings_committed(json!({"effort":"high"}));
        bridge.state(
            Some("session"),
            json!({"connection":"ready","settings":{"effort":"low"}}),
        );
        assert_eq!(bridge.validate(&change), Err("settings_changed"));
        assert_eq!(bridge.snapshot()["state"]["settings"]["effort"], "high");
    }

    #[tokio::test]
    async fn managed_reconnect_deduplicates_delta_and_large_history_cannot_stall_pagination() {
        let (bridge, _) = bridge();
        let event = json!({"session_id":"session","cursor":"1","turn_id":"turn","event":{
            "type":"assistant.delta","request_id":"session","payload":{"text":"once","item_id":"message"}}});
        bridge.publish("managed.event", event.clone());
        bridge.publish("managed.event", event);
        bridge.publish("agent.event",json!({"type":"tool.result","request_id":"session","payload":{"result":"x".repeat(MAX_FRAME)}}));
        let snapshot = bridge.snapshot();
        let page = bridge
            .dispatch(Request {
                id: "read".into(),
                method: "history.live".into(),
                params: json!({"snapshot_token":snapshot["snapshot_token"]}),
            })
            .await;
        assert_eq!(page["records"][0]["payload"]["text"], "once");
        assert_eq!(page["has_more"], false);
        assert_eq!(snapshot["live_history_truncated"], true);
    }

    #[tokio::test]
    async fn dropped_owner_is_unknown_not_rejected() {
        let (bridge, mut commands) = bridge();
        bridge.state(Some("session"), json!({"connection":"ready"}));
        let task = tokio::spawn(async move { bridge.dispatch(request()).await });
        drop(commands.recv().await.unwrap());
        assert_eq!(task.await.unwrap()["status"], "unknown");
    }
}
