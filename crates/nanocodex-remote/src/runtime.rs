//! Rust Hand screen publication. Credentials and signaling remain on the host.
use crate::Result as CoreResult;
use crate::audio_duplex::SinkFactory;
use crate::diagnostics::{Budget, HttpOutcome, close_reason};
use crate::ice_cache::{IceCache, Response as IceResponse};
use crate::input::{Lease, timely_event};
use crate::preparation::{Preparations, VIEWER_CAPACITY};
use crate::target::PublisherTarget;
use crate::video::{Video, VideoSource, ice_servers};
use futures_util::{SinkExt, StreamExt, future::BoxFuture};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use url::Url;

pub type Backend = Arc<
    dyn Fn(Value) -> BoxFuture<'static, Result<Value, Box<dyn std::error::Error + Send + Sync>>>
        + Send
        + Sync,
>;

/// Host identity published in the surface catalog. Credentials belong to PublisherTarget.
#[derive(Clone)]
pub struct Machine {
    id: String,
    name: String,
}
impl Machine {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> CoreResult<Self> {
        let value = Self {
            id: id.into(),
            name: name.into(),
        };
        if !valid_id(&value.id) || value.name.is_empty() || value.name.len() > 256 {
            return Err(error("invalid publisher machine"));
        }
        Ok(value)
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}
/// Optional host observation enrichment. Capture remains independently bounded.
pub trait Observation: Send + Sync {
    fn valid_context(&self, context: Option<&Value>) -> bool;
    fn collect(
        &self,
        context: Option<Value>,
        captured_at: u64,
        budget: Duration,
    ) -> BoxFuture<'_, Value>;
}
/// Optional broadcast lifecycle. The publisher stops it when authorization expires.
pub trait Broadcast: Send {
    fn supported(&self) -> bool;
    fn request<'a>(&'a mut self, value: &'a Value) -> BoxFuture<'a, Value>;
    fn stop(&mut self) -> BoxFuture<'_, ()>;
    /// Portable HLS playback (catalog `playback: true`), sharing the broadcast slot.
    fn playback(&self) -> bool {
        false
    }
    /// Broker-originated `target:"hls"` command. `origin` is the authenticated
    /// publisher origin; upload URLs must match it exactly.
    fn playback_request<'a>(
        &'a mut self,
        value: &'a Value,
        _origin: &'a url::Origin,
    ) -> BoxFuture<'a, Value> {
        Box::pin(async move {
            json!({"type":"broadcast_result","target":"hls","request_id":value["request_id"],
                "stream_id":value["stream_id"],"status":"failed","error":"unsupported"})
        })
    }
    /// Asynchronous playback status results, already in their exact wire shape.
    fn playback_events(&self) -> Option<watch::Receiver<Value>> {
        None
    }
}
/// Broker HLS command identifiers use the broker's exact ID grammar.
fn valid_playback_id(value: &Value) -> bool {
    value.as_str().is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id.as_bytes()[0].is_ascii_alphanumeric()
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
    })
}
struct NoBroadcast;
impl Broadcast for NoBroadcast {
    fn supported(&self) -> bool {
        false
    }
    fn request<'a>(&'a mut self, _: &'a Value) -> BoxFuture<'a, Value> {
        Box::pin(async { json!({"type":"broadcast_result","status":"unavailable"}) })
    }
    fn stop(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}
pub struct Options {
    /// Required for publication. No JPEG live-view transport is available.
    pub video: Option<VideoSource>,
    pub audio: Option<VideoSource>,
    pub microphone_factory: Option<SinkFactory>,
    pub observation: Option<Arc<dyn Observation>>,
    pub broadcast: Box<dyn Broadcast>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            video: None,
            audio: None,
            microphone_factory: None,
            observation: None,
            broadcast: Box::new(NoBroadcast),
        }
    }
}
pub struct Publisher {
    target: watch::Sender<PublisherTarget>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    initially_replaced: bool,
}
impl Drop for Publisher {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
impl Publisher {
    /// Whether publication has ended, including an authenticated host replacement.
    pub fn is_finished(&self) -> bool {
        self.initially_replaced || self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub async fn start(
        target: &PublisherTarget,
        machine: &Machine,
        backend: Backend,
        options: Options,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let Options {
            video,
            audio,
            microphone_factory,
            mut broadcast,
            observation: providers,
        } = options;
        let video = video.ok_or_else(|| {
            error("Hand live view requires an H.264 video source; JPEG transport is not supported")
        })?;
        endpoint(target)?;
        crate::tls::ensure_crypto_provider();
        let started = Instant::now();
        let first =
            tokio::time::timeout(Duration::from_secs(8), backend(json!({"action":"observe"})))
                .await
                .map_err(|_| error("screen capture timed out"))??;
        if first["status"] != "ok" {
            return Err(error(
                "screen capture is unavailable; check display and OS permissions",
            ));
        }
        tracing::info!(target: "nanocodex2", stage = "screen.capture.initial", machine_id = machine.id(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
        let input_keepalive = first["inputKeepalive"] == true;
        let capabilities = call(
            &backend,
            json!({"action":"capabilities"}),
            Duration::from_secs(2),
        )
        .await;
        let dimensions = (
            first["width"].as_u64().unwrap_or(1280),
            first["height"].as_u64().unwrap_or(720),
        );
        let (sender, mut targets) = watch::channel(target.clone());
        let (stop, mut stopped) = oneshot::channel();
        let (ready, waiting) = oneshot::channel();
        let machine = machine.clone();
        let task = tokio::spawn(async move {
            let mut ready = Some(ready);
            let mut authorized_at = Instant::now();
            loop {
                if authorized_at.elapsed() > Duration::from_secs(25) {
                    broadcast.stop().await;
                }
                let target = targets.borrow_and_update().clone();
                let session_started = Instant::now();
                let result = tokio::select! {
                    _ = &mut stopped => break,
                    changed = targets.changed() => {
                        if changed.is_err() { break; }
                        broadcast.stop().await;
                        let _ = call(&backend, json!({"action":"release"}), Duration::from_secs(3)).await;
                        continue;
                    },
                    result = session(&target, &machine, &backend, &video, audio.as_ref(), microphone_factory.clone(), dimensions, &capabilities, input_keepalive, &mut ready, &providers, &mut broadcast, &mut authorized_at) => result,
                };
                if let Err(error) = &result {
                    tracing::warn!(target: "nanocodex2", stage = "screen.session.exit", reason = error.category(), http_status = error.http_status(), close_code = error.close_code(), elapsed_ms = session_started.elapsed().as_millis() as u64);
                }
                // Return the terminal handle even if replacement raced initial
                // publication. A supervising owner must not treat this as a
                // retryable startup failure and reclaim the newer host's screen.
                if matches!(result, Err(SessionError::Replaced))
                    && let Some(ready) = ready.take()
                {
                    let _ = ready.send(false);
                }
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    backend(json!({"action":"release"})),
                )
                .await;
                // The scoped request has been dropped; mutable ownership is back
                // here before stopping any resources created during that request.
                broadcast.stop().await;
                if matches!(result, Err(SessionError::Replaced)) {
                    break;
                }
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                    changed = targets.changed() => {
                        if changed.is_err() { break; }
                        if targets.borrow().endpoint() != target.endpoint() { broadcast.stop().await; }
                    },
                }
            }
            broadcast.stop().await;
            let _ =
                tokio::time::timeout(Duration::from_secs(3), backend(json!({"action":"release"})))
                    .await;
        });
        let mut publisher = Self {
            target: sender,
            stop: Some(stop),
            task: Some(task),
            initially_replaced: false,
        };
        match tokio::time::timeout(Duration::from_secs(30), waiting).await {
            Ok(Ok(published)) => {
                // Replacement may win before the first catalog acknowledgment.
                // Return its terminal handle, but never call it a ready screen.
                publisher.initially_replaced = !published;
                Ok(publisher)
            }
            _ => {
                let _ = publisher.shutdown().await;
                Err(error("Hand screen did not publish within 30 seconds"))
            }
        }
    }
    pub async fn refresh(
        &self,
        target: &PublisherTarget,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        endpoint(target)?;
        self.target
            .send(target.clone())
            .map_err(|_| error("Hand screen publisher has stopped"))
    }
    pub async fn shutdown(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }
}
fn endpoint(target: &PublisherTarget) -> Result<Url, Box<dyn std::error::Error + Send + Sync>> {
    Ok(target.endpoint().clone())
}
#[derive(Debug)]
enum SessionError {
    Closed,
    Replaced,
    Unauthorized,
    MediaFailed,
    Renewal(HttpOutcome),
    SocketEnded,
    SocketReadFailed,
    BrokerClosed(u16, &'static str),
}
impl SessionError {
    const fn category(&self) -> &'static str {
        match self {
            Self::Closed => "session_closed",
            Self::Replaced => "host_replaced",
            Self::Unauthorized => "authorization_expired",
            Self::MediaFailed => "media_pipeline_failed",
            Self::Renewal(outcome) => outcome.category,
            Self::SocketEnded => "socket_or_peer_stream_ended",
            Self::SocketReadFailed => "socket_read_failed",
            Self::BrokerClosed(_, reason) => reason,
        }
    }
    const fn http_status(&self) -> Option<u16> {
        match self {
            Self::Renewal(outcome) => outcome.status,
            _ => None,
        }
    }
    const fn close_code(&self) -> Option<u16> {
        match self {
            Self::BrokerClosed(code, _) => Some(*code),
            _ => None,
        }
    }
}
type Wire =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Socket {
    wire: Wire,
    video: Option<Video>,
    microphone: MicrophoneControl,
}
// Mirrors the lease deadline conservatively without extending authorization on input.
#[derive(Default)]
struct MicrophoneControl {
    deadline: Option<Instant>,
    active: Option<Value>,
}
impl MicrophoneControl {
    fn refreshed(&mut self, started: Instant) {
        self.deadline = Some(started + crate::input::LEASE_DURATION);
    }
    fn remaining(&self) -> Duration {
        self.deadline.map_or(Duration::ZERO, |d| {
            d.saturating_duration_since(Instant::now())
        })
    }
    fn request(
        &mut self,
        lease: &Lease,
        viewer: &str,
        data: &Value,
        apply: impl FnOnce(bool, Duration) -> bool,
    ) -> Option<Value> {
        let generation = data["generation"].as_str()?;
        let request = data["requestID"].as_str().filter(|s| valid_id(s))?;
        let enabled = data["enabled"].as_bool()?;
        if !lease.valid(viewer, generation) {
            return None;
        }
        let enabled = apply(enabled, self.remaining());
        let ack = json!({"type":"control","viewer_id":viewer,"data":{
            "type":"microphone","generation":generation,"requestID":request,"enabled":enabled}});
        self.active = enabled.then(|| ack.clone());
        Some(ack)
    }
    fn stopped(&mut self) -> Option<Value> {
        let mut ack = self.active.take()?;
        ack["data"]["enabled"] = json!(false);
        Some(ack)
    }
}
// WebRTC events have already been decoded and validated by the data-channel
// boundary. Keep them structured instead of serializing them into a pretend
// WebSocket message and parsing them again on the latency-sensitive input path.
enum Incoming {
    Broker(Message),
    Peer(Value, Option<Arc<std::sync::atomic::AtomicBool>>),
}
fn broker_event(text: &str, webrtc: bool) -> Result<Option<Value>, SessionError> {
    let value: Value = serde_json::from_str(text).map_err(|_| SessionError::Closed)?;
    // A signaling socket never acquires input authority for a WebRTC viewer.
    if webrtc
        && matches!(
            value["type"].as_str(),
            Some("input" | "control" | "frame_request")
        )
    {
        return Ok(None);
    }
    Ok(Some(value))
}
impl Socket {
    async fn send(
        &mut self,
        message: Message,
    ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        self.wire.send(message).await
    }
    async fn next(&mut self) -> Option<Result<Incoming, tokio_tungstenite::tungstenite::Error>> {
        loop {
            tokio::select! {
                message = self.wire.next() => {
                    return message.map(|result| result.map(Incoming::Broker));
                }
                event = async { match &mut self.video { Some(video) => video.next().await, None => std::future::pending().await } } => {
                    let event = event?;
                    if event.outgoing {
                        if send(self, event.value).await.is_err() { return None; }
                    } else {
                        // A delayed key-up cannot be silently dropped. Revoke
                        // its whole lease instead of replaying stale input.
                        let Some(value) = timely_event(event.value, event.created.elapsed()) else { continue; };
                        return Some(Ok(Incoming::Peer(value, event.active)));
                    }
                }
            }
        }
    }
}
async fn send(socket: &mut Socket, value: Value) -> Result<(), SessionError> {
    if let Some(video) = &mut socket.video {
        if value["type"] == "control" {
            return video
                .control(value["viewer_id"].as_str().unwrap_or(""), &value["data"])
                .await
                .map_err(|_| SessionError::Closed);
        }
        if value["type"] == "close_viewer" {
            video.remove(value["viewer_id"].as_str().unwrap_or(""));
        }
    }
    tokio::time::timeout(
        Duration::from_secs(3),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .map_err(|_| SessionError::Closed)?
    .map_err(|_| SessionError::Closed)
}
struct OwnedJob(JoinHandle<Value>);
impl Drop for OwnedJob {
    fn drop(&mut self) {
        self.0.abort();
    }
}
// Joining cancellation prevents a preempted input future from racing native release.
async fn cancel_job(job: &mut Option<OwnedJob>) -> bool {
    let Some(mut job) = job.take() else {
        return false;
    };
    job.0.abort();
    let _ = (&mut job.0).await;
    true
}
fn broadcast_failure(request: &Value, error: &str) -> Value {
    json!({"type":"broadcast_result","viewer_id":request["viewer_id"],
        "request_id":request["request_id"],"status":"failed","error":error})
}
async fn completed(job: &mut Option<OwnedJob>) -> Value {
    match job {
        Some(job) => (&mut job.0)
            .await
            .unwrap_or_else(|_| json!({"status":"unavailable"})),
        None => std::future::pending().await,
    }
}
fn control_grant(generation: &str, capabilities: &Value) -> Value {
    let mut grant = json!({"type":"granted", "generation":generation});
    if capabilities["status"] == "ok" {
        for name in ["relativePointer", "gamepad"] {
            if capabilities[name] == true {
                grant[name] = json!(true);
            }
        }
    }
    grant
}
// Only an authorized renewal may refresh a guest's held-input fail-safe.
async fn renew_control(
    lease: &mut Lease,
    owner: &str,
    generation: &str,
    backend: &Backend,
    input_keepalive: bool,
) -> bool {
    lease
        .renew_with(owner, generation, async {
            !input_keepalive
                || call(
                    backend,
                    json!({"action":"keepAlive"}),
                    Duration::from_secs(2),
                )
                .await["status"]
                    == "ok"
        })
        .await
}
async fn release(
    lease: &mut Lease,
    backend: &Backend,
    socket: &mut Socket,
) -> Result<(), SessionError> {
    let owner = lease.clear();
    socket.microphone.deadline = None;
    if let Some(video) = &socket.video {
        video.revoke_microphone(&owner);
    }
    let microphone_ack = socket.microphone.stopped();
    // A cleared lease is not proof that the native device released its holds.
    // Do not acknowledge release or grant a new owner after a failed cleanup.
    if call(backend, json!({"action":"release"}), Duration::from_secs(3)).await["status"] != "ok" {
        return Err(SessionError::Closed);
    }
    if let Some(ack) = microphone_ack {
        send(socket, ack).await?;
    }
    if !owner.is_empty() && !owner.starts_with("agent:") {
        send(
            socket,
            json!({"type":"control","viewer_id":owner,"data":{"type":"revoked"}}),
        )
        .await?;
    }
    Ok(())
}
async fn viewer_left(
    viewer: &str,
    lease: &mut Lease,
    backend: &Backend,
    socket: &mut Socket,
) -> Result<(), SessionError> {
    // Retire the dead transport before native lease cleanup. Revocation must
    // not await a control acknowledgement on the failed peer and tear down
    // unrelated viewers.
    if let Some(video) = &mut socket.video {
        video.remove(viewer);
    }
    if lease.owner() == viewer {
        release(lease, backend, socket).await?;
    }
    Ok(())
}
// The session owns these separately borrowed transport and lifecycle resources.
#[allow(clippy::too_many_arguments)]
async fn session(
    target: &PublisherTarget,
    machine: &Machine,
    backend: &Backend,
    video: &VideoSource,
    audio: Option<&VideoSource>,
    microphone_factory: Option<SinkFactory>,
    dimensions: (u64, u64),
    capabilities: &Value,
    input_keepalive: bool,
    ready: &mut Option<oneshot::Sender<bool>>,
    providers: &Option<Arc<dyn Observation>>,
    broadcast: &mut Box<dyn Broadcast>,
    last_authorized: &mut Instant,
) -> Result<(), SessionError> {
    let started = Instant::now();
    let base = endpoint(target).map_err(|_| SessionError::Closed)?;
    let mut host = base.clone();
    host.set_path(&format!("{}/host", base.path()));
    host.set_scheme(if base.scheme() == "https" {
        "wss"
    } else {
        "ws"
    })
    .map_err(|_| SessionError::Closed)?;
    let mut request = host
        .as_str()
        .into_client_request()
        .map_err(|_| SessionError::Closed)?;
    let mut authorization = format!("Bearer {}", target.bearer())
        .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
        .map_err(|_| SessionError::Closed)?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("authorization", authorization);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(750_000))
        .max_frame_size(Some(750_000));
    let (wire, _) = tokio::time::timeout(
        Duration::from_secs(10),
        async {
            // Keep DNS/TCP separate from TLS + HTTP upgrade in startup traces.
            let address = format!("{}:{}", host.host_str().ok_or(SessionError::Closed)?, host.port_or_known_default().ok_or(SessionError::Closed)?);
            let addresses: Vec<_> = tokio::net::lookup_host(address).await.map_err(|_| SessionError::Closed)?.collect();
            tracing::info!(target: "nanocodex2", stage = "screen.socket.resolved", machine_id = machine.id(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
            let stream = tokio::net::TcpStream::connect(addresses.as_slice()).await.map_err(|_| SessionError::Closed)?;
            stream.set_nodelay(true).map_err(|_| SessionError::Closed)?;
            tracing::info!(target: "nanocodex2", stage = "screen.socket.tcp", machine_id = machine.id(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
            let connector = if host.scheme() == "wss" {
                Some(tokio_tungstenite::Connector::Rustls(crate::tls::native_client_config().await.map_err(|_| SessionError::Closed)?))
            } else { None };
            tracing::info!(target: "nanocodex2", stage = "screen.socket.trust", machine_id = machine.id(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
            tokio_tungstenite::client_async_tls_with_config(request, stream, Some(config), connector).await.map_err(|_| SessionError::Closed)
        },
    )
    .await
    .map_err(|_| SessionError::Closed)?
    .map_err(|_| SessionError::Closed)?;
    tracing::info!(target: "nanocodex2", stage = "screen.socket.connected", machine_id = machine.id(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
    let video = Video::start_with_microphone(video, audio, microphone_factory)
        .await
        .map_err(|error| {
            tracing::error!(target: "nanocodex2", stage = "screen.video.start_failed", %error,
                "WebRTC encoder unavailable; retrying video startup (JPEG transport disabled)");
            SessionError::MediaFailed
        })?;
    session_loop(
        target,
        machine,
        backend,
        dimensions,
        capabilities,
        input_keepalive,
        ready,
        providers,
        broadcast,
        last_authorized,
        started,
        Socket {
            wire,
            video: Some(video),
            microphone: MicrophoneControl::default(),
        },
    )
    .await
}
// Transport initialization always requires video. Keeping the event loop separate
// lets lease/cancellation tests exercise failures without an unauthenticated peer.
#[allow(clippy::too_many_arguments)]
async fn session_loop(
    target: &PublisherTarget,
    machine: &Machine,
    backend: &Backend,
    dimensions: (u64, u64),
    capabilities: &Value,
    input_keepalive: bool,
    ready: &mut Option<oneshot::Sender<bool>>,
    providers: &Option<Arc<dyn Observation>>,
    broadcast: &mut Box<dyn Broadcast>,
    last_authorized: &mut Instant,
    started: Instant,
    mut socket: Socket,
) -> Result<(), SessionError> {
    let base = endpoint(target).map_err(|_| SessionError::Closed)?;
    crate::tls::ensure_crypto_provider();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| SessionError::Closed)?;
    let mut renew_url = base.clone();
    renew_url.set_path(&format!("{}/renew", base.path()));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_renewal = Instant::now();
    let mut renewal: Option<BoxFuture<'static, HttpOutcome>> = None;
    let diagnostics = Budget::default();
    let mut renewal_reported = false;
    let mut connection = String::new();
    let mut generation = String::new();
    let mut viewers = HashSet::<String>::new();
    let mut preparations: Preparations<IceResponse> = Preparations::new();
    let mut ice = IceCache::new(http.clone(), base.clone(), target.bearer());
    let mut lease = Lease::default();
    let mut job = None;
    let mut request_id = String::new();
    let mut recording_job = false;
    let broadcast_supported = broadcast.supported();
    let playback_supported = broadcast.playback();
    let origin = base.origin();
    let mut playback_events = broadcast.playback_events();
    if let Some(events) = &mut playback_events {
        events.mark_unchanged();
    }
    // The mutex lends mutable ownership to a single scoped future. Dropping the
    // session cancels it before the publisher calls stop; no worker is detached.
    let broadcast = tokio::sync::Mutex::new(broadcast);
    let mut broadcast_job: Option<BoxFuture<'_, Result<Value, SessionError>>> = None;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if socket.video.as_ref().is_some_and(Video::failed) { return Err(SessionError::MediaFailed); }
                for viewer in socket.video.as_ref().map(Video::expired).unwrap_or_default() {
                    if lease.owner() == viewer { release(&mut lease, backend, &mut socket).await?; }
                    viewers.remove(&viewer);
                    send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;
                }
                if lease.expired() {
                    if lease.owner().starts_with("agent:") && cancel_job(&mut job).await {
                        send(&mut socket,json!({"type":"agent_result","request_id":std::mem::take(&mut request_id),"status":"cancelled"})).await?;
                    }
                    release(&mut lease,backend,&mut socket).await?;
                }
                if socket.microphone.active.is_some() && !socket.video.as_ref().is_some_and(|v| v.microphone_enabled(lease.owner()))
                    && let Some(ack) = socket.microphone.stopped() { send(&mut socket, ack).await?; }
                if last_authorized.elapsed()>Duration::from_secs(25) { return Err(SessionError::Unauthorized); }
                ice.refresh();
                if !connection.is_empty() && last_renewal.elapsed()>=Duration::from_secs(10) && renewal.is_none() {
                    last_renewal=Instant::now(); let http=http.clone(); let url=renew_url.clone(); let token=target.bearer().to_string(); let id=connection.clone();
                    renewal=Some(Box::pin(async move { HttpOutcome::response(&http.post(url).bearer_auth(token).json(&json!({"connection_id":id})).send().await) }));
                }
            },
            outcome = async { match &mut renewal { Some(future)=>future.await,None=>std::future::pending().await } } => {
                renewal=None;
                // One success per session; failures are terminal. No response body/error text.
                if !renewal_reported || !outcome.success {
                    tracing::info!(target: "nanocodex2", stage = "screen.renewal", outcome = outcome.category, http_status = outcome.status, elapsed_ms = last_renewal.elapsed().as_millis() as u64);
                    renewal_reported = true;
                }
                if !outcome.success { return Err(SessionError::Renewal(outcome)); } *last_authorized=Instant::now();
            },
            _ = ice.next() => {},
            (viewer, deadline, response) = preparations.next() => {
                let video = socket.video.as_mut().ok_or(SessionError::Closed)?;
                let offer: CoreResult<()> = (|| {
                    let response = response.inspect_err(|_| {
                        diagnostics.event("ice_fetch", "deadline_exceeded", None);
                    })?.inspect_err(|error| {
                        diagnostics.event("ice_fetch", "request_or_decode_failed", error.status().map(|status| status.as_u16()));
                    })?;
                    video.add(&viewer, ice_servers(&response)?, deadline)
                })();
                match offer {
                    Ok(()) => { diagnostics.event("peer_prepare", "scheduled", None); viewers.insert(viewer.clone()); },
                    Err(_) => {
                        diagnostics.event("peer_prepare", "failed", None);
                        send(&mut socket, json!({"type":"close_viewer","viewer_id":viewer})).await?;
                    }
                }
            },
            result = async { match &mut broadcast_job { Some(job) => job.await, None => std::future::pending().await } } => {
                broadcast_job = None;
                let result = result?;
                if result["target"] == "hls" || viewers.contains(result["viewer_id"].as_str().unwrap_or(""))
                    || preparations.contains(result["viewer_id"].as_str().unwrap_or("")) {
                    send(&mut socket, result).await?;
                }
            },
            changed = async { match &mut playback_events { Some(events) => events.changed().await, None => std::future::pending().await } } => {
                let Some(events) = playback_events.as_mut().filter(|_| changed.is_ok()) else { playback_events = None; continue; };
                let value = events.borrow_and_update().clone();
                if value["target"] == "hls" { send(&mut socket, value).await?; }
            },
            result = completed(&mut job) => {
                job=None;
                if !recording_job && lease.owner().starts_with("agent:") { release(&mut lease,backend,&mut socket).await?; }
                let mut result=if recording_job { checked_recording_result(result) } else { checked_result(result) };
                recording_job = false; result["type"]=json!("agent_result"); result["request_id"]=json!(std::mem::take(&mut request_id));
                send(&mut socket,result).await?;
            },
            message = socket.next() => {
                let message=message.ok_or(SessionError::SocketEnded)?.map_err(|_|SessionError::SocketReadFailed)?;
                let from_broker = matches!(message, Incoming::Broker(_));
                let (value, admission)=match message {
                    Incoming::Peer(value, admission)=>(value, admission),
                    Incoming::Broker(Message::Text(text))=>{
                        let Some(value)=broker_event(&text, socket.video.is_some())? else {continue;};
                        (value, None)
                    },
                    Incoming::Broker(Message::Ping(bytes))=>{socket.send(Message::Pong(bytes)).await.map_err(|_|SessionError::Closed)?;continue;},
                    Incoming::Broker(Message::Close(close))=>return Err(match close {
                        Some(close) if close.reason == "Host replaced" => SessionError::Replaced,
                        Some(close) => SessionError::BrokerClosed(close.code.into(), close_reason(&close.reason)),
                        None => SessionError::BrokerClosed(1005, "no_close_frame"),
                    }),
                    Incoming::Broker(Message::Pong(_))=>continue,
                    _=>return Err(SessionError::Closed),
                };
                if admission.as_ref().is_some_and(|permission| !permission.load(std::sync::atomic::Ordering::Acquire)) { continue; }
                let viewer=value["viewer_id"].as_str().unwrap_or("");
                match value["type"].as_str().unwrap_or("") {
                    "ready"=>{
                        *last_authorized = Instant::now();
                        tracing::info!(target: "nanocodex2", stage = "screen.socket.ready", machine_id = machine.id(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
                        if !connection.is_empty(){return Err(SessionError::Closed);}
                        connection=value["connection_id"].as_str().filter(|s|!s.is_empty()).ok_or(SessionError::Closed)?.into();
                        let mut surface=json!({"id":"desktop","name":"Desktop","kind":if base.path().starts_with("/v1/vm-host-attachments/"){"vm"}else{"desktop"},"width":dimensions.0,"height":dimensions.1,"controllable":true,"agent_tools":true});
                        surface["broadcast"]=json!(broadcast_supported);
                        if playback_supported { surface["playback"]=json!(true); }
                        surface["recording"] = recording_capability(capabilities).cloned().unwrap_or(json!(false));
                        let details = &capabilities["recordingCapabilities"];
                        if capabilities["status"] == "ok" && details["schemaVersion"] == 1
                            && details["available"].is_boolean() && details["available"] == surface["recording"]
                            && serde_json::to_vec(details).is_ok_and(|bytes| bytes.len() <= 8192) {
                            surface["recordingCapabilities"] = details.clone();
                        }
                        send(&mut socket,json!({"type":"catalog","machine_id":machine.id(),"machine_name":machine.name(),"surfaces":[surface]})).await?;
                    },
                    "published" => {
                        tracing::info!(target: "nanocodex2", stage = "screen.published", machine_id = machine.id(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
                        generation = value["generation"].as_str().ok_or(SessionError::Closed)?.into();
                        if socket.video.is_some() { ice.prefetch(); }
                        if let Some(ready) = ready.take() { let _ = ready.send(true); }
                    },
                    // Playback is commanded only by the broker itself, never by a
                    // relayed viewer (which always carries viewer_id) or a peer.
                    "broadcast" if value.get("target").is_some() || value.get("upload").is_some() => {
                        if from_broker && playback_supported && value["target"] == "hls"
                            && value.get("viewer_id").is_none() && value["surface_id"] == "desktop"
                            && valid_playback_id(&value["request_id"]) && valid_playback_id(&value["stream_id"]) {
                            if broadcast_job.is_some() {
                                send(&mut socket, json!({"type":"broadcast_result","target":"hls","request_id":value["request_id"],
                                    "stream_id":value["stream_id"],"status":"failed","error":"busy"})).await?;
                            } else {
                                let broadcast = &broadcast;
                                let origin = &origin;
                                broadcast_job = Some(Box::pin(async move {
                                    let mut broadcast = broadcast.lock().await;
                                    tokio::time::timeout(Duration::from_secs(5), broadcast.playback_request(&value, origin))
                                        .await.map_err(|_| SessionError::Closed)
                                }));
                            }
                        }
                    },
                    // Status is read-only and is sent when the viewer socket opens,
                    // before asynchronous ICE preparation has admitted its peer.
                    "broadcast" if (viewers.contains(viewer) || (preparations.contains(viewer) && value["action"] == "status"))
                        && value["surface_id"] == "desktop" => {
                        if broadcast_job.is_some() {
                            send(&mut socket, broadcast_failure(&value, "busy")).await?;
                        } else {
                            let broadcast = &broadcast;
                            broadcast_job = Some(Box::pin(async move {
                                let mut broadcast = broadcast.lock().await;
                                match tokio::time::timeout(Duration::from_secs(5), broadcast.request(&value)).await {
                                    Ok(result) => Ok(result),
                                    // Return ownership to the publisher for stop/cleanup.
                                    // A timed-out start must not outlive this session.
                                    Err(_) => Err(SessionError::Closed),
                                }
                            }));
                        }
                    },
                    "renewed"=>*last_authorized=Instant::now(),
                    "pong"=>{},
                    "viewer"=>{
                        diagnostics.event("viewer", "received", None);
                        if viewer.is_empty() || value["surface_id"]!="desktop" {return Err(SessionError::Closed);}
                        // A duplicate ID is ambiguous even at capacity: cancel
                        // that viewer without disturbing unrelated peers.
                        if viewers.contains(viewer) || preparations.contains(viewer) {
                            preparations.remove(viewer);
                            viewers.remove(viewer);
                            if lease.owner()==viewer{release(&mut lease,backend,&mut socket).await?;}
                            send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;
                            continue;
                        }
                        if viewers.len()+preparations.len()>=VIEWER_CAPACITY {
                            send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;
                        } else if socket.video.is_some() {
                            let deadline=tokio::time::Instant::now()+Duration::from_secs(8);
                            preparations.insert(viewer, deadline, ice.request()).map_err(|_|SessionError::Closed)?;
                        } else {
                            viewers.insert(viewer.into());
                        }
                    },
                    "viewer_suspended" if viewers.contains(viewer)=>{
                        if lease.owner()==viewer { release(&mut lease,backend,&mut socket).await?; }
                    },
                    "viewer_resumed" if viewers.contains(viewer)=>{
                        send(&mut socket,json!({"type":"control","viewer_id":viewer,"data":{"type":"revoked"}})).await?;
                    },
                    "viewer_left"=>{
                        preparations.remove(viewer);
                        viewers.remove(viewer);
                        viewer_left(viewer,&mut lease,backend,&mut socket).await?;
                    },
                    "signal" if preparations.contains(viewer)=>{
                        diagnostics.event("signal", "before_offer", None);
                        // No pre-offer signaling queue: cancel and fail closed.
                        preparations.remove(viewer);
                        send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;
                    },
                    "signal" if viewers.contains(viewer)=>{
                        if let Some(video)=&mut socket.video
                            && video.signal(viewer,&value["signal"]).is_err() {
                            diagnostics.event("signal", "rejected", None);
                            if lease.owner()==viewer{release(&mut lease,backend,&mut socket).await?;}
                            send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;viewers.remove(viewer);
                        }
                    },
                    "control" if viewers.contains(viewer)=>{
                        let permission = admission;
                        if socket.video.is_some() && permission.is_none() { continue; }
                        let data=&value["data"];
                        if socket.video.as_ref().is_some_and(|video| video.revoked_control(viewer, data)) { continue; }
                        match data["type"].as_str().unwrap_or("") {
                            "acquire" if data.get("generation").is_none()=>{
                                // A human cancels an agent before receiving the input lease.
                                if lease.owner().starts_with("agent:") || lease.expired() {
                                    if cancel_job(&mut job).await{send(&mut socket,json!({"type":"agent_result","request_id":std::mem::take(&mut request_id),"status":"cancelled"})).await?;}
                                    release(&mut lease,backend,&mut socket).await?;
                                }
                                if lease.owner().is_empty(){release(&mut lease,backend,&mut socket).await?;socket.microphone.refreshed(Instant::now());
                                    if !lease.acquire_connected(viewer, permission) { continue; }
                                    if socket.video.as_ref().is_some_and(|video| !lease.transport_permission().is_some_and(|permission| video.grant_control(viewer, permission, lease.generation()))) { continue; }
                                    let mut grant = control_grant(lease.generation(), capabilities);
                                    if socket.video.as_ref().is_some_and(Video::microphone_available) { grant["microphone"] = json!(true); }
                                    send(&mut socket,json!({"type":"control","viewer_id":viewer,"data":grant})).await?;}
                                else{send(&mut socket,json!({"type":"control","viewer_id":viewer,"data":{"type":"denied"}})).await?;}
                            },
                            "renew" if lease.valid(viewer,data["generation"].as_str().unwrap_or(""))=>{
                                let renewed_at = Instant::now();
                                if !renew_control(&mut lease, viewer, data["generation"].as_str().unwrap_or(""), backend, input_keepalive).await {
                                    release(&mut lease,backend,&mut socket).await?;
                                } else {
                                    socket.microphone.refreshed(renewed_at);
                                    if let Some(video) = &socket.video { video.renew_microphone(viewer, socket.microphone.remaining()); }
                                }
                            },
                            "microphone" => {
                                let ack = {
                                    let video = socket.video.as_ref();
                                    socket.microphone.request(&lease, viewer, data, |enabled, remaining| {
                                        video.is_some_and(|v| lease.transport_permission().is_some_and(|permission| v.set_microphone(viewer, enabled, remaining, permission)))
                                    })
                                };
                                if let Some(ack) = ack { send(&mut socket, ack).await?; }
                            },
                            "release" if lease.valid(viewer,data["generation"].as_str().unwrap_or(""))=>release(&mut lease,backend,&mut socket).await?,
                            _=>{send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;if lease.owner()==viewer{release(&mut lease,backend,&mut socket).await?;}viewers.remove(viewer);},
                        }
                    },
                    "input" if viewers.contains(viewer)=>{
                        if lease.accept(viewer,&value["data"]){
                            let mut input=value["data"].clone(); if let Some(input)=input.as_object_mut(){input.remove("generation");input.remove("sequence");}
                            let result=call(backend,json!({"action":"input","input":input}),Duration::from_secs(2)).await;
                            if result["status"]!="ok" {release(&mut lease,backend,&mut socket).await?;}
                        }
                    },
                    "agent_cancel"=>{if value["request_id"]==request_id && cancel_job(&mut job).await{if !recording_job && lease.owner().starts_with("agent:"){release(&mut lease,backend,&mut socket).await?;}recording_job=false;send(&mut socket,json!({"type":"agent_result","request_id":std::mem::take(&mut request_id),"status":"cancelled"})).await?;}},
                    "agent_call"=>{
                        let id=value["request_id"].as_str().unwrap_or("");let action=&value["input"];let now=now_ms();let deadline=value["deadline_at"].as_u64().unwrap_or(0);
                        let job_deadline = tokio::time::Instant::now() + Duration::from_millis(deadline.saturating_sub(now).min(10_000));
                        let owner=format!("agent:{}",value["agent_id"].as_str().unwrap_or(""));
                        let status=if value["surface_id"]!="desktop" || value["generation"]!=generation || !valid_id(id) || !valid_id(value["agent_id"].as_str().unwrap_or("")) || deadline<=now || deadline>now+10_000 {Some("invalid")}
                        else if job.is_some() || (!lease.owner().is_empty() && !lease.expired() && action["action"]!="observe" && action["action"]!="recording" && lease.owner()!=owner) {Some("busy")} else {None};
                        if let Some(status)=status{send(&mut socket,json!({"type":"agent_result","request_id":id,"status":status})).await?;continue;}
                        if action["action"] == "recording" {
                            if !recording_capability(capabilities).is_some_and(|value| value == true || value["available"] == true) {
                                send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"unavailable"})).await?;
                                continue;
                            }
                            // Recording belongs to the native Hand. Preserve its JSON contract
                            // and the authenticated envelope without acquiring an input lease.
                            let backend = backend.clone();
                            let action = action.clone();
                            request_id = id.into();
                            recording_job = true;
                            job = Some(OwnedJob(tokio::spawn(async move {
                                if tokio::time::Instant::now() >= job_deadline { return json!({"status":"cancelled"}); }
                                tokio::time::timeout_at(job_deadline, backend(action)).await
                                    .map(|result| result.unwrap_or_else(|_|json!({"status":"unavailable"})))
                                    .unwrap_or_else(|_|json!({"status":"cancelled"}))
                            })));
                            continue;
                        }
                        if action["action"]=="release" {if lease.owner()==owner{release(&mut lease,backend,&mut socket).await?;}send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"ok"})).await?;continue;}
                        let steps=match steps(action){Ok(steps)=>steps,Err(())=>{send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"invalid"})).await?;continue;}};
                        if action.get("context").is_some() && action["action"] != "observe" {send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"invalid"})).await?;continue;}
                        let context = action.get("context").cloned();
                        if !providers.as_ref().map_or(context.is_none(), |p| p.valid_context(context.as_ref())) {
                            send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"invalid"})).await?;continue;
                        }
                        let providers = providers.clone();
                        let settle = !steps.is_empty();
                        if settle {
                            // Native release can await up to three seconds: recheck the
                            // original deadline before acquiring or starting any input.
                            release(&mut lease, backend, &mut socket).await?;
                        }
                        if tokio::time::Instant::now() >= job_deadline {
                            send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"cancelled"})).await?;
                            continue;
                        }
                        if settle { lease.acquire(&owner); }
                        let backend=backend.clone();request_id=id.into();
                        job=Some(OwnedJob(tokio::spawn(async move{
                            tokio::time::timeout_at(job_deadline,async{
                                // Tokio polls the inner future before its timeout. Do not
                                // inject when scheduling consumed the remaining budget.
                                if tokio::time::Instant::now() >= job_deadline { return json!({"status":"cancelled"}); }
                                for (delay,input) in steps {if !delay.is_zero(){tokio::time::sleep(delay).await;}
                                if tokio::time::Instant::now() >= job_deadline {return json!({"status":"cancelled"});}let result=call(&backend,json!({"action":"input","input":input}),Duration::from_secs(2)).await;if result["status"]!="ok"{return result;}}
                                if settle { tokio::time::sleep(Duration::from_millis(80)).await; }
                                observe_agent(&backend, &providers, context, deadline).await
                            }).await.unwrap_or_else(|_|json!({"status":"cancelled"}))
                        })));
                    },
                    // A stale viewer, or a start/stop before preparation completes,
                    // has no authority. It must not tear down unrelated viewers.
                    "frame_request"|"input"|"control"|"broadcast"=>{},
                    _=>return Err(SessionError::Closed),
                }
            },
        }
    }
}
// Provider deadlines are independent of image capture and finish before the
// agent envelope expires, preserving a successful screenshot when a provider stalls.
async fn observe_agent(
    backend: &Backend,
    providers: &Option<Arc<dyn Observation>>,
    context: Option<Value>,
    deadline: u64,
) -> Value {
    // Anchor the collection request; capture and providers complete independently.
    let captured_at = now_ms();
    let budget = Duration::from_millis(deadline.saturating_sub(captured_at).saturating_sub(50));
    let (mut capture, observation) = tokio::join!(
        call(backend, json!({"action":"observe"}), Duration::from_secs(4)),
        async {
            match providers {
                Some(providers) => Some(providers.collect(context, captured_at, budget).await),
                None => None,
            }
        }
    );
    if capture["status"] == "ok"
        && let Some(observation) = observation
    {
        capture["observation"] = observation;
    }
    capture
}
async fn call(backend: &Backend, input: Value, timeout: Duration) -> Value {
    let started = Instant::now();
    let action = input["action"].as_str().unwrap_or("unknown").to_owned();
    let result = match tokio::time::timeout(timeout, backend(input)).await {
        Ok(Ok(value)) => value,
        _ => json!({"status":"unavailable"}),
    };
    tracing::debug!(target: "nanocodex2", stage = "screen.backend", %action,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, status = result["status"].as_str().unwrap_or("unknown"));
    result
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}
fn valid_frame(value: &Value) -> bool {
    value["jpeg"]
        .as_str()
        .is_some_and(|v| v.starts_with("/9j/") && v.len() <= 700_000)
        && ["width", "height"]
            .iter()
            .all(|key| value[*key].as_u64().is_some_and(|v| v > 0 && v <= 1280))
}
fn recording_capability(capabilities: &Value) -> Option<&Value> {
    let value = &capabilities["recording"];
    (capabilities["status"] == "ok"
        && (value.is_boolean() || (value["schemaVersion"] == 1 && value["available"].is_boolean()))
        && serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 8192))
    .then_some(value)
}
// The broker accepts at most 750,000 UTF-8 bytes. Leave room for its envelope,
// and retain native recording errors/metadata without treating them as screenshots.
fn checked_recording_result(value: Value) -> Value {
    if !value.is_object()
        || !["ok", "error", "busy", "invalid", "unavailable", "cancelled"]
            .contains(&value["status"].as_str().unwrap_or(""))
        || serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 740_000)
    {
        return json!({"status":"unavailable"});
    }
    let status = if value["status"] == "error" {
        match value["error"].as_str() {
            Some("invalid_request" | "not_found") => "invalid",
            Some("conflict") => "busy",
            _ => "unavailable",
        }
    } else {
        value["status"].as_str().unwrap_or("unavailable")
    };
    json!({"status": status, "recording": value})
}
fn checked_result(value: Value) -> Value {
    let status = value["status"].as_str().unwrap_or("unavailable");
    if status == "ok" && valid_frame(&value) {
        let mut result = json!({"status":"ok","jpeg":value["jpeg"],"width":value["width"],"height":value["height"]});
        if let Some(observation) = value.get("observation") {
            result["observation"] = observation.clone();
        }
        result
    } else {
        json!({"status":if ["busy","invalid","unavailable","cancelled"].contains(&status){status}else{"unavailable"}})
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn error(value: impl std::fmt::Display) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(value.to_string()).into()
}
fn steps(action: &Value) -> Result<Vec<(Duration, Value)>, ()> {
    let mut out = Vec::new();
    let mut push = |value| out.push((Duration::ZERO, value));
    match action["action"].as_str().ok_or(())? {
        "observe" => {}
        "click" => {
            for down in [true, false] {
                push(
                    json!({"kind":"button","x":action["x"],"y":action["y"],"button":action.get("button").cloned().unwrap_or(json!(0)),"down":down}),
                );
            }
        }
        "type" => push(json!({"kind":"text","text":action["text"]})),
        "key" => {
            let modifiers = action
                .get("modifiers")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if modifiers.len() > 4 {
                return Err(());
            }
            let mut seen = HashSet::new();
            for modifier in &modifiers {
                let key = modifier
                    .as_u64()
                    .filter(|v| (224..=231).contains(v))
                    .ok_or(())?;
                if !seen.insert(key) {
                    return Err(());
                }
                push(json!({"kind":"key","key":key,"down":true}));
            }
            for down in [true, false] {
                push(json!({"kind":"key","key":action["key"],"down":down}));
            }
            for modifier in modifiers.iter().rev() {
                push(json!({"kind":"key","key":modifier,"down":false}));
            }
        }
        "scroll" => push(
            json!({"kind":"scroll","x":action["x"],"y":action["y"],"deltaX":action.get("deltaX").cloned().unwrap_or(json!(0)),"deltaY":action.get("deltaY").cloned().unwrap_or(json!(0))}),
        ),
        "drag" => {
            let coord = |key| {
                action[key]
                    .as_f64()
                    .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                    .ok_or(())
            };
            let (x, y, end_x, end_y) = (coord("x")?, coord("y")?, coord("endX")?, coord("endY")?);
            let duration = action
                .get("durationMs")
                .map_or(Some(300), Value::as_u64)
                .filter(|d| (50..=1500).contains(d))
                .ok_or(())?;
            let count = (duration / 33).max(2);
            push(json!({"kind":"button","x":x,"y":y,"button":0,"down":true}));
            for i in 1..=count {
                let fraction = i as f64 / count as f64;
                out.push((
                    Duration::from_millis(duration / count),
                    json!({"kind":"move","x":x+(end_x-x)*fraction,"y":y+(end_y-y)*fraction}),
                ));
            }
            out.push((
                Duration::ZERO,
                json!({"kind":"button","x":end_x,"y":end_y,"button":0,"down":false}),
            ));
        }
        _ => return Err(()),
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    #[test]
    fn broker_transport_cannot_inject_webrtc_input() {
        for kind in ["input", "control", "frame_request"] {
            let event = serde_json::json!({"type":kind,"viewer_id":"fixture"});
            let text = event.to_string();
            assert!(super::broker_event(&text, true).unwrap().is_none());
            assert_eq!(super::broker_event(&text, false).unwrap(), Some(event));
        }
        let signaling = serde_json::json!({"type":"signal","viewer_id":"fixture"});
        assert_eq!(
            super::broker_event(&signaling.to_string(), true).unwrap(),
            Some(signaling)
        );
        for webrtc in [false, true] {
            assert!(super::broker_event("invalid JSON", webrtc).is_err());
        }
    }

    use super::*;
    type TestWire = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;
    async fn wire_send(wire: &mut TestWire, value: Value) {
        wire.send(Message::Text(value.to_string().into()))
            .await
            .unwrap();
    }
    async fn wire_read(wire: &mut TestWire) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(1), wire.next())
            .await
            .expect("session stopped processing messages")
            .unwrap()
            .unwrap();
        serde_json::from_str(message.to_text().unwrap()).unwrap()
    }
    async fn test_session(backend: Backend, options: Options) -> (Publisher, TestWire) {
        let (publisher, wire, _) = test_session_catalog(backend, options).await;
        (publisher, wire)
    }
    async fn test_session_catalog(
        backend: Backend,
        options: Options,
    ) -> (Publisher, TestWire, Value) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = PublisherTarget::from_attachment(
            &format!(
                "ws://{}/v1/account/tool-host",
                listener.local_addr().unwrap()
            ),
            "test-token",
        )
        .unwrap();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut wire = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.headers()["authorization"], "Bearer test-token");
                    assert_eq!(request.uri().path(), "/v1/account/hands/host");
                    Ok(response)
                },
            )
            .await
            .unwrap();
            wire_send(&mut wire, json!({"type":"ready","connection_id":"test"})).await;
            let catalog = wire_read(&mut wire).await;
            assert_eq!(catalog["type"], "catalog");
            wire_send(&mut wire, json!({"type":"published","generation":"g"})).await;
            wire_send(
                &mut wire,
                json!({"type":"viewer","viewer_id":"v","surface_id":"desktop"}),
            )
            .await;
            (wire, catalog)
        });
        let machine = Machine::new("test", "Test").unwrap();
        let (wire, _) = tokio_tungstenite::connect_async({
            let mut url = target.endpoint().clone();
            url.set_scheme("ws").unwrap();
            url.set_path(&format!("{}/host", url.path()));
            let mut request = url.to_string().into_client_request().unwrap();
            request
                .headers_mut()
                .insert("authorization", "Bearer test-token".parse().unwrap());
            request
        })
        .await
        .unwrap();
        let (sender, _) = watch::channel(target.clone());
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut broadcast = options.broadcast;
            let mut ready = None;
            let mut authorized = Instant::now();
            let capabilities = call(
                &backend,
                json!({"action":"capabilities"}),
                Duration::from_secs(5),
            )
            .await;
            tokio::select! {
                _ = &mut stopped => {},
                _ = session_loop(&target, &machine, &backend, (1, 1),
                    &capabilities, false, &mut ready,
                    &options.observation, &mut broadcast, &mut authorized,
                    Instant::now(), Socket { wire, video: None,
                        microphone: MicrophoneControl::default() }) => {},
            }
            let _ = call(
                &backend,
                json!({"action":"release"}),
                Duration::from_secs(3),
            )
            .await;
            broadcast.stop().await;
        });
        let publisher = Publisher {
            target: sender,
            stop: Some(stop),
            task: Some(task),
            initially_replaced: false,
        };
        let (wire, catalog) = peer.await.unwrap();
        (publisher, wire, catalog)
    }
    #[tokio::test]
    async fn recording_transport_preserves_human_control_and_bounds_native_results() {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        let backend: Backend = Arc::new(move |input| {
            let seen = seen.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(input.clone());
                Ok(match input["action"].as_str().unwrap_or("") {
                    "capabilities" => {
                        json!({"status":"ok","recording":true,"recordingCapabilities":{"schemaVersion":1,"available":true,"operations":["start","status","frame","export"],"capture":{"native_observer":true}}})
                    }
                    "recording" => match input["operation"].as_str().unwrap_or("") {
                        "pause" => json!({"status":"error","error":"conflict"}),
                        "frame" => json!({"status":"ok","data_base64":"a".repeat(740_000)}),
                        "export" => {
                            std::future::pending::<()>().await;
                            unreachable!()
                        }
                        _ => {
                            json!({"status":"ok","id":"rec_00000000000000000000000000000000","state":"recording"})
                        }
                    },
                    _ => json!({"status":"ok","width":1,"height":1,"jpeg":"/9j/2Q=="}),
                })
            })
        });
        let (publisher, mut wire, catalog) =
            test_session_catalog(backend, Options::default()).await;
        assert_eq!(catalog["surfaces"][0]["recording"], true);
        assert_eq!(
            catalog["surfaces"][0]["recordingCapabilities"],
            json!({"schemaVersion":1,"available":true,"operations":["start","status","frame","export"],"capture":{"native_observer":true}})
        );
        wire_send(
            &mut wire,
            json!({"type":"control","viewer_id":"v","data":{"type":"acquire"}}),
        )
        .await;
        let grant = wire_read(&mut wire).await;
        assert_eq!(grant["data"]["type"], "granted");
        let input_generation = grant["data"]["generation"].clone();
        calls.lock().unwrap().clear();
        let request = json!({"type":"agent_call","request_id":"recording-request","agent_id":"a","surface_id":"desktop","generation":"g","deadline_at":now_ms()+8000,"input":{"action":"recording","operation":"start","scope":{"apps":["pid:1"]}}});
        for (field, value) in [
            ("surface_id", json!("other")),
            ("generation", json!("stale")),
            ("deadline_at", json!(0)),
        ] {
            let mut stale = request.clone();
            stale[field] = value;
            wire_send(&mut wire, stale).await;
            assert_eq!(wire_read(&mut wire).await["status"], "invalid");
            eprintln!(
                "RECORDING_EVIDENCE {}",
                json!({"transport":"publisher-websocket","rejected_envelope_field":field,"backend_calls":0})
            );
        }
        assert!(calls.lock().unwrap().is_empty());
        wire_send(&mut wire, request.clone()).await;
        let result = wire_read(&mut wire).await;
        eprintln!(
            "RECORDING_EVIDENCE {}",
            json!({"transport":"publisher-websocket","capability":catalog["surfaces"][0]["recordingCapabilities"],"input":request["input"],"result":result})
        );
        assert_eq!(result["recording"]["state"], "recording");
        assert_eq!(*calls.lock().unwrap(), [request["input"].clone()]);
        let mut conflict = request.clone();
        conflict["input"]["operation"] = json!("pause");
        wire_send(&mut wire, conflict).await;
        let conflict = wire_read(&mut wire).await;
        assert_eq!(conflict["status"], "busy");
        assert_eq!(
            conflict["recording"],
            json!({"status":"error","error":"conflict"})
        );
        let mut frame = request.clone();
        frame["input"]["operation"] = json!("frame");
        wire_send(&mut wire, frame).await;
        assert_eq!(
            wire_read(&mut wire).await,
            json!({"type":"agent_result","request_id":"recording-request","status":"unavailable"})
        );
        let mut expired = request.clone();
        expired["input"]["operation"] = json!("export");
        expired["deadline_at"] = json!(now_ms() + 50);
        wire_send(&mut wire, expired).await;
        assert_eq!(wire_read(&mut wire).await["status"], "cancelled");
        let mut stalled = request.clone();
        stalled["input"]["operation"] = json!("export");
        wire_send(&mut wire, stalled).await;
        wire_send(
            &mut wire,
            json!({"type":"agent_cancel","request_id":"recording-request"}),
        )
        .await;
        assert_eq!(wire_read(&mut wire).await["status"], "cancelled");
        wire_send(&mut wire, json!({"type":"input","viewer_id":"v","data":{"generation":input_generation,"sequence":1,"kind":"text","text":"still-controlled"}})).await;
        // A status request fences processing of the preceding human input.
        let mut status = request;
        status["input"]["operation"] = json!("status");
        wire_send(&mut wire, status).await;
        assert_eq!(wire_read(&mut wire).await["status"], "ok");
        let calls = calls.lock().unwrap().clone();
        assert!(calls.iter().any(|value| value["action"] == "input"));
        assert!(!calls.iter().any(|value| value["action"] == "release"));
        eprintln!(
            "RECORDING_EVIDENCE {}",
            json!({"transport":"publisher-websocket","native_conflict":"busy","oversized_frame":"unavailable","deadline":"cancelled","cancel":"cancelled","human_input_accepted":true,"input_release_calls":0})
        );
        publisher.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recording_transport_requires_native_capability() {
        let backend: Backend = Arc::new(|input| {
            Box::pin(async move {
                assert_ne!(input["action"], "recording");
                Ok(
                    json!({"status":"ok","width":1,"height":1,"jpeg":"/9j/2Q==","recording":false,"recordingCapabilities":{"schemaVersion":1,"available":false,"reason":"store_unavailable"}}),
                )
            })
        });
        let (publisher, mut wire, catalog) =
            test_session_catalog(backend, Options::default()).await;
        assert_eq!(catalog["surfaces"][0]["recording"], false);
        assert_eq!(
            catalog["surfaces"][0]["recordingCapabilities"],
            json!({"schemaVersion":1,"available":false,"reason":"store_unavailable"})
        );
        wire_send(&mut wire, json!({"type":"agent_call","request_id":"unsupported","agent_id":"a","surface_id":"desktop","generation":"g","deadline_at":now_ms()+8000,"input":{"action":"recording","operation":"start","scope":{"apps":["pid:1"]}}})).await;
        assert_eq!(wire_read(&mut wire).await["status"], "unavailable");
        publisher.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn early_broadcast_status_does_not_cancel_webrtc_preparation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Status(Arc<std::sync::Mutex<Vec<String>>>);
        impl Broadcast for Status {
            fn supported(&self) -> bool {
                true
            }
            fn request<'a>(&'a mut self, request: &'a Value) -> BoxFuture<'a, Value> {
                self.0
                    .lock()
                    .unwrap()
                    .push(request["action"].as_str().unwrap().into());
                Box::pin(async move {
                    json!({"type":"broadcast_result", "viewer_id":request["viewer_id"], "request_id":request["request_id"], "status":"idle"})
                })
            }
            fn stop(&mut self) -> BoxFuture<'_, ()> {
                Box::pin(async {})
            }
        }
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let video: VideoSource = Arc::new(|| {
            Box::pin(async {
                Ok(crate::capture::Capture::packets(
                    futures_util::stream::once(async {
                        Ok(bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1]))
                    })
                    .chain(futures_util::stream::pending()),
                    None,
                ))
            })
        });
        let backend: Backend =
            Arc::new(|_| Box::pin(async { Ok(json!({"status":"ok","width":1,"height":1})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = PublisherTarget::from_attachment(
            &format!(
                "ws://{}/v1/account/tool-host",
                listener.local_addr().unwrap()
            ),
            "test-token",
        )
        .unwrap();
        let (ice_started, waiting_ice) = oneshot::channel();
        let (allow_ice, ice_allowed) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut wire = tokio_tungstenite::accept_async(stream).await.unwrap();
            wire_send(&mut wire, json!({"type":"ready","connection_id":"test"})).await;
            let catalog = wire_read(&mut wire).await;
            assert_eq!(catalog["type"], "catalog");
            assert!(
                catalog["surfaces"][0].get("transport").is_none(),
                "fixture must exercise WebRTC"
            );
            wire_send(&mut wire, json!({"type":"published","generation":"g"})).await;
            wire_send(
                &mut wire,
                json!({"type":"viewer","viewer_id":"v","surface_id":"desktop"}),
            )
            .await;
            let http = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                assert!(stream.read(&mut bytes).await.unwrap() > 0);
                ice_started.send(()).unwrap();
                ice_allowed.await.unwrap();
                let body = r#"{"iceServers":[]}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
            (wire, http)
        });
        let publisher = Publisher::start(
            &target,
            &Machine::new("test", "Test").unwrap(),
            backend,
            Options {
                video: Some(video),
                broadcast: Box::new(Status(calls.clone())),
                ..Options::default()
            },
        )
        .await
        .unwrap();
        let (mut wire, http) = peer.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), waiting_ice)
            .await
            .unwrap()
            .unwrap();
        // The browser asks for status as soon as its socket opens, before ICE is ready.
        wire_send(&mut wire,json!({"type":"broadcast","viewer_id":"v","surface_id":"desktop","request_id":"early","action":"status"})).await;
        assert_eq!(wire_read(&mut wire).await["request_id"], "early");
        // Preparing viewers gain no start/stop authority; stale IDs cannot disrupt others.
        for (viewer, action) in [("v", "start"), ("v", "stop"), ("gone", "status")] {
            wire_send(&mut wire,json!({"type":"broadcast","viewer_id":viewer,"surface_id":"desktop","request_id":"ignored","action":action})).await;
        }
        wire_send(&mut wire,json!({"type":"broadcast","viewer_id":"v","surface_id":"desktop","request_id":"still-live","action":"status"})).await;
        assert_eq!(wire_read(&mut wire).await["request_id"], "still-live");
        assert_eq!(*calls.lock().unwrap(), ["status", "status"]);
        allow_ice.send(()).unwrap();
        http.await.unwrap();
        let offer = wire_read(&mut wire).await;
        assert_eq!(offer["signal"]["type"], "offer");
        assert_eq!(offer["viewer_id"], "v");
        publisher.shutdown().await.unwrap();
    }

    async fn departing_owner_fixture() -> (Socket, TestWire, Lease) {
        let source: VideoSource = Arc::new(|| {
            Box::pin(async {
                Ok(crate::capture::Capture::packets(
                    futures_util::stream::unfold(
                        tokio::time::interval(Duration::from_millis(20)),
                        |mut tick| async move {
                            tick.tick().await;
                            Some((Ok(bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1])), tick))
                        },
                    ),
                    None,
                ))
            })
        });
        let mut video = Video::start(&source, None).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        for viewer in ["owner", "survivor"] {
            video.add(viewer, Vec::new(), deadline).unwrap();
        }
        // Polling next installs the real peers; add alone only schedules setup.
        // Leave both offers unanswered so control sends fail deterministically.
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut offered = HashSet::new();
            while offered.len() < 2 {
                let event = video.next().await.expect("peer preparation ended");
                assert!(event.outgoing, "peer preparation failed: {}", event.value);
                if event.value["signal"]["type"] == "offer" {
                    let viewer = event.value["viewer_id"].as_str().unwrap();
                    assert!(matches!(viewer, "owner" | "survivor"));
                    assert!(offered.insert(viewer.to_owned()));
                }
            }
        })
        .await
        .expect("peer offers did not complete");
        for viewer in ["owner", "survivor"] {
            assert!(
                video.add(viewer, Vec::new(), deadline).is_err(),
                "fixture peer must be installed"
            );
        }
        assert!(!video.failed());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, server) = tokio::join!(
            tokio::net::TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept(),
        );
        let wire = Wire::from_raw_socket(
            tokio_tungstenite::MaybeTlsStream::Plain(client.unwrap()),
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let broker = TestWire::from_raw_socket(
            server.unwrap().0,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        let mut lease = Lease::default();
        lease.acquire("owner");
        let mut microphone = MicrophoneControl::default();
        microphone.refreshed(Instant::now());
        let ack = microphone
            .request(
                &lease,
                "owner",
                &json!({"generation":lease.generation(),"requestID":"mic","enabled":true}),
                |enabled, _| enabled,
            )
            .unwrap();
        assert_eq!(ack["data"]["enabled"], true);
        assert!(microphone.active.is_some());
        (
            Socket {
                wire,
                video: Some(video),
                microphone,
            },
            broker,
            lease,
        )
    }

    #[tokio::test]
    async fn failed_control_acknowledgment_retires_only_its_viewer() {
        let (mut socket, _broker, mut lease) = departing_owner_fixture().await;
        let backend: Backend = Arc::new(|_| Box::pin(async { Ok(json!({"status":"ok"})) }));
        release(&mut lease, &backend, &mut socket).await.unwrap();
        let video = socket.video.as_mut().unwrap();
        let event = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = video.next().await.unwrap();
                if !event.outgoing {
                    break event;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(event.value["type"], "viewer_left");
        assert_eq!(event.value["viewer_id"], "owner");
        assert!(!video.failed());
        // The second peer can still receive signaling after the failed ack.
        assert!(
            video
                .signal("survivor", &json!({"type":"candidate","candidate":null}))
                .is_ok()
        );
    }

    #[tokio::test]
    async fn departed_owner_is_removed_before_release_acknowledgments() {
        // Native failure must still close the session after the peer is gone.
        for status in ["ok", "unavailable"] {
            let (mut socket, _broker, mut lease) = departing_owner_fixture().await;
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = calls.clone();
            let backend: Backend = Arc::new(move |input| {
                recorded.lock().unwrap().push(input);
                Box::pin(async move { Ok(json!({"status":status})) })
            });

            let result = viewer_left("owner", &mut lease, &backend, &mut socket).await;
            if status == "ok" {
                assert!(
                    result.is_ok(),
                    "dead peer acknowledgments closed the session"
                );
            } else {
                assert!(matches!(result, Err(SessionError::Closed)));
            }
            assert!(lease.owner().is_empty());
            assert!(lease.generation().is_empty());
            assert!(socket.microphone.active.is_none());
            assert!(socket.microphone.deadline.is_none());
            assert_eq!(*calls.lock().unwrap(), [json!({"action":"release"})]);
            let video = socket.video.as_mut().unwrap();
            assert!(!video.failed(), "one departure poisoned the media pipeline");
            assert!(
                video
                    .control("owner", &json!({"type":"revoked"}))
                    .await
                    .is_ok()
            );
            assert!(
                video
                    .add(
                        "survivor",
                        Vec::new(),
                        tokio::time::Instant::now() + Duration::from_secs(5)
                    )
                    .is_err(),
                "the other unnegotiated peer must remain installed"
            );
            // Repeated departure notifications must not release native input twice.
            viewer_left("owner", &mut lease, &backend, &mut socket)
                .await
                .unwrap();
            assert_eq!(*calls.lock().unwrap(), [json!({"action":"release"})]);
        }
    }

    #[tokio::test]
    async fn failed_native_release_cannot_grant_control() {
        for failure in ["status", "error", "timeout"] {
            let backend: Backend = Arc::new(move |input| {
                Box::pin(async move {
                    if input["action"] == "release" {
                        match failure {
                            "status" => return Ok(json!({"status":"unavailable"})),
                            "error" => return Err(error("native release failed")),
                            _ => std::future::pending::<()>().await,
                        }
                    }
                    Ok(json!({"status":"ok","jpeg":"/9j/a","width":1,"height":1}))
                })
            });
            let (publisher, mut wire) = test_session(backend, Options::default()).await;
            wire_send(
                &mut wire,
                json!({"type":"control","viewer_id":"v","data":{"type":"acquire"}}),
            )
            .await;
            // Closing the session fences every viewer. It must not continue with
            // a grant when device state is unknown, including on timeout.
            let next = tokio::time::timeout(Duration::from_secs(5), wire.next())
                .await
                .expect("release failure did not close the session");
            assert!(
                matches!(next, None | Some(Err(_)) | Some(Ok(Message::Close(_)))),
                "{failure} release failure sent a control message: {next:?}"
            );
            publisher.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn human_takeover_cancels_agent_input_before_native_release_and_grant() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        struct InputGuard(Arc<std::sync::Mutex<Vec<&'static str>>>);
        impl Drop for InputGuard {
            fn drop(&mut self) {
                self.0.lock().unwrap().push("cancelled");
            }
        }
        let backend: Backend = {
            let events = events.clone();
            let entered = entered.clone();
            Arc::new(move |input| {
                let events = events.clone();
                let entered = entered.clone();
                Box::pin(async move {
                    if input["action"] == "input" {
                        let _guard = InputGuard(events.clone());
                        events.lock().unwrap().push("input");
                        entered.notify_one();
                        std::future::pending::<()>().await;
                    }
                    if input["action"] == "release" {
                        events.lock().unwrap().push("release");
                    }
                    Ok(json!({"status":"ok","jpeg":"/9j/a","width":1,"height":1}))
                })
            })
        };
        let (publisher, mut wire) = test_session(backend, Options::default()).await;
        wire_send(&mut wire, json!({"type":"agent_call","request_id":"r","agent_id":"a","surface_id":"desktop","generation":"g","deadline_at":now_ms()+9000,"input":{"action":"click","x":1,"y":1}})).await;
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        wire_send(
            &mut wire,
            json!({"type":"control","viewer_id":"v","data":{"type":"acquire"}}),
        )
        .await;
        let cancelled = wire_read(&mut wire).await;
        assert_eq!(cancelled["type"], "agent_result");
        assert_eq!(cancelled["request_id"], "r");
        assert_eq!(cancelled["status"], "cancelled");
        let grant = wire_read(&mut wire).await;
        assert_eq!(grant["data"]["type"], "granted");
        let recorded = events.lock().unwrap().clone();
        let input = recorded.iter().position(|event| *event == "input").unwrap();
        assert_eq!(
            &recorded[input..input + 3],
            &["input", "cancelled", "release"]
        );
        assert_eq!(
            recorded.iter().filter(|event| **event == "input").count(),
            1
        );
        publisher.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn stalled_broadcast_allows_control_and_busy_reply_and_cancels_on_shutdown() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Stall {
            entered: Arc<tokio::sync::Notify>,
            dropped: Arc<AtomicBool>,
            stopped: Arc<AtomicBool>,
        }
        struct RequestGuard(Arc<AtomicBool>);
        impl Drop for RequestGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        impl Broadcast for Stall {
            fn supported(&self) -> bool {
                true
            }
            fn request<'a>(&'a mut self, _: &'a Value) -> BoxFuture<'a, Value> {
                Box::pin(async move {
                    let _guard = RequestGuard(self.dropped.clone());
                    self.entered.notify_one();
                    std::future::pending().await
                })
            }
            fn stop(&mut self) -> BoxFuture<'_, ()> {
                Box::pin(async move {
                    assert!(self.dropped.load(Ordering::SeqCst));
                    self.stopped.store(true, Ordering::SeqCst);
                })
            }
        }
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let options = Options {
            broadcast: Box::new(Stall {
                entered: entered.clone(),
                dropped: dropped.clone(),
                stopped: stopped.clone(),
            }),
            ..Options::default()
        };
        let backend: Backend = Arc::new(|_| {
            Box::pin(async { Ok(json!({"status":"ok","jpeg":"/9j/a","width":1,"height":1})) })
        });
        let (publisher, mut wire) = test_session(backend, options).await;
        wire_send(
            &mut wire,
            json!({"type":"broadcast","viewer_id":"v","surface_id":"desktop","request_id":"b1"}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        wire_send(
            &mut wire,
            json!({"type":"control","viewer_id":"v","data":{"type":"acquire"}}),
        )
        .await;
        assert_eq!(wire_read(&mut wire).await["data"]["type"], "granted");
        wire_send(
            &mut wire,
            json!({"type":"broadcast","viewer_id":"v","surface_id":"desktop","request_id":"b2"}),
        )
        .await;
        let busy = wire_read(&mut wire).await;
        assert_eq!(busy["request_id"], "b2");
        assert_eq!(busy["error"], "busy");
        assert!(!dropped.load(Ordering::SeqCst));
        tokio::time::timeout(Duration::from_secs(1), publisher.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(stopped.load(Ordering::SeqCst));
    }
    #[tokio::test]
    async fn replaced_host_finishes_and_releases_without_reclaiming() {
        check_replaced_host(false).await;
    }
    #[tokio::test]
    async fn replacement_before_initial_publication_returns_a_terminal_handle() {
        check_replaced_host(true).await;
    }
    async fn check_replaced_host(before_publication: bool) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = PublisherTarget::from_attachment(
            &format!(
                "ws://{}/v1/account/tool-host",
                listener.local_addr().unwrap()
            ),
            "test-token",
        )
        .unwrap();
        let releases = Arc::new(AtomicUsize::new(0));
        let observed = releases.clone();
        let backend: Backend = Arc::new(move |input| {
            let releases = observed.clone();
            Box::pin(async move {
                if input["action"] == "release" {
                    releases.fetch_add(1, Ordering::SeqCst);
                }
                Ok(json!({"status":"ok","width":1,"height":1}))
            })
        });
        let (replace, replaced) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut wire = tokio_tungstenite::accept_async(stream).await.unwrap();
            wire.send(Message::Text(
                json!({"type":"ready","connection_id":"test"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let _catalog = wire.next().await.unwrap().unwrap();
            if !before_publication {
                wire.send(Message::Text(
                    json!({"type":"published","generation":"g"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                // Publication starts ICE prefetch, not another publisher. Drain
                // that HTTP socket before asserting no reconnect after fencing.
                let (mut http, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
                let mut bytes = [0; 4096];
                assert!(http.read(&mut bytes).await.unwrap() > 0);
                http.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"iceServers\":[]}").await.unwrap();
                drop(http);
                replaced.await.unwrap();
            }
            wire.close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "Host replaced".into(),
            }))
            .await
            .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(1200), listener.accept())
                    .await
                    .is_err()
            );
        });
        let publisher = Publisher::start(
            &target,
            &Machine::new("test", "Test").unwrap(),
            backend,
            video_options(),
        )
        .await
        .unwrap();
        if !before_publication {
            assert!(!publisher.is_finished());
            replace.send(()).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while !publisher.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        publisher.shutdown().await.unwrap();
        assert!(releases.load(Ordering::SeqCst) > 0);
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn stalled_preparation_allows_established_input_and_lease_processing() {
        let mut preparations = Preparations::new();
        preparations
            .insert(
                "new-viewer",
                tokio::time::Instant::now() + Duration::from_secs(8),
                std::future::pending::<()>(),
            )
            .unwrap();
        let mut lease = Lease::default();
        lease.acquire("established");
        let generation = lease.generation().to_owned();
        for sequence in 1..=3 {
            let event = json!({"kind":"key", "generation":generation, "sequence":sequence});
            tokio::select! {
                biased;
                _ = preparations.next() => panic!("stalled preparation completed"),
                input = std::future::ready(event) => assert!(lease.accept("established", &input)),
            }
        }
        let expired_at = Instant::now() + Duration::from_secs(11);
        tokio::select! {
            biased;
            _ = preparations.next() => panic!("stalled preparation completed"),
            _ = std::future::ready(()) => assert!(lease.expired_at(expired_at)),
        }
        assert!(preparations.contains("new-viewer"));
    }
    #[test]
    fn grant_uses_runtime_capabilities_and_never_infers_from_host_os() {
        let grant = control_grant(
            "lease-generation",
            &json!({"status":"ok","relativePointer":true,"gamepad":true,"secret":"hidden"}),
        );
        assert_eq!(
            grant,
            json!({"type":"granted","generation":"lease-generation","relativePointer":true,"gamepad":true})
        );
        for capabilities in [
            json!({}),
            json!({"status":"unavailable","relativePointer":true}),
            json!({"status":"ok","relativePointer":false}),
        ] {
            assert_eq!(
                control_grant("g", &capabilities),
                json!({"type":"granted","generation":"g"})
            );
        }
    }
    fn video_options() -> Options {
        Options {
            video: Some(Arc::new(|| {
                Box::pin(async {
                    Ok(crate::capture::Capture::packets(
                        futures_util::stream::unfold(
                            tokio::time::interval(Duration::from_millis(20)),
                            |mut tick| async move {
                                tick.tick().await;
                                Some((Ok(bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1])), tick))
                            },
                        ),
                        None,
                    ))
                })
            })),
            ..Options::default()
        }
    }

    #[tokio::test]
    async fn missing_video_source_fails_before_capture_or_publication() {
        let target =
            PublisherTarget::from_attachment("ws://127.0.0.1:1/v1/account/tool-host", "test-token")
                .unwrap();
        let backend: Backend = Arc::new(|_| panic!("no source must not call backend"));
        let result = Publisher::start(
            &target,
            &Machine::new("test", "Test").unwrap(),
            backend,
            Options::default(),
        )
        .await;
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("requires an H.264 video source")
        );
    }

    #[tokio::test]
    async fn failed_video_start_and_capture_recover_without_jpeg_live_view() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = PublisherTarget::from_attachment(
            &format!(
                "ws://{}/v1/account/tool-host",
                listener.local_addr().unwrap()
            ),
            "test-token",
        )
        .unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = attempts.clone();
        let (fail, failed) = oneshot::channel();
        let failure = Arc::new(std::sync::Mutex::new(Some(failed)));
        let source: VideoSource = Arc::new(move || {
            let attempt = observed.fetch_add(1, Ordering::SeqCst);
            let failed = if attempt == 1 {
                failure.lock().unwrap().take()
            } else {
                None
            };
            Box::pin(async move {
                if attempt == 0 {
                    return Err(error("fixture encoder unavailable"));
                }
                Ok(crate::capture::Capture::packets(
                    futures_util::stream::once(async {
                        Ok(bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1]))
                    })
                    .chain(futures_util::stream::once(async move {
                        if let Some(failed) = failed {
                            let _ = failed.await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                        Err(error("fixture encoder stopped"))
                    })),
                    None,
                ))
            })
        });
        let captures = Arc::new(AtomicUsize::new(0));
        let count = captures.clone();
        let backend: Backend = Arc::new(move |input| {
            let captures = count.clone();
            Box::pin(async move {
                if input["action"] == "observe" {
                    captures.fetch_add(1, Ordering::SeqCst);
                }
                Ok(json!({"status":"ok","jpeg":"/9j/a","width":1,"height":1}))
            })
        });
        let peer = tokio::spawn(async move {
            // The failed initial encoder never advertises any surface or frame.
            let (stream, _) = listener.accept().await.unwrap();
            let mut wire = tokio_tungstenite::accept_async(stream).await.unwrap();
            wire_send(&mut wire, json!({"type":"ready","connection_id":"failed"})).await;
            let next = tokio::time::timeout(Duration::from_secs(2), wire.next())
                .await
                .unwrap();
            assert!(matches!(
                next,
                None | Some(Err(_)) | Some(Ok(Message::Close(_)))
            ));
            let (stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut wire = tokio_tungstenite::accept_async(stream).await.unwrap();
            wire_send(
                &mut wire,
                json!({"type":"ready","connection_id":"recovered"}),
            )
            .await;
            let catalog = wire_read(&mut wire).await;
            assert_eq!(catalog["type"], "catalog");
            assert!(catalog["surfaces"][0].get("transport").is_none());
            assert!(catalog["surfaces"][0].get("frame_window").is_none());
            wire_send(&mut wire, json!({"type":"published","generation":"g"})).await;
            // Complete publication's authenticated ICE prefetch independently.
            let (mut http, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            assert!(http.read(&mut bytes).await.unwrap() > 0);
            http.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"iceServers\":[]}").await.unwrap();
            drop(http);
            for viewer in ["v", "stale"] {
                wire_send(
                    &mut wire,
                    json!({"type":"frame_request","viewer_id":viewer,"count":6}),
                )
                .await;
            }
            wire_send(&mut wire, json!({"type":"agent_call","request_id":"snapshot","agent_id":"a","surface_id":"desktop","generation":"g","deadline_at":now_ms()+3000,"input":{"action":"observe"}})).await;
            let snapshot = wire_read(&mut wire).await;
            assert_eq!(snapshot["type"], "agent_result");
            assert_eq!(snapshot["status"], "ok");
            assert_eq!(snapshot["jpeg"], "/9j/a");
            assert_eq!(
                captures.load(Ordering::SeqCst),
                2,
                "only initial capture and on-demand agent observation"
            );
            fail.send(()).unwrap();
            let next = tokio::time::timeout(Duration::from_secs(2), wire.next())
                .await
                .unwrap();
            assert!(matches!(
                next,
                None | Some(Err(_)) | Some(Ok(Message::Close(_)))
            ));
            let (stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut wire = tokio_tungstenite::accept_async(stream).await.unwrap();
            wire_send(
                &mut wire,
                json!({"type":"ready","connection_id":"recovered-again"}),
            )
            .await;
            let catalog = wire_read(&mut wire).await;
            assert_eq!(catalog["type"], "catalog");
            assert!(catalog["surfaces"][0].get("transport").is_none());
            wire_send(&mut wire, json!({"type":"published","generation":"g2"})).await;
            assert_eq!(captures.load(Ordering::SeqCst), 2);
            wire
        });
        let publisher = Publisher::start(
            &target,
            &Machine::new("test", "Test").unwrap(),
            backend,
            Options {
                video: Some(source),
                ..Options::default()
            },
        )
        .await
        .unwrap();
        let _wire = peer.await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        publisher.shutdown().await.unwrap();
    }

    #[test]
    fn gestures_bound_duration_and_release_modifiers() {
        assert!(
            steps(&json!({"action":"drag","x":0,"y":0,"endX":1,"endY":1,"durationMs":1501}))
                .is_err()
        );
        let keys = steps(&json!({"action":"key","key":4,"modifiers":[224,225]})).unwrap();
        assert_eq!(
            keys.last().unwrap().1,
            json!({"kind":"key","key":224,"down":false})
        );
        assert!(steps(&json!({"action":"key","key":4,"modifiers":[224,224]})).is_err());
    }
    #[test]
    fn lease_rejects_replay_and_stale_generation() {
        let mut lease = Lease::default();
        lease.acquire("viewer");
        let generation = lease.generation().to_owned();
        let event = json!({"kind":"button","generation":generation,"sequence":2});
        assert!(lease.accept("viewer", &event));
        assert!(!lease.accept("viewer", &event));
        assert!(!lease.accept(
            "other",
            &json!({"kind":"key","generation":generation,"sequence":3})
        ));
        lease.acquire("viewer");
        assert!(!lease.accept(
            "viewer",
            &json!({"kind":"key","generation":generation,"sequence":3})
        ));
    }
    #[test]
    fn capture_results_are_bounded() {
        assert!(!valid_frame(
            &json!({"jpeg":"/9j/a","width":1281,"height":720})
        ));
        assert_eq!(
            checked_result(json!({"status":"ok","secret":"extra"})),
            json!({"status":"unavailable"})
        );
    }
}

#[cfg(test)]
mod microphone_tests {
    use super::*;

    #[test]
    fn microphone_requires_current_owner_and_well_formed_request() {
        let mut lease = Lease::default();
        lease.acquire("owner");
        let mut control = MicrophoneControl::default();
        control.refreshed(Instant::now());
        let request =
            json!({"generation":lease.generation(),"requestID":"request-1","enabled":true});
        for (viewer, data) in [
            ("other", request.clone()),
            (
                "owner",
                json!({"generation":"stale","requestID":"old","enabled":true}),
            ),
            (
                "owner",
                json!({"generation":lease.generation(),"enabled":true}),
            ),
            (
                "owner",
                json!({"generation":lease.generation(),"requestID":"bad","enabled":"true"}),
            ),
        ] {
            assert!(
                control
                    .request(&lease, viewer, &data, |_, _| panic!("unauthorized apply"))
                    .is_none()
            );
        }
        let ack = control
            .request(&lease, "owner", &request, |enabled, remaining| {
                assert!(enabled);
                assert!(remaining > Duration::ZERO && remaining <= crate::input::LEASE_DURATION);
                true
            })
            .unwrap();
        assert_eq!(ack["data"]["requestID"], "request-1");
        assert_eq!(ack["data"]["generation"], lease.generation());
        assert_eq!(ack["data"]["enabled"], true);
        lease.clear();
        assert!(
            control
                .request(&lease, "owner", &request, |_, _| panic!("revoked apply"))
                .is_none()
        );
        let stopped = control.stopped().unwrap();
        assert_eq!(stopped["data"]["requestID"], "request-1");
        assert_eq!(stopped["data"]["enabled"], false);
        assert!(control.stopped().is_none());
        lease.acquire("owner");
        assert!(
            control
                .request(&lease, "owner", &request, |_, _| panic!(
                    "old generation after reacquire"
                ))
                .is_none()
        );
    }

    #[test]
    fn microphone_apply_failure_mute_and_receiver_failure_preserve_request_identity() {
        let mut lease = Lease::default();
        lease.acquire("owner");
        let mut control = MicrophoneControl::default();
        control.refreshed(Instant::now());
        let mut request =
            json!({"generation":lease.generation(),"requestID":"failed","enabled":true});
        let ack = control
            .request(&lease, "owner", &request, |_, _| false)
            .unwrap();
        assert_eq!(ack["data"]["enabled"], false);
        assert!(control.active.is_none());
        request["requestID"] = json!("live");
        control
            .request(&lease, "owner", &request, |_, _| true)
            .unwrap();
        let stopped = control.stopped().unwrap();
        assert_eq!(stopped["data"]["requestID"], "live");
        assert_eq!(stopped["data"]["generation"], lease.generation());
        assert_eq!(stopped["data"]["enabled"], false);
        control
            .request(&lease, "owner", &request, |_, _| true)
            .unwrap();
        request["requestID"] = json!("mute");
        request["enabled"] = json!(false);
        let ack = control
            .request(&lease, "owner", &request, |enabled, _| {
                assert!(!enabled);
                false
            })
            .unwrap();
        assert_eq!(ack["data"]["requestID"], "mute");
        assert!(control.stopped().is_none());
        lease.acquire_at("owner", Instant::now() - crate::input::LEASE_DURATION);
        request["generation"] = json!(lease.generation());
        assert!(
            control
                .request(&lease, "owner", &request, |_, _| panic!("expired apply"))
                .is_none()
        );
    }
}

#[cfg(test)]
#[path = "runtime_ice_tests.rs"]
mod ice_tests;
