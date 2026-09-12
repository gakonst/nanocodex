use crate::{
    Error, Result,
    ax::{Node, Sessions},
    engine::{index, point, string},
    selection,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};
#[path = "browser_artifacts.rs"]
mod artifacts;
#[path = "browser_beforeunload.rs"]
pub(crate) mod beforeunload;
#[path = "browser_children.rs"]
mod children;
#[path = "browser_chooser.rs"]
mod chooser;
#[path = "browser_clipboard.rs"]
mod clipboard;
#[path = "browser_contract.rs"]
mod contract;
#[path = "browser_dialog.rs"]
mod dialog;
#[path = "browser_downloads.rs"]
mod downloads;
#[path = "browser_durable.rs"]
mod durable;
#[path = "browser_geometry.rs"]
mod geometry;
#[path = "browser_iab.rs"]
pub mod iab;
#[path = "browser_iab_host.rs"]
mod iab_host;
#[path = "browser_persistence.rs"]
pub mod persistence;
#[path = "browser_raw_events.rs"]
mod raw_events;
#[path = "browser_raw_wait_host.rs"]
mod raw_wait_host;
#[path = "browser_screencast.rs"]
mod screencast;
#[path = "browser_selectors.rs"]
mod selectors;
#[path = "browser_snapshot.rs"]
pub mod snapshot;
#[path = "browser_surface.rs"]
mod surface;
#[path = "browser_webmcp.rs"]
mod webmcp;

// The terminal counter value is a permanent exhausted sentinel. Every wire
// command on a connection, including fire-and-forget callbacks, shares it.
fn allocate_request_id(next: &mut u64) -> Result<u64> {
    let id = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| Error::action("CDP request ID space exhausted"))?;
    Ok(id)
}

#[derive(Clone, Copy, Default)]
struct CallAdmissions<'a, 'runtime> {
    webmcp: Option<&'a webmcp::Admission>,
    beforeunload: Option<&'a beforeunload::Operation<'runtime>>,
    html_continuation: Option<downloads::HtmlContinuation<'a>>,
}

pub struct Cdp {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    pub events: VecDeque<Value>,
    event_bytes: usize,
    event_internal: VecDeque<bool>,
    dialogs: std::sync::Arc<std::sync::Mutex<dialog::State>>,
    raw_events: std::sync::Arc<std::sync::Mutex<raw_events::Log>>,
    screencast: screencast::State,
    webmcp: std::sync::Arc<std::sync::Mutex<webmcp::State>>,
    beforeunload: beforeunload::State,
    dialog_request: Option<u64>,
    dialog_error_before_close: bool,
    clipboard: Option<std::sync::Arc<std::sync::Mutex<clipboard::Clipboard>>>,
    clipboard_sessions: std::collections::BTreeSet<String>,
    downloads: Option<downloads::Shared>,
    download_extension: bool,
    waiting: std::collections::BTreeSet<u64>,
    responses: BTreeMap<u64, Value>,
    deadlines: Vec<Instant>,
    cleanup_until: Option<Instant>,
    revoking_download: bool,
    disabling_download: std::collections::BTreeSet<String>,
}
impl Cdp {
    pub fn connect(endpoint: &str) -> Result<Self> {
        let url = url::Url::parse(endpoint).map_err(|_| Error::invalid("Invalid CDP endpoint"))?;
        if url.scheme() != "ws" {
            return Err(Error::invalid("Expected a ws:// CDP endpoint"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| Error::invalid("CDP host missing"))?;
        let addresses = (host, url.port_or_known_default().unwrap_or(80)).to_socket_addrs()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = None;
        for address in addresses {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Ok(s) = TcpStream::connect_timeout(&address, remaining) {
                stream = Some(s);
                break;
            }
        }
        let stream =
            stream.ok_or_else(|| Error::new(-32006, "CDP connection failed or timed out"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(crate::protocol::MAX_FRAME))
            .max_frame_size(Some(crate::protocol::MAX_FRAME));
        let (socket, _) = tungstenite::client::client_with_config(
            endpoint,
            MaybeTlsStream::Plain(stream),
            Some(config),
        )
        .map_err(|e| Error::new(-32006, format!("CDP handshake failed: {e}")))?;
        Ok(Self {
            socket,
            next: 1,
            events: VecDeque::new(),
            event_bytes: 0,
            event_internal: VecDeque::new(),
            dialogs: Default::default(),
            raw_events: Default::default(),
            screencast: Default::default(),
            webmcp: Default::default(),
            beforeunload: Default::default(),
            dialog_request: None,
            dialog_error_before_close: false,
            clipboard: None,
            clipboard_sessions: Default::default(),
            downloads: None,
            download_extension: false,
            waiting: Default::default(),
            responses: Default::default(),
            deadlines: vec![],
            cleanup_until: None,
            revoking_download: false,
            disabling_download: Default::default(),
        })
    }
    pub fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.call_with_webmcp(method, params, session, None)
    }
    fn call_with_webmcp(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        webmcp: Option<&webmcp::Admission>,
    ) -> Result<Value> {
        self.call_scoped(
            method,
            params,
            session,
            CallAdmissions {
                webmcp,
                ..Default::default()
            },
        )
    }
    fn call_scoped(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        mut admissions: CallAdmissions<'_, '_>,
    ) -> Result<Value> {
        admissions.beforeunload = admissions.beforeunload.filter(|operation| {
            self.beforeunload_command_valid(operation, method, &params, session)
        });
        admissions.html_continuation = admissions.html_continuation.filter(|continuation| {
            continuation.admits(self.downloads.as_ref(), method, &params, session)
        });
        if self.waiting.len() >= 16 {
            return Err(Error::action("CDP nested command limit exceeded"));
        }
        if self.deadlines.is_empty() {
            self.cleanup_until = None;
        }
        let budget = params["timeout"]
            .as_u64()
            .filter(|ms| *ms > 0)
            .map(|ms| Duration::from_millis(ms.clamp(1, 120000)))
            .unwrap_or(Duration::from_secs(10));
        let own_deadline = Instant::now() + budget;
        let deadline = self
            .deadlines
            .last()
            .copied()
            .map_or(own_deadline, |outer| outer.min(own_deadline));
        if Instant::now() >= deadline {
            return Err(Error::new(
                -32006,
                "CDP inherited request deadline expired before dispatch",
            ));
        }
        let id = allocate_request_id(&mut self.next)?;
        self.waiting.insert(id);
        self.deadlines.push(deadline);
        let owns_dialog = method == "Page.handleJavaScriptDialog"
            && self.dialog_request.is_none()
            && self.dialogs.lock().unwrap().watches_request(session);
        if owns_dialog {
            self.dialog_request = Some(id);
            self.dialog_error_before_close = false;
        }
        let owns_disable = method == "Fetch.disable"
            && session.is_some_and(|s| self.disabling_download.insert(s.to_owned()));
        let result = self.request(id, method, params, session, deadline, admissions);
        if owns_dialog {
            self.dialog_request = None;
            self.dialog_error_before_close = false;
        }
        if owns_disable {
            self.disabling_download.remove(session.unwrap());
        }
        self.deadlines.pop();
        self.waiting.remove(&id);
        self.responses.remove(&id);
        result
    }
    fn dialog_closes_request(&self, id: u64) -> bool {
        self.dialog_request == Some(id)
            && !self.dialog_error_before_close
            && self.dialogs.lock().unwrap().matching_close_observed()
    }
    fn observe_dialog_reply(&mut self, response: &Value) {
        if self.dialog_request.is_some()
            && response["id"].as_u64() == self.dialog_request
            && response.get("error").is_some()
            && !self.dialogs.lock().unwrap().matching_close_observed()
        {
            self.dialog_error_before_close = true;
        }
    }
    fn defer_response(&mut self, value: Value) -> Result<()> {
        if let Some(id) = value["id"].as_u64().filter(|id| self.waiting.contains(id)) {
            let bytes = self
                .responses
                .values()
                .map(|v| v.to_string().len())
                .sum::<usize>();
            if bytes + value.to_string().len() > crate::protocol::MAX_FRAME {
                return Err(Error::action("CDP deferred response byte limit exceeded"));
            }
            self.responses.insert(id, value);
        }
        Ok(())
    }
    fn response(
        &self,
        value: Value,
        id: u64,
        html_continuation: Option<downloads::HtmlContinuation<'_>>,
    ) -> Result<Value> {
        if value["id"].as_u64() == Some(id)
            && value["error"]["code"].as_i64() == Some(-32001)
            && html_continuation
                .is_some_and(|continuation| continuation.retired(self.downloads.as_ref()))
        {
            // This private callback has no result consumer. Retirement makes
            // its missing-session reply terminal; it is not a successful CDP
            // acknowledgement. Keep waiting for the real enclosing response.
            return Ok(Value::Null);
        }
        if let Some(error) = value.get("error") {
            return Err(Error::new(
                error["code"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .unwrap_or(-10005),
                error["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string()),
            ));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| Error::action("CDP response lacks result"))
    }
    fn receive_event(&mut self, value: Value) -> Result<()> {
        self.receive_event_scoped(value, None)
    }
    fn receive_event_scoped(
        &mut self,
        value: Value,
        automatic: Option<(&beforeunload::Operation<'_>, Instant)>,
    ) -> Result<()> {
        // Freeze the source and collect internal IDs before callbacks: download
        // handling can recursively receive later CDP events on this connection.
        let context = self.dialogs.lock().unwrap().raw_event_context(&value);
        let internal = context
            .source
            .as_ref()
            .is_some_and(|owner| self.screencast.observe(&owner.tab, owner.top_level, &value));
        self.webmcp.lock().unwrap().event(&context, &value);
        // This observer only revokes local operation views; it cannot send.
        self.beforeunload.observe(&context, &value);
        self.download_event(&value)?;
        let fresh = {
            let mut dialogs = self.dialogs.lock().unwrap();
            let previous = context
                .source
                .as_ref()
                .and_then(|source| dialogs.get(&source.tab))
                .and_then(|dialog| dialog["id"].as_str())
                .map(str::to_owned);
            dialogs.event(&value);
            beforeunload::State::fresh(&dialogs, &context, &value, previous.as_deref())
        };
        self.raw_events
            .lock()
            .unwrap()
            .event(context, &value, internal)?;
        self.enqueue_event(value, internal);
        if let (Some((operation, deadline)), Some(fresh)) = (automatic, fresh) {
            self.send_beforeunload(operation, fresh, deadline)?;
        }
        Ok(())
    }

    fn enqueue_event(&mut self, value: Value, internal: bool) {
        if self.events.len() != self.event_internal.len() {
            // Only standalone Cdp exposes direct queue mutation. Its private
            // capture owner is inactive, so reset metadata without expanding it.
            self.event_internal = std::iter::repeat_n(false, self.events.len()).collect();
            self.event_bytes = self
                .events
                .iter()
                .map(|value| value.to_string().len())
                .sum();
        }
        let size = value.to_string().len();
        // State already retained the one pending screenshot payload, even when
        // this best-effort event history has no room for the frame.
        if size > 4 * 1024 * 1024 {
            return;
        }
        while self.events.len() >= 256 || self.event_bytes + size > 4 * 1024 * 1024 {
            if let Some(old) = self.events.pop_front() {
                self.event_internal.pop_front();
                self.event_bytes = self.event_bytes.saturating_sub(old.to_string().len());
            } else {
                self.event_internal.clear();
                self.event_bytes = 0;
                break;
            }
        }
        self.event_bytes += size;
        self.events.push_back(value);
        self.event_internal.push_back(internal);
    }

    fn drain_events(&mut self) -> Vec<(Value, bool)> {
        self.event_bytes = 0;
        // Browser exclusively owns this queue. Standalone public Cdp users can
        // inspect/mutate events, but cannot activate its private capture owner.
        let events = self
            .events
            .drain(..)
            .map(|value| (value, self.event_internal.pop_front().unwrap_or(false)))
            .collect();
        self.event_internal.clear();
        events
    }

    fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Value,
        session: Option<&str>,
        deadline: Instant,
        admissions: CallAdmissions<'_, '_>,
    ) -> Result<Value> {
        // Automatic handling has an absolute bound. A later unrelated human
        // approval may extend ordinary CDP accounting, never this native intent.
        let automatic_deadline = deadline;
        if self.dialog_closes_request(id) {
            return Ok(json!({}));
        }
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(s) = session {
            request["sessionId"] = json!(s);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::new(
                -32006,
                "CDP inherited request deadline expired before dispatch",
            ));
        }
        if let MaybeTlsStream::Plain(stream) = self.socket.get_ref() {
            stream.set_write_timeout(Some(Duration::from_millis(
                remaining.as_millis().max(1) as u64
            )))?;
        }
        // Only the native WebMCP call path supplies this proof. No callback or
        // provider operation occurs between the final check and socket send.
        if let Some(admission) = admissions.webmcp {
            admission.check_source(&self.dialogs.lock().unwrap())?;
            self.webmcp.lock().unwrap().validate(admission)?;
        }
        self.socket
            .send(Message::text(request.to_string()))
            .map_err(|e| Error::new(-32006, e.to_string()))?;
        if let Some(operation) = admissions.beforeunload {
            operation.dispatched(method, automatic_deadline);
        }
        loop {
            if self.dialog_closes_request(id) {
                return Ok(json!({}));
            }
            if let Some(value) = self.responses.remove(&id) {
                return self.response(value, id, admissions.html_continuation);
            }
            // A trusted approval callback can suspend every enclosing command.
            // Re-read this frame's deadline after nested event handling instead
            // of retaining the pre-approval copy passed into this function.
            let deadline = self.deadlines.last().copied().unwrap_or(deadline);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::new(-32006, "CDP request timed out"));
            }
            if let MaybeTlsStream::Plain(s) = self.socket.get_ref() {
                // Quantize to milliseconds: Darwin rejects timeval usec == 1_000_000.
                s.set_read_timeout(Some(Duration::from_millis(
                    remaining.as_millis().max(1) as u64
                )))?;
            }
            let message = self.socket.read().map_err(|e| {
                Error::new(
                    -32006,
                    format!("CDP {method} disconnected or timed out: {e}"),
                )
            })?;
            match message {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text)?;
                    if value.get("id").is_some() {
                        if self.beforeunload.discard_reply(&value) {
                            continue;
                        }
                        self.observe_dialog_reply(&value);
                        if value["id"] != id {
                            self.defer_response(value)?;
                            continue;
                        }
                        return self.response(value, id, admissions.html_continuation);
                    }
                    if value["method"] == "Runtime.bindingCalled"
                        && value["params"]["name"] == "__skyre_clipboard"
                        && value["sessionId"]
                            .as_str()
                            .is_some_and(|s| self.clipboard_sessions.contains(s))
                    {
                        self.clipboard_request(&value)?;
                        continue;
                    }
                    self.receive_event_scoped(
                        value,
                        admissions
                            .beforeunload
                            .map(|operation| (operation, automatic_deadline.min(deadline))),
                    )?;
                    if self.dialog_closes_request(id) {
                        return Ok(json!({}));
                    }
                }
                Message::Ping(data) => self
                    .socket
                    .send(Message::Pong(data))
                    .map_err(|e| Error::action(e.to_string()))?,
                Message::Close(_) => return Err(Error::new(-32006, "CDP connection closed")),
                _ => {}
            }
        }
    }
    /// Receive queued events without issuing a browser/renderer command. Modal
    /// dialogs can suspend ordinary CDP commands, including Target queries.
    fn poll_events(&mut self) -> Result<()> {
        let own = self.deadlines.is_empty();
        if own {
            self.cleanup_until = None;
            self.deadlines
                .push(Instant::now() + Duration::from_secs(10));
        }
        let result = self.poll_events_inner();
        if own {
            self.deadlines.pop();
        }
        result
    }
    fn poll_events_inner(&mut self) -> Result<()> {
        for _ in 0..256 {
            // Event handlers may issue nested commands, which replace this
            // socket's timeout with their remaining command budget. Restore
            // the short polling timeout before every read, including the idle
            // read after such a command's acknowledgement.
            if self
                .deadlines
                .last()
                .is_some_and(|end| Instant::now() >= *end)
            {
                return Err(Error::new(-32006, "CDP request timed out"));
            }
            if let MaybeTlsStream::Plain(stream) = self.socket.get_ref() {
                stream.set_read_timeout(Some(Duration::from_millis(1)))?;
            }
            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(());
                }
                Err(error) => {
                    return Err(Error::new(
                        -32006,
                        format!("CDP event stream failed: {error}"),
                    ));
                }
            };
            match message {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text)?;
                    if value.get("id").is_some() {
                        if self.beforeunload.discard_reply(&value) {
                            continue;
                        }
                        self.observe_dialog_reply(&value);
                        self.defer_response(value)?;
                        continue;
                    }
                    if value["method"] == "Runtime.bindingCalled"
                        && value["params"]["name"] == "__skyre_clipboard"
                        && value["sessionId"]
                            .as_str()
                            .is_some_and(|s| self.clipboard_sessions.contains(s))
                    {
                        self.clipboard_request(&value)?;
                        continue;
                    }
                    self.receive_event(value)?;
                }
                Message::Ping(data) => self
                    .socket
                    .send(Message::Pong(data))
                    .map_err(|e| Error::new(-32006, e.to_string()))?,
                Message::Close(_) => return Err(Error::new(-32006, "CDP connection closed")),
                _ => {}
            }
        }
        Ok(())
    }
    fn clipboard_request(&mut self, event: &Value) -> Result<()> {
        let Some(payload) = event["params"]["payload"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        else {
            return Ok(());
        };
        let Some(id) = payload["id"]
            .as_i64()
            .filter(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
        else {
            return Ok(());
        };
        let Some(store) = &self.clipboard else {
            return Ok(());
        };
        let result = {
            let mut store = store
                .lock()
                .map_err(|_| Error::action("Clipboard store unavailable"))?;
            match payload["op"].as_str() {
                Some("read") => Ok(json!(store.items)),
                Some("write") => store.write(&payload["items"]).map(|_| json!([])),
                _ => Err(Error::invalid("Invalid clipboard operation")),
            }
        };
        let response = match result {
            Ok(items) => json!({"id":id,"ok":true,"items":items}),
            Err(e) => json!({"id":id,"ok":false,"error":e.message}),
        };
        let command_id = allocate_request_id(&mut self.next)?;
        let request = json!({"id":command_id,"sessionId":event["sessionId"],"method":"Runtime.evaluate","params":{"contextId":event["params"]["executionContextId"],"expression":format!("globalThis.__skyre_clipboard_state?.respond({response})"),"returnByValue":true}});
        self.socket
            .send(Message::text(request.to_string()))
            .map_err(|e| Error::new(-32006, e.to_string()))?;
        Ok(())
    }
}
struct Browser {
    endpoint: String,
    extension: bool,
    extension_started: bool,
    iab: Option<iab::Authority>,
    iab_host: iab_host::State,
    durable: Option<durable::Journal>,
    host_binding: Option<durable::HostBinding>,
    client: Option<Cdp>,
    authorization_connection: String,
    sessions: BTreeMap<String, String>,
    name: String,
    invalidated: bool,
    surface: surface::Surface,
    contract: contract::State,
}
impl Browser {
    fn ensure_context(&self) -> Result<()> {
        if self
            .host_binding
            .as_ref()
            .is_some_and(|binding| !binding.active)
        {
            return Err(Error::action(
                "Browser route requires an active trusted host turn",
            ));
        }
        if let Some(authority) = &self.iab {
            authority.context()?;
        }
        Ok(())
    }
    fn metadata(&self, id: &str) -> Value {
        if self.iab.is_some() {
            return self.iab_info(id);
        }
        let mut info = contract::metadata(id, &self.name);
        if self.extension {
            info["type"] = json!("extension");
        }
        info
    }
    fn client(&mut self) -> Result<&mut Cdp> {
        if self.client.is_none() {
            self.cancel_raw_waits();
            let mut nonce = [0u8; 16];
            getrandom::fill(&mut nonce)
                .map_err(|_| Error::action("Cannot identify browser connection"))?;
            self.client = Some(Cdp::connect(&self.endpoint)?);
            self.client.as_mut().unwrap().dialogs = self.surface.dialogs.clone();
            self.client.as_mut().unwrap().raw_events = self.surface.raw_events.clone();
            self.client.as_mut().unwrap().screencast = self.surface.screencast.clone();
            self.client.as_mut().unwrap().webmcp = self.surface.webmcp.clone();
            self.authorization_connection = nonce.iter().map(|b| format!("{b:02x}")).collect();
            self.client.as_mut().unwrap().downloads = Some(self.surface.downloads.clone());
            self.client.as_mut().unwrap().download_extension = self.extension;
            if self.extension && !self.extension_started && self.host_binding.is_none() {
                let result = self
                    .client
                    .as_mut()
                    .unwrap()
                    .call("Skyre.beginTurn", json!({}), None);
                if let Err(error) = result {
                    self.client = None;
                    return Err(error);
                }
                self.extension_started = true;
            }
        }
        Ok(self.client.as_mut().unwrap())
    }
    fn poll_events(&mut self) -> Result<()> {
        let result = self.client()?.poll_events();
        if let Some(client) = self.client.as_mut() {
            for (event, internal) in client.drain_events() {
                self.iab_event(&event);
                self.contract.event(&event);
                self.surface.event(event, &mut self.sessions, internal);
            }
        }
        if result.as_ref().is_err_and(|error| error.code == -32006) {
            self.client = None;
            self.sessions.clear();
            self.invalidated = true;
            if let Some(authority) = &mut self.iab {
                authority.invalidate_inputs();
            }
            self.surface.disconnect();
            self.contract.disconnect();
        }
        if result.is_ok() {
            let _ = self.maintain_child_choosers();
            let _ = self.download_maintenance();
        }
        result
    }
    fn check_dialog_method(&self, tab: &str, method: &str) -> Result<()> {
        self.ensure_context()?;
        if let Some(authority) = &self.iab {
            authority.require_tab(tab)?;
        }
        self.surface
            .dialogs
            .lock()
            .unwrap()
            .check_method(tab, method)
    }
    fn call_for_tab(
        &mut self,
        tab: &str,
        method: &str,
        args: Value,
        session: Option<&str>,
    ) -> Result<Value> {
        self.check_dialog_method(tab, method)?;
        self.call(method, args, session)
    }
    fn call(&mut self, method: &str, args: Value, session: Option<&str>) -> Result<Value> {
        self.call_owned(method, args, session, false, None)
    }
    /// Only existing native-owned rollback/release sites use this path. It
    /// retains the same provider guard, durable intent and transport handling.
    /// No public request can select this exemption.
    fn call_maintenance(
        &mut self,
        method: &str,
        args: Value,
        session: Option<&str>,
    ) -> Result<Value> {
        self.call_owned(method, args, session, true, None)
    }
    fn call_owned(
        &mut self,
        method: &str,
        args: Value,
        session: Option<&str>,
        maintenance: bool,
        attempted: Option<&mut bool>,
    ) -> Result<Value> {
        self.call_owned_webmcp(method, args, session, maintenance, attempted, None)
    }
    fn call_webmcp(
        &mut self,
        method: &str,
        args: Value,
        session: Option<&str>,
        admission: &webmcp::Admission,
    ) -> Result<Value> {
        self.call_owned_webmcp(method, args, session, false, None, Some(admission))
    }
    fn call_owned_webmcp(
        &mut self,
        method: &str,
        args: Value,
        session: Option<&str>,
        maintenance: bool,
        attempted: Option<&mut bool>,
        admission: Option<&webmcp::Admission>,
    ) -> Result<Value> {
        self.call_owned_scoped(
            method,
            args,
            session,
            maintenance,
            attempted,
            CallAdmissions {
                webmcp: admission,
                ..Default::default()
            },
        )
    }
    fn call_beforeunload(
        &mut self,
        method: &str,
        args: Value,
        session: Option<&str>,
        operation: &beforeunload::Operation<'_>,
    ) -> Result<Value> {
        self.call_owned_scoped(
            method,
            args,
            session,
            false,
            None,
            CallAdmissions {
                beforeunload: Some(operation),
                ..Default::default()
            },
        )
    }
    fn call_owned_scoped(
        &mut self,
        method: &str,
        mut args: Value,
        session: Option<&str>,
        maintenance: bool,
        attempted: Option<&mut bool>,
        admissions: CallAdmissions<'_, '_>,
    ) -> Result<Value> {
        self.iab_cdp_guard(method, &mut args, session)?;
        if !maintenance && let Some(session) = session {
            let owner = self
                .surface
                .dialogs
                .lock()
                .unwrap()
                .modal_tab_for_session(session)
                .map(str::to_owned)
                .or_else(|| {
                    self.sessions
                        .iter()
                        .find(|(_, value)| value.as_str() == session)
                        .map(|(tab, _)| tab.clone())
                })
                .or_else(|| self.surface.frame_session_tab(session));
            if let Some(tab) = owner {
                self.check_dialog_method(&tab, method)?;
            }
        }
        // A failed connection cannot have dispatched this command. Establish
        // the channel first; only then persist intent before attempting send.
        self.client()?;
        self.durable_before(method)?;
        let result = (|| {
            if let Some(admission) = admissions.webmcp {
                self.validate_webmcp_context(admission)?;
            }
            if let Some(attempted) = attempted {
                // Guards and durable intent succeeded. The transport may now have
                // changed provider state even when it returns an error.
                *attempted = true;
            }
            self.client
                .as_mut()
                .unwrap()
                .call_scoped(method, args, session, admissions)
        })();
        if let Some(client) = self.client.as_mut() {
            for (event, internal) in client.drain_events() {
                self.iab_event(&event);
                self.contract.event(&event);
                self.surface.event(event, &mut self.sessions, internal);
            }
        }
        if result.as_ref().is_err_and(|e| e.code == -32006) {
            self.client = None;
            self.sessions.clear();
            self.invalidated = true;
            if let Some(authority) = &mut self.iab {
                authority.invalidate_inputs();
            }
            self.surface.disconnect();
            self.contract.disconnect();
        }
        self.durable_after(method, &result)?;
        if result.is_ok() {
            let _ = self.maintain_child_choosers();
            let _ = self.download_maintenance();
        }
        result
    }
    fn session(&mut self, tab: &str) -> Result<String> {
        if let Some(authority) = &self.iab {
            authority.require_tab(tab)?;
        }
        if let Some(s) = self.sessions.get(tab).cloned() {
            self.attach_download_interception(tab, &s)?;
            return Ok(s);
        }
        let response = self.call(
            "Target.attachToTarget",
            json!({"targetId":tab,"flatten":true}),
            None,
        )?;
        let session = string(&response, "sessionId")?.to_owned();
        self.sessions.insert(tab.into(), session.clone());
        self.surface
            .raw_events
            .lock()
            .unwrap()
            .attach(tab, &session)?;
        self.surface.dialogs.lock().unwrap().root(tab, &session);
        self.surface.resolve_children(&self.sessions);
        self.call("Accessibility.enable", json!({}), Some(&session))?;
        self.attach_download_interception(tab, &session)?;
        Ok(session)
    }
    fn tab(&mut self, tab: &str, method: &str, args: Value) -> Result<Value> {
        let session = self.command_session(tab, method)?;
        self.call(method, args, Some(&session))
    }
    fn command_session(&mut self, tab: &str, method: &str) -> Result<String> {
        self.check_dialog_method(tab, method)?;
        if method == "Page.handleJavaScriptDialog"
            && self.surface.dialogs.lock().unwrap().contains_key(tab)
        {
            // A remembered handler already has an owned route. Closing it must
            // not first enable an unrelated interceptor or attach a new route.
            return self
                .sessions
                .get(tab)
                .cloned()
                .ok_or_else(|| Error::action("JavaScript dialog session is no longer active"));
        }
        self.session(tab)
    }
    fn tab_attempt(
        &mut self,
        tab: &str,
        method: &str,
        args: Value,
        attempted: &mut bool,
    ) -> Result<Value> {
        let session = self.command_session(tab, method)?;
        self.call_owned(method, args, Some(&session), false, Some(attempted))
    }
    /// Cleanup never attaches a fresh session or revives a retired route.
    fn tab_maintenance(&mut self, tab: &str, method: &str, args: Value) -> Result<Value> {
        self.ensure_context()?;
        if let Some(authority) = &self.iab {
            authority.require_tab(tab)?;
        }
        let session = self
            .sessions
            .get(tab)
            .cloned()
            .ok_or_else(|| Error::action("Owned cleanup session is no longer attached"))?;
        self.call_maintenance(method, args, Some(&session))
    }
    fn snapshot(&mut self, tab: &str) -> Result<Node> {
        let result = self.tab(tab, "Accessibility.getFullAXTree", json!({}))?;
        let nodes = result["nodes"]
            .as_array()
            .ok_or_else(|| Error::action("CDP AX tree missing"))?;
        if nodes.len() > 10000 {
            return Err(Error::action("CDP AX node limit exceeded"));
        }
        let map: BTreeMap<String, &Value> = nodes
            .iter()
            .filter_map(|n| n["nodeId"].as_str().map(|id| (id.into(), n)))
            .collect();
        let root = nodes
            .iter()
            .find(|n| n.get("parentId").is_none())
            .and_then(|n| n["nodeId"].as_str())
            .ok_or_else(|| Error::action("No CDP AX root"))?;
        build_node(root, &map, &mut vec![], 0)
    }
    fn element(&mut self, tab: &str, node: &Node) -> Result<String> {
        let backend = node
            .identifier
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| Error::action("No DOM node for AX target"))?;
        let value = self.tab(tab, "DOM.resolveNode", json!({"backendNodeId":backend}))?;
        Ok(string(&value["object"], "objectId")?.into())
    }
    fn call_element(
        &mut self,
        tab: &str,
        node: &Node,
        function: &str,
        args: Vec<Value>,
    ) -> Result<Value> {
        let object = self.element(tab, node)?;
        let arguments: Vec<_> = args.into_iter().map(|v| json!({"value":v})).collect();
        let result=self.tab(tab,"Runtime.callFunctionOn",json!({"objectId":object,"functionDeclaration":function,"arguments":arguments,"returnByValue":true,"awaitPromise":true,"userGesture":true}));
        if !result.as_ref().is_err_and(|e| e.code == -32006) {
            let _ = self.tab_maintenance(tab, "Runtime.releaseObject", json!({"objectId":object}));
        }
        let result = result?;
        if result.get("exceptionDetails").is_some() {
            return Err(Error::action(result["exceptionDetails"].to_string()));
        }
        Ok(result["result"]["value"].clone())
    }
}
#[derive(Default)]
pub struct Browsers {
    providers: BTreeMap<String, Browser>,
    revisions: Sessions,
    current_host_route: Option<String>,
    host_managed: bool,
    chooser_owner: Option<chooser::Owner>,
    download_security: crate::security::Security,
    activation: crate::browser_activation::Owner,
}
impl Browsers {
    /// Native authorization snapshot. The provider/session identity is never
    /// obtained from a renderer or accepted as authority in JavaScript arguments.
    pub(crate) fn authorization_context(&mut self, args: &Value) -> Result<Value> {
        self.execute("info", args)?;
        let mut context = if args["tab"].is_string() {
            self.execute("document_context", args)?
        } else {
            json!({})
        };
        let browser = self
            .providers
            .get_mut(string(args, "browser")?)
            .ok_or_else(|| Error::action("Browser not found"))?;
        browser.client()?;
        context["providerConnection"] = json!(browser.authorization_connection);
        if let Some(tab) = args["tab"].as_str() {
            context["providerSession"] = json!(
                browser
                    .sessions
                    .get(tab)
                    .ok_or_else(|| Error::action("Browser session changed"))?
            );
        }
        Ok(context)
    }

    pub fn register(&mut self, id: &str, endpoint: &str) -> Result<()> {
        let url = url::Url::parse(endpoint).map_err(|_| Error::invalid("Invalid CDP URL"))?;
        if url.scheme() != "ws" {
            return Err(Error::invalid("This build supports ws:// CDP endpoints"));
        }
        if self.providers.contains_key(id) {
            return Err(Error::invalid("Browser ID already registered"));
        }
        let mut surface = surface::Surface::default();
        surface.choosers.owner = self.chooser_owner.clone();
        surface.downloads.lock().unwrap().owner = self.chooser_owner.clone();
        surface.downloads.lock().unwrap().policy = self.download_security.clone();
        self.providers.insert(
            id.into(),
            Browser {
                endpoint: endpoint.into(),
                extension: url
                    .query_pairs()
                    .any(|(key, value)| key == "skyre-provider" && value == "extension"),
                extension_started: false,
                iab: None,
                iab_host: Default::default(),
                durable: None,
                host_binding: None,
                client: None,
                authorization_connection: String::new(),
                sessions: BTreeMap::new(),
                name: id.into(),
                invalidated: false,
                surface,
                contract: contract::State::default(),
            },
        );
        Ok(())
    }
    /// Register an independently owned IAB renderer route. The host supplies
    /// current turn metadata separately; model command fields cannot select it.
    pub fn register_iab(
        &mut self,
        id: &str,
        endpoint: &str,
        route: iab::RouteConfig,
    ) -> Result<()> {
        let authority = iab::Authority::new(route)?;
        self.register(id, endpoint)?;
        let browser = self.providers.get_mut(id).unwrap();
        if browser.extension {
            self.providers.remove(id);
            return Err(Error::invalid(
                "An extension endpoint cannot be an IAB renderer",
            ));
        }
        browser.iab = Some(authority);
        Ok(())
    }
    pub fn set_iab_context(
        &mut self,
        id: &str,
        metadata: Option<&Value>,
    ) -> Result<iab::SessionParams> {
        let browser = self
            .providers
            .get_mut(id)
            .ok_or_else(|| Error::action("IAB route not found"))?;
        let authority = browser
            .iab
            .as_mut()
            .ok_or_else(|| Error::action("IAB route not found"))?;
        let previous = authority
            .context()
            .ok()
            .map(|context| (context.session_id.clone(), context.turn_id.clone()));
        let context = authority.set_context(metadata)?;
        if previous.as_ref() != Some(&(context.session_id.clone(), context.turn_id.clone())) {
            browser.cancel_raw_waits();
        }
        browser.durable_save()?;
        Ok(context)
    }
    pub fn set_iab_route_available(&mut self, id: &str, available: bool) -> Result<()> {
        let browser = self
            .providers
            .get_mut(id)
            .ok_or_else(|| Error::action("IAB route not found"))?;
        let authority = browser
            .iab
            .as_mut()
            .ok_or_else(|| Error::action("IAB route not found"))?;
        let generation = authority.generation;
        authority.set_route_available(available);
        if authority.generation != generation {
            browser.cancel_raw_waits();
        }
        Ok(())
    }
    pub fn iab_diagnostics(&self, id: &str) -> Result<Value> {
        let browser = self
            .providers
            .get(id)
            .filter(|browser| browser.iab.is_some())
            .ok_or_else(|| Error::action("IAB route not found"))?;
        Ok(
            json!({"messages":browser.iab_host.diagnostics,"pending":browser.iab.as_ref().unwrap().pending()}),
        )
    }
    /// Called only when the host connection authority scope actually ends.
    /// Each provider reports failures; disconnect alone never fabricates success.
    pub fn end_session(&mut self) -> Vec<(String, Error)> {
        self.cancel_all_raw_waits();
        let mut errors = Vec::new();
        // Fetched registrations never cross native client connections, even
        // when a plain CDP transport remains connected or cleanup later fails.
        for browser in self.providers.values_mut() {
            browser.surface.webmcp.lock().unwrap().clear_fetched();
        }
        self.current_host_route = None;
        for (id, browser) in &mut self.providers {
            browser.cancel_choosers(None);
            if let Err(error) = browser.shutdown_downloads(None) {
                errors.push((id.clone(), error));
            }
            if browser.host_binding.is_some() {
                browser.client = None;
                browser.sessions.clear();
                browser.surface.disconnect();
                browser.contract.disconnect();
                if let Err(error) = browser.durable_save() {
                    errors.push((id.clone(), error));
                }
                continue;
            }
            if browser.iab.is_some() {
                for error in browser.end_iab_session() {
                    errors.push((id.clone(), error));
                }
            }
            if browser.extension && browser.extension_started {
                if let Err(error) = browser.call("Skyre.turnEnded", json!({}), None) {
                    errors.push((id.clone(), error));
                }
                browser.client = None;
                browser.extension_started = false;
                browser.sessions.clear();
                browser.surface.disconnect();
                browser.contract.disconnect();
            }
        }
        errors
    }
    pub fn capabilities(&self) -> Vec<&'static str> {
        vec![
            "explicit-cdp",
            "list",
            "list_tabs",
            "new_tab",
            "close_tab",
            "navigate",
            "reload",
            "snapshot",
            "screenshot",
            "click",
            "drag",
            "scroll",
            "type_text",
            "paste",
            "press_key",
            "set_value",
            "select_text",
            "evaluate",
            "events",
            "locator",
            "frames",
            "dialogs",
            "clipboard",
            "downloads",
            "exports",
            "back",
            "forward",
            "viewport",
            "dom",
            "file-chooser",
            "cdp",
            "webmcp",
        ]
    }
    pub fn execute(&mut self, method: &str, args: &Value) -> Result<Value> {
        let (method, args) = Self::normalize_request(method, args)?;
        self.execute_normalized(&method, &args)
    }
    /// Resolve aliases, nested action envelopes, and selector frame bindings
    /// before a caller applies origin or command policy to a browser request.
    pub fn normalize_request(method: &str, args: &Value) -> Result<(String, Value)> {
        surface::normalize(method, args)
    }
    /// Dispatch the exact canonical request already checked by the host policy.
    /// Do not normalize a checked request a second time: aliases may be nested.
    pub fn execute_normalized(&mut self, method: &str, args: &Value) -> Result<Value> {
        self.execute_normalized_with_raw_admission(method, args, None)
    }
    fn execute_normalized_with_raw_admission(
        &mut self,
        method: &str,
        args: &Value,
        raw_admission: Option<raw_wait_host::StartAdmission>,
    ) -> Result<Value> {
        self.execute_normalized_admitted(method, args, raw_admission, None, None)
    }
    pub(crate) fn execute_beforeunload_admitted(
        &mut self,
        method: &str,
        args: &Value,
        automatic: &beforeunload::RuntimeAdmission,
    ) -> Result<Value> {
        self.execute_normalized_admitted(method, args, None, Some(automatic), None)
    }
    pub(crate) fn execute_webmcp_admitted(
        &mut self,
        method: &str,
        args: &Value,
        model: &crate::browser_activation::Model,
    ) -> Result<Value> {
        self.execute_normalized_admitted(method, args, None, None, Some(model))
    }
    fn execute_normalized_admitted(
        &mut self,
        method: &str,
        args: &Value,
        raw_admission: Option<raw_wait_host::StartAdmission>,
        automatic: Option<&beforeunload::RuntimeAdmission>,
        model: Option<&crate::browser_activation::Model>,
    ) -> Result<Value> {
        let mut args = args.clone();
        self.prepare_webmcp(method, &mut args)?;
        if let Some(command) = crate::browser_activation::command(method) {
            self.check_webmcp_activation(&args, model.unwrap_or(&Default::default()), command)?;
        }
        let result = self.execute_inner_admitted(method, &args, raw_admission, automatic);
        for browser in self.providers.values_mut() {
            browser.durable_save()?;
        }
        result
    }
    fn check_webmcp_activation(
        &mut self,
        args: &Value,
        model: &crate::browser_activation::Model,
        command: &str,
    ) -> Result<()> {
        let (id, browser) = if let Some(id) = args["browser"].as_str() {
            (
                id,
                self.providers
                    .get(id)
                    .ok_or_else(|| Error::action("Browser not found"))?,
            )
        } else {
            self.providers
                .iter()
                .find(|(_, browser)| self.host_visible(browser))
                .map(|(id, browser)| (id.as_str(), browser))
                .ok_or_else(|| {
                    Error::action(
                        "No browser is available; configure --cdp with an owned debugging endpoint",
                    )
                })?
        };
        if !self.host_visible(browser) {
            return Err(Error::new(
                -32003,
                "Browser route is outside the current trusted host context",
            ));
        }
        browser.ensure_context()?;
        if let Some(authority) = &browser.iab {
            authority.require_tab(string(args, "tab")?)?;
        }
        // Metadata is already owned locally. This does not connect, discover,
        // evaluate JavaScript, or promote protocol support to advertisement.
        let info = browser.metadata(id);
        self.activation.admit(model, &info, command)
    }
    fn execute_inner(&mut self, method: &str, args: &Value) -> Result<Value> {
        self.execute_inner_with_raw_admission(method, args, None)
    }
    fn execute_inner_with_raw_admission(
        &mut self,
        method: &str,
        args: &Value,
        raw_admission: Option<raw_wait_host::StartAdmission>,
    ) -> Result<Value> {
        self.execute_inner_admitted(method, args, raw_admission, None)
    }
    fn execute_inner_admitted(
        &mut self,
        method: &str,
        args: &Value,
        raw_admission: Option<raw_wait_host::StartAdmission>,
        automatic: Option<&beforeunload::RuntimeAdmission>,
    ) -> Result<Value> {
        let automatic =
            automatic.filter(|_| !self.host_managed && self.current_host_route.is_none());
        if let Some(id) = args["browser"].as_str()
            && let Some(browser) = self.providers.get(id)
            && !self.host_visible(browser)
        {
            return Err(Error::new(
                -32003,
                "Browser route is outside the current trusted host context",
            ));
        }
        if method == "get_documentation"
            || method == "documentation"
                && (args.get("name").is_some() || args.get("browser").is_none())
        {
            let docs: Value = serde_json::from_str(include_str!("browser_docs.json"))?;
            let name = args["name"].as_str();
            return name
                .and_then(|name| docs.get(name))
                .cloned()
                .ok_or_else(|| {
                    Error::action(format!(
                        "Documentation is not available: {}",
                        name.map(str::to_owned).unwrap_or_else(|| args
                            .get("name")
                            .map(Value::to_string)
                            .unwrap_or("undefined".into()))
                    ))
                });
        }

        if method == "list" {
            for browser in self
                .providers
                .values()
                .filter(|browser| self.host_visible(browser))
            {
                browser.ensure_context()?;
            }
            return Ok(json!(
                self.providers
                    .iter()
                    .filter(|(_, b)| self.host_visible(b))
                    .map(|(id, b)| b.metadata(id))
                    .collect::<Vec<_>>()
            ));
        }
        if method == "get_default_browser" || method == "get_browser_for_url" {
            if method == "get_browser_for_url" {
                url::Url::parse(string(args, "url")?)
                    .map_err(|_| Error::invalid("Invalid browser URL"))?;
            }
            let (id, browser) = self
                .providers
                .iter()
                .find(|(_, browser)| self.host_visible(browser))
                .ok_or_else(|| Error::action("No browser is available."))?;
            browser.ensure_context()?;
            return Ok(browser.metadata(id));
        }
        if method == "documentation" {
            let id = string(args, "browser")?;
            let provider = self
                .providers
                .get(id)
                .ok_or_else(|| Error::action("Browser not found"))?;
            provider.ensure_context()?;
            return contract::documentation(&provider.metadata(id));
        }
        if method == "info" || method == "get_browser" {
            let id = string(args, "browser")?;
            let browser = self
                .providers
                .get(id)
                .ok_or_else(|| Error::action("Browser not found"))?;
            browser.ensure_context()?;
            return Ok(browser.metadata(id));
        }
        if method == "ax_capture" {
            let content = args["content"].as_str().unwrap_or("axState");
            if !["axState", "screenshot", "axStateAndScreenshot"].contains(&content) {
                return Err(Error::invalid("Invalid AX capture content"));
            }
            let mut result = json!({});
            if content != "screenshot" {
                result["state"] = self.execute_inner("snapshot", args)?["state"].clone();
            }
            if content != "axState" {
                match self.execute_inner("screenshot", args) {
                    Ok(image) => result["data"] = image["data"].clone(),
                    Err(error) if content == "axStateAndScreenshot" => {
                        result["screenshot_unavailable"] = json!(error.message)
                    }
                    Err(error) => return Err(error),
                }
            }
            return Ok(result);
        }
        // Validate action options before focusing or scrolling a DOM target.
        match method {
            "click" => {
                if let Some(count) = args.get("clickCount").or_else(|| args.get("click_count"))
                    && !count.as_u64().is_some_and(|n| (1..=3).contains(&n))
                {
                    return Err(Error::invalid("clickCount must be an integer in 1..3"));
                }
                if let Some(button) = args.get("mouseButton").or_else(|| args.get("mouse_button"))
                    && !button.as_str().is_some_and(|s| {
                        ["left", "right", "middle", "back", "forward"].contains(&s)
                    })
                {
                    return Err(Error::invalid("Invalid mouseButton"));
                }
            }
            "scroll" => {
                let d = string(args, "direction")?;
                if !["up", "down", "left", "right"].contains(&d) {
                    return Err(Error::invalid("Invalid scroll direction"));
                }
                if let Some(pages) = args.get("pages")
                    && !pages.as_f64().is_some_and(|p| p > 0. && p <= 100.)
                {
                    return Err(Error::invalid("pages must be in (0,100]"));
                }
            }
            _ => {}
        }
        let id = args["browser"]
            .as_str()
            .or_else(|| {
                self.providers
                    .iter()
                    .find(|(_, browser)| self.host_visible(browser))
                    .map(|(id, _)| id.as_str())
            })
            .ok_or_else(|| {
                Error::action(
                    "No browser is available; configure --cdp with an owned debugging endpoint",
                )
            })?
            .to_owned();
        let b = self
            .providers
            .get_mut(&id)
            .ok_or_else(|| Error::action("Browser not found"))?;
        if b.invalidated {
            let prefix = format!("{id}:");
            self.revisions
                .revisions
                .retain(|k, _| !k.starts_with(&prefix));
            b.invalidated = false;
        }
        if method == "close_tab" {
            b.check_dialog_method(string(args, "tab")?, "Target.getTargets")?;
        }
        if let Some(result) = b.iab_command(method, args)? {
            return Ok(result);
        }
        if let Some(result) = b.contract_command(method, args)? {
            return Ok(result);
        }
        if method == "cdp_events"
            && let Some(admission) = raw_admission
        {
            return b.raw_events_start(args, Some(admission));
        }
        if let Some(result) = b.surface_command(method, args)? {
            return Ok(result);
        }
        match method {
            "begin_turn" | "end_turn" if b.extension => {
                b.cancel_choosers(None);
                b.shutdown_downloads(None)?;
                let result = b.call(
                    if method == "begin_turn" {
                        "Skyre.beginTurn"
                    } else {
                        "Skyre.turnEnded"
                    },
                    json!({}),
                    None,
                )?;
                b.sessions.clear();
                b.surface.disconnect();
                b.contract.disconnect();
                return Ok(result);
            }
            "name_session" => {
                let name = string(args, "name")?.trim();
                if name.is_empty() {
                    return Err(Error::invalid("Session name is empty"));
                }
                if b.extension {
                    b.call("Skyre.nameSession", json!({"name":name}), None)?;
                }
                b.name = name.into();
                return Ok(Value::Null);
            }
            "list_tabs" => {
                return Ok(b.call("Target.getTargets", json!({}), None)?["targetInfos"].clone());
            }
            "new_tab" => {
                // The first non-blank response must not race attachment of the
                // owned Document interceptor, including for host-only URLs.
                let created = b.call("Target.createTarget", json!({"url":"about:blank"}), None)?;
                let tab = string(&created, "targetId")?;
                let initialized = (|| -> Result<()> {
                    b.page(tab)?;
                    if let Some(url) = args["url"].as_str().filter(|url| *url != "about:blank") {
                        b.navigate_url(tab, url)?;
                    }
                    if let Some(mut viewport) = b.contract.viewport.clone() {
                        viewport["tab"] = created["targetId"].clone();
                        b.surface_command("viewport_set", &viewport)?;
                    }
                    Ok(())
                })();
                if let Err(mut error) = initialized {
                    // Only this acknowledged, newly created target is ours to
                    // close. Never repeat its failed navigation.
                    match b.call("Target.closeTarget", json!({"targetId":tab}), None) {
                        Ok(result) if result["success"] != false => {
                            b.surface.remove_tab(tab);
                            b.contract.remove_tab(tab);
                            b.sessions.remove(tab);
                        }
                        Ok(_) => error
                            .message
                            .push_str("; cleanup of the newly created target was refused"),
                        Err(cleanup) => error.message.push_str(&format!(
                            "; cleanup of the newly created target failed: {}",
                            cleanup.message
                        )),
                    }
                    return Err(error);
                }
                return Ok(created);
            }
            _ => {}
        }
        let tab = string(args, "tab")?;
        let key = format!("{id}:{tab}");
        let automatic = automatic.and_then(|runtime| b.beforeunload_operation(&id, tab, runtime));
        match method {
            "close_tab" => {
                b.cancel_downloads(Some(tab), None, None);
                b.download_maintenance()?;
                let r = match automatic
                    .as_ref()
                    .filter(|operation| operation.kind() == beforeunload::Kind::Close)
                {
                    Some(operation) => b.call_beforeunload(
                        "Target.closeTarget",
                        json!({"targetId":tab}),
                        None,
                        operation,
                    )?,
                    None => b.call("Target.closeTarget", json!({"targetId":tab}), None)?,
                };
                if r["success"] == false {
                    return Err(Error::action("Browser did not close the target"));
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let targets = match automatic
                        .as_ref()
                        .filter(|operation| operation.kind() == beforeunload::Kind::Close)
                    {
                        Some(operation) => {
                            b.call_beforeunload("Target.getTargets", json!({}), None, operation)?
                        }
                        None => b.call("Target.getTargets", json!({}), None)?,
                    };
                    if !targets["targetInfos"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|target| target["targetId"] == tab)
                    {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::action("Target did not finish closing"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }

                b.surface.remove_tab(tab);
                b.contract.remove_tab(tab);
                b.sessions.remove(tab);
                self.revisions.revisions.remove(&key);
                Ok(r)
            }
            "navigate" => b.navigate_url_scoped(tab, string(args, "url")?, automatic.as_ref()),
            "reload" => b.tab(tab, "Page.reload", json!({})),
            "snapshot" => {
                let root = b.snapshot(tab)?;
                let (state, revision) = self.revisions.observe(
                    &key,
                    root,
                    args["disableDiffing"].as_bool().unwrap_or(false),
                )?;
                Ok(json!({"state":state,"tree":revision.root,"revision":revision.generation}))
            }
            "screenshot" => {
                if args.get("clip").is_none_or(Value::is_null)
                    && args["fullPage"] != true
                    && let Some(data) = b.viewport_screencast(tab)
                {
                    return Ok(json!({"mime_type":"image/png","data":data}));
                }
                let mut params = json!({"format":"png","captureBeyondViewport":false});
                if let Some(clip) = args.get("clip").filter(|v| !v.is_null()) {
                    for key in ["x", "y", "width", "height"] {
                        if !clip[key].as_f64().is_some_and(|n| {
                            n.is_finite()
                                && if key == "width" || key == "height" {
                                    n > 0.
                                } else {
                                    n >= 0.
                                }
                        }) {
                            return Err(Error::invalid("Invalid screenshot clip"));
                        }
                    }
                    params["clip"] = clip.clone();
                    params["clip"]["scale"] = json!(1);
                    params["captureBeyondViewport"] = json!(true);
                } else if args["fullPage"] == true {
                    let metrics = b.tab(tab, "Page.getLayoutMetrics", json!({}))?;
                    let size = metrics
                        .get("cssContentSize")
                        .or_else(|| metrics.get("contentSize"))
                        .ok_or_else(|| Error::action("Full-page dimensions unavailable"))?;
                    params["clip"] = json!({"x":size["x"],"y":size["y"],"width":size["width"],"height":size["height"],"scale":1});
                    params["captureBeyondViewport"] = json!(true);
                }
                let r = b.tab(tab, "Page.captureScreenshot", params)?;
                Ok(json!({"mime_type":"image/png","data":r["data"]}))
            }
            "evaluate" => b.evaluate(tab, string(args, "expression")?, args),
            "events" => Ok(json!(
                b.surface
                    .events
                    .iter()
                    .map(|(_, event)| event)
                    .collect::<Vec<_>>()
            )),
            "type_text" | "paste" => {
                if method == "paste" && args.get("format").is_some_and(|f| f != "text") {
                    return Err(Error::unsupported(
                        "CDP paste currently supports plain text",
                    ));
                }
                b.tab(
                    tab,
                    "Input.insertText",
                    json!({"text":string(args,"text")?}),
                )
            }
            "press_key" => b.key(tab, string(args, "key")?),
            "set_value" | "select_text" | "click" | "scroll" => {
                let node = if args.get("element_index").is_some() {
                    let root = b.snapshot(tab)?;
                    Some(self.revisions.resolve(&key, index(args)?, root, false)?)
                } else {
                    None
                };
                if method == "set_value" {
                    let target = node.ok_or_else(|| Error::invalid("element_index required"))?;
                    let value = string(args, "value")?;
                    let state = b.call_element(
                        tab,
                        &target,
                        include_str!("browser_ax_value.js"),
                        vec![json!(value)],
                    )?;
                    if state == "needs-click" {
                        // Use the existing native AX pointer path, then verify
                        // selection on the same backend node rather than assume
                        // a click changed an ARIA tab's state.
                        let mut click = args.clone();
                        click["clickCount"] = json!(1);
                        click["mouseButton"] = json!("left");
                        self.execute_inner("click", &click)?;
                        let browser = self
                            .providers
                            .get_mut(&id)
                            .ok_or_else(|| Error::action("Browser not found"))?;
                        if browser.call_element(
                            tab,
                            &target,
                            include_str!("browser_ax_value.js"),
                            vec![json!(value)],
                        )? != "done"
                        {
                            return Err(Error::action("Tab did not become selected"));
                        }
                    } else if state != "done" {
                        return Err(Error::action("Accessibility value action did not complete"));
                    }
                    return Ok(Value::Null);
                }
                if method == "select_text" {
                    let n = node.ok_or_else(|| Error::invalid("element_index required"))?;
                    let mode = serde_json::from_value(
                        args.get("selectionType")
                            .or_else(|| args.get("selection"))
                            .cloned()
                            .unwrap_or(json!("text")),
                    )
                    .map_err(|_| Error::invalid("Invalid selection mode"))?;
                    let range = selection::select(
                        n.value.as_deref().unwrap_or_default(),
                        string(args, "text")?,
                        args["prefix"].as_str(),
                        args["suffix"].as_str(),
                        mode,
                    )?;
                    return b.call_element(tab,&n,"function(start,end){this.focus();if(!this.setSelectionRange)throw new Error('Selection unsupported');this.setSelectionRange(start,end);}",vec![json!(range.location),json!(range.location+range.length)]);
                }
                let p = if let Some(n) = node {
                    let result=b.call_element(tab,&n,"function(){this.scrollIntoView({block:'center',inline:'center'});const r=this.getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2];}",vec![])?;
                    point(&json!({"point":result}), "point", "x", "y")?
                } else {
                    point(args, "point", "x", "y")?
                };
                if method == "scroll" {
                    let pages = args
                        .get("pages")
                        .map(|v| {
                            v.as_f64()
                                .ok_or_else(|| Error::invalid("pages must be a number"))
                        })
                        .transpose()?
                        .unwrap_or(1.0);
                    if !(0.0..=100.0).contains(&pages) || pages == 0.0 {
                        return Err(Error::invalid("pages must be in (0,100]"));
                    }
                    let direction = string(args, "direction")?;
                    let (dx, dy) = match direction {
                        "down" => (0.0, 600.0 * pages),
                        "up" => (0.0, -600.0 * pages),
                        "left" => (-600.0 * pages, 0.0),
                        "right" => (600.0 * pages, 0.0),
                        _ => return Err(Error::invalid("Invalid direction")),
                    };
                    return b.tab(
                        tab,
                        "Input.dispatchMouseEvent",
                        json!({"type":"mouseWheel","x":p[0],"y":p[1],"deltaX":dx,"deltaY":dy}),
                    );
                }
                let count = args
                    .get("clickCount")
                    .or_else(|| args.get("click_count"))
                    .map(|v| {
                        v.as_u64()
                            .ok_or_else(|| Error::invalid("clickCount must be an integer"))
                    })
                    .transpose()?
                    .unwrap_or(1);
                if !(1..=3).contains(&count) {
                    return Err(Error::invalid("clickCount must be 1..3"));
                }
                let button = args
                    .get("mouseButton")
                    .or_else(|| args.get("mouse_button"))
                    .map(|v| {
                        v.as_str()
                            .ok_or_else(|| Error::invalid("mouseButton must be a string"))
                    })
                    .transpose()?
                    .unwrap_or("left");
                if !["left", "middle", "right"].contains(&button) {
                    return Err(Error::invalid("Invalid mouseButton"));
                }
                b.tab(tab,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":p[0],"y":p[1],"button":button,"clickCount":count}))?;
                b.tab_maintenance(tab,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":p[0],"y":p[1],"button":button,"clickCount":count}))
            }
            "drag" => {
                let from = point(args, "from", "from_x", "from_y")?;
                let to = point(args, "to", "to_x", "to_y")?;
                b.tab(tab,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":from[0],"y":from[1],"button":"left","buttons":1,"clickCount":1}))?;
                let mut last = from;
                let result = (|| -> Result<()> {
                    for step in 1..=10 {
                        let t = step as f64 / 10.0;
                        last = [
                            from[0] + (to[0] - from[0]) * t,
                            from[1] + (to[1] - from[1]) * t,
                        ];
                        b.tab(tab,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":last[0],"y":last[1],"button":"left","buttons":1}))?;
                    }
                    Ok(())
                })();
                if let Err(error) = &result
                    && error.code == -32006
                {
                    return Err(error.clone());
                }
                let release = b.tab_maintenance(tab,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":last[0],"y":last[1],"button":"left","buttons":0,"clickCount":1}));
                result?;
                release
            }
            _ => Err(Error::unsupported(format!(
                "Browser method unavailable: {method}"
            ))),
        }
    }
}
fn build_node(
    id: &str,
    map: &BTreeMap<String, &Value>,
    ancestors: &mut Vec<String>,
    depth: usize,
) -> Result<Node> {
    if depth > 100 || ancestors.iter().any(|x| x == id) {
        return Err(Error::action("Cyclic or over-depth CDP AX tree"));
    }
    let n = map
        .get(id)
        .ok_or_else(|| Error::action("Missing CDP AX node"))?;
    ancestors.push(id.into());
    let property = |name: &str| {
        n["properties"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["name"] == name))
            .map(|p| &p["value"]["value"])
    };
    let children = n["childIds"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(|id| build_node(id, map, ancestors, depth + 1))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    ancestors.pop();
    Ok(Node {
        identity: id.into(),
        role: n["role"]["value"].as_str().unwrap_or("group").into(),
        title: n["name"]["value"].as_str().map(String::from),
        value: n
            .get("value")
            .and_then(|v| v["value"].as_str())
            .map(String::from),
        identifier: n["backendDOMNodeId"].as_u64().map(|v| v.to_string()),
        enabled: property("disabled").and_then(Value::as_bool) != Some(true),
        focused: property("focused").and_then(Value::as_bool) == Some(true),
        settable: property("editable").is_some(),
        children,
        ..Default::default()
    })
}

#[cfg(test)]
mod request_id_tests {
    use super::allocate_request_id;

    #[test]
    fn cdp_request_ids_preserve_initial_sequence() {
        let mut next = 1;
        let ordinary = allocate_request_id(&mut next).unwrap();
        let callback = allocate_request_id(&mut next).unwrap();
        let following = allocate_request_id(&mut next).unwrap();
        assert_eq!([ordinary, callback, following], [1, 2, 3]);
        assert_eq!(next, 4);
    }

    #[test]
    fn cdp_request_id_exhaustion_never_wraps_or_reuses_an_id() {
        let mut next = u64::MAX - 1;
        assert_eq!(allocate_request_id(&mut next).unwrap(), u64::MAX - 1);
        for _ in 0..3 {
            let error = allocate_request_id(&mut next).unwrap_err();
            assert_eq!(error.message, "CDP request ID space exhausted");
            assert_eq!(next, u64::MAX);
        }
    }
}

#[cfg(test)]
#[path = "browser_webmcp_transport_tests.rs"]
mod webmcp_transport_tests;
