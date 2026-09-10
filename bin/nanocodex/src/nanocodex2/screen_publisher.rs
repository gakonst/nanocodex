//! Rust Hand screen publication. Credentials and signaling remain on the host.
use futures_util::{SinkExt, StreamExt, future::BoxFuture};
use nanocodex_managed::ManagedError;
use nanocodex_tools::attachment::{AttachmentMachine, AttachmentTarget};
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

pub(crate) type ScreenBackend =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, ManagedError>> + Send + Sync>;
pub(crate) struct ScreenPublisher {
    target: watch::Sender<AttachmentTarget>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl Drop for ScreenPublisher {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
impl ScreenPublisher {
    pub(crate) async fn start(
        target: &AttachmentTarget,
        machine: &AttachmentMachine,
        backend: ScreenBackend,
    ) -> Result<Self, ManagedError> {
        endpoint(target)?;
        let first =
            tokio::time::timeout(Duration::from_secs(8), backend(json!({"action":"observe"})))
                .await
                .map_err(|_| error("screen capture timed out"))??;
        if first["status"] != "ok" {
            return Err(error(
                "screen capture is unavailable; check display and OS permissions",
            ));
        }
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
            loop {
                let target = targets.borrow_and_update().clone();
                let result = tokio::select! {
                    _ = &mut stopped => break,
                    changed = targets.changed() => { if changed.is_err() { break; } continue; },
                    result = session(&target, &machine, &backend, dimensions, &mut ready) => result,
                };
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    backend(json!({"action":"release"})),
                )
                .await;
                if matches!(result, Err(SessionError::Replaced)) {
                    break;
                }
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                    changed = targets.changed() => if changed.is_err() { break; },
                }
            }
            let _ =
                tokio::time::timeout(Duration::from_secs(3), backend(json!({"action":"release"})))
                    .await;
        });
        let publisher = Self {
            target: sender,
            stop: Some(stop),
            task: Some(task),
        };
        match tokio::time::timeout(Duration::from_secs(30), waiting).await {
            Ok(Ok(())) => Ok(publisher),
            _ => {
                let _ = publisher.shutdown().await;
                Err(error("Hand screen did not publish within 30 seconds"))
            }
        }
    }
    pub(crate) async fn refresh(&self, target: &AttachmentTarget) -> Result<(), ManagedError> {
        endpoint(target)?;
        self.target
            .send(target.clone())
            .map_err(|_| error("Hand screen publisher has stopped"))
    }
    pub(crate) async fn shutdown(mut self) -> Result<(), ManagedError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        Ok(())
    }
}
fn endpoint(target: &AttachmentTarget) -> Result<Url, ManagedError> {
    let mut url = target.endpoint().clone();
    let path = url
        .path()
        .strip_suffix("/tool-host")
        .filter(|path| *path == "/v1/account" || path.starts_with("/v1/vm-host-attachments/"))
        .ok_or_else(|| error("screen requires an account or allocated VM Hand endpoint"))?;
    let path = format!("{path}/hands");
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error("invalid screen endpoint"));
    }
    let scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => return Err(error("invalid screen transport")),
    };
    url.set_scheme(scheme)
        .map_err(|_| error("invalid screen transport"))?;
    url.set_path(&path);
    Ok(url)
}
#[derive(Debug)]
enum SessionError {
    Closed,
    Replaced,
}
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn send(socket: &mut Socket, value: Value) -> Result<(), SessionError> {
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
async fn completed(job: &mut Option<OwnedJob>) -> Value {
    match job {
        Some(job) => (&mut job.0)
            .await
            .unwrap_or_else(|_| json!({"status":"unavailable"})),
        None => std::future::pending().await,
    }
}
#[derive(Default)]
struct Lease {
    owner: String,
    generation: String,
    deadline: Option<Instant>,
    motion: u64,
    discrete: u64,
}
impl Lease {
    fn expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
    fn acquire(&mut self, owner: &str) {
        *self = Self {
            owner: owner.into(),
            generation: uuid::Uuid::new_v4().to_string(),
            deadline: Some(Instant::now() + Duration::from_secs(10)),
            ..Self::default()
        };
    }
    fn valid(&self, owner: &str, generation: &str) -> bool {
        !self.owner.is_empty()
            && self.owner == owner
            && self.generation == generation
            && !self.expired()
    }
    fn accept(&mut self, owner: &str, value: &Value) -> bool {
        if !self.valid(owner, value["generation"].as_str().unwrap_or("")) {
            return false;
        }
        let Some(sequence) = value["sequence"].as_u64().filter(|v| *v > 0) else {
            return false;
        };
        if value["kind"] == "move" {
            if sequence <= self.motion.max(self.discrete) {
                return false;
            }
            self.motion = sequence;
        } else {
            if sequence <= self.discrete {
                return false;
            }
            self.discrete = sequence;
        }
        true
    }
}
async fn release(
    lease: &mut Lease,
    backend: &ScreenBackend,
    socket: &mut Socket,
) -> Result<(), SessionError> {
    let owner = std::mem::take(&mut lease.owner);
    *lease = Lease::default();
    let _ =
        tokio::time::timeout(Duration::from_secs(3), backend(json!({"action":"release"}))).await;
    if !owner.is_empty() && !owner.starts_with("agent:") {
        send(
            socket,
            json!({"type":"control","viewer_id":owner,"data":{"type":"revoked"}}),
        )
        .await?;
    }
    Ok(())
}
async fn session(
    target: &AttachmentTarget,
    machine: &AttachmentMachine,
    backend: &ScreenBackend,
    dimensions: (u64, u64),
    ready: &mut Option<oneshot::Sender<()>>,
) -> Result<(), SessionError> {
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
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| SessionError::Closed)?
    .map_err(|_| SessionError::Closed)?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| SessionError::Closed)?;
    let mut renew_url = base.clone();
    renew_url.set_path(&format!("{}/renew", base.path()));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_renewal = Instant::now();
    let mut last_authorized = Instant::now();
    let mut renewal: Option<BoxFuture<'static, bool>> = None;
    let mut connection = String::new();
    let mut generation = String::new();
    let mut viewers = HashSet::<String>::new();
    let mut pending_frames = HashSet::<String>::new();
    let mut lease = Lease::default();
    let mut job = None;
    let mut frame = None;
    let mut request_id = String::new();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if lease.expired() { release(&mut lease,backend,&mut socket).await?; }
                if last_authorized.elapsed()>Duration::from_secs(25) { return Err(SessionError::Closed); }
                if !connection.is_empty() && last_renewal.elapsed()>=Duration::from_secs(10) && renewal.is_none() {
                    last_renewal=Instant::now(); let http=http.clone(); let url=renew_url.clone(); let token=target.bearer().to_string(); let id=connection.clone();
                    renewal=Some(Box::pin(async move { http.post(url).bearer_auth(token).json(&json!({"connection_id":id})).send().await.is_ok_and(|r|r.status().is_success()) }));
                }
            },
            ok = async { match &mut renewal { Some(future)=>future.await,None=>std::future::pending().await } } => {
                renewal=None; if !ok { return Err(SessionError::Closed); } last_authorized=Instant::now();
            },
            result = completed(&mut job) => {
                job=None;
                if lease.owner.starts_with("agent:") { release(&mut lease,backend,&mut socket).await?; }
                let mut result=checked_result(result); result["type"]=json!("agent_result"); result["request_id"]=json!(std::mem::take(&mut request_id));
                send(&mut socket,result).await?;
            },
            result = completed(&mut frame) => {
                frame=None;
                for viewer in pending_frames.drain() {
                    if !viewers.contains(&viewer) {continue;}
                    if result["status"]=="ok" && valid_frame(&result) {
                        send(&mut socket,json!({"type":"frame","viewer_id":viewer,"jpeg":result["jpeg"],"width":result["width"],"height":result["height"]})).await?;
                    } else { send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?; }
                }
            },
            message = socket.next() => {
                let message=message.ok_or(SessionError::Closed)?.map_err(|_|SessionError::Closed)?;
                let text=match message {
                    Message::Text(text)=>text,
                    Message::Ping(bytes)=>{socket.send(Message::Pong(bytes)).await.map_err(|_|SessionError::Closed)?;continue;},
                    Message::Close(close)=>return Err(if close.is_some_and(|c|c.reason=="Host replaced") {SessionError::Replaced}else{SessionError::Closed}),
                    Message::Pong(_)=>continue,
                    _=>return Err(SessionError::Closed),
                };
                let value:Value=serde_json::from_str(&text).map_err(|_|SessionError::Closed)?;
                let viewer=value["viewer_id"].as_str().unwrap_or("");
                match value["type"].as_str().unwrap_or("") {
                    "ready"=>{
                        if !connection.is_empty(){return Err(SessionError::Closed);}
                        connection=value["connection_id"].as_str().filter(|s|!s.is_empty()).ok_or(SessionError::Closed)?.into();
                        send(&mut socket,json!({"type":"catalog","machine_id":machine.id(),"machine_name":machine.name(),"surfaces":[{"id":"desktop","name":"Desktop","kind":if base.path().starts_with("/v1/vm-host-attachments/"){"vm"}else{"desktop"},"width":dimensions.0,"height":dimensions.1,"controllable":true,"agent_tools":true,"transport":"frames-v1"}]})).await?;
                    },
                    "published"=>{generation=value["generation"].as_str().ok_or(SessionError::Closed)?.into();if let Some(ready)=ready.take(){let _=ready.send(());}},
                    "renewed"=>last_authorized=Instant::now(),
                    "pong"=>{},
                    "viewer"=>{
                        if viewer.is_empty() || value["surface_id"]!="desktop" {return Err(SessionError::Closed);}
                        if viewers.len()>=4 {send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;}else{viewers.insert(viewer.into());}
                    },
                    "viewer_left"=>{viewers.remove(viewer);pending_frames.remove(viewer);if lease.owner==viewer{release(&mut lease,backend,&mut socket).await?;}},
                    "frame_request" if viewers.contains(viewer)=>{
                        pending_frames.insert(viewer.into());
                        if frame.is_none(){let backend=backend.clone();frame=Some(OwnedJob(tokio::spawn(async move{call(&backend,json!({"action":"observe"}),Duration::from_secs(5)).await})));}
                    },
                    "control" if viewers.contains(viewer)=>{
                        let data=&value["data"];
                        match data["type"].as_str().unwrap_or("") {
                            "acquire" if data.get("generation").is_none()=>{
                                // A human cancels an agent before receiving the input lease.
                                if lease.owner.starts_with("agent:") || lease.expired() {
                                    if job.take().is_some(){send(&mut socket,json!({"type":"agent_result","request_id":std::mem::take(&mut request_id),"status":"cancelled"})).await?;}
                                    release(&mut lease,backend,&mut socket).await?;
                                }
                                if lease.owner.is_empty(){release(&mut lease,backend,&mut socket).await?;lease.acquire(viewer);send(&mut socket,json!({"type":"control","viewer_id":viewer,"data":{"type":"granted","generation":lease.generation}})).await?;}
                                else{send(&mut socket,json!({"type":"control","viewer_id":viewer,"data":{"type":"denied"}})).await?;}
                            },
                            "renew" if lease.valid(viewer,data["generation"].as_str().unwrap_or(""))=>lease.deadline=Some(Instant::now()+Duration::from_secs(10)),
                            "release" if lease.valid(viewer,data["generation"].as_str().unwrap_or(""))=>release(&mut lease,backend,&mut socket).await?,
                            _=>{send(&mut socket,json!({"type":"close_viewer","viewer_id":viewer})).await?;if lease.owner==viewer{release(&mut lease,backend,&mut socket).await?;}viewers.remove(viewer);},
                        }
                    },
                    "input" if viewers.contains(viewer)=>{
                        if lease.accept(viewer,&value["data"]){
                            let mut input=value["data"].clone(); if let Some(input)=input.as_object_mut(){input.remove("generation");input.remove("sequence");}
                            let result=call(backend,json!({"action":"input","input":input}),Duration::from_secs(2)).await;
                            if result["status"]!="ok" {release(&mut lease,backend,&mut socket).await?;}
                        }
                    },
                    "agent_cancel"=>{if value["request_id"]==request_id && job.take().is_some(){release(&mut lease,backend,&mut socket).await?;send(&mut socket,json!({"type":"agent_result","request_id":std::mem::take(&mut request_id),"status":"cancelled"})).await?;}},
                    "agent_call"=>{
                        let id=value["request_id"].as_str().unwrap_or("");let action=&value["input"];let now=now_ms();let deadline=value["deadline_at"].as_u64().unwrap_or(0);
                        let owner=format!("agent:{}",value["agent_id"].as_str().unwrap_or(""));
                        let status=if value["surface_id"]!="desktop" || value["generation"]!=generation || !valid_id(id) || !valid_id(value["agent_id"].as_str().unwrap_or("")) || deadline<=now || deadline>now+10_000 {Some("invalid")}
                        else if job.is_some() || (!lease.owner.is_empty() && !lease.expired() && action["action"]!="observe" && lease.owner!=owner) {Some("busy")} else {None};
                        if let Some(status)=status{send(&mut socket,json!({"type":"agent_result","request_id":id,"status":status})).await?;continue;}
                        if action["action"]=="release" {if lease.owner==owner{release(&mut lease,backend,&mut socket).await?;}send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"ok"})).await?;continue;}
                        let steps=match steps(action){Ok(steps)=>steps,Err(())=>{send(&mut socket,json!({"type":"agent_result","request_id":id,"status":"invalid"})).await?;continue;}};
                        if !steps.is_empty(){release(&mut lease,backend,&mut socket).await?;lease.acquire(&owner);}
                        let backend=backend.clone();request_id=id.into();
                        job=Some(OwnedJob(tokio::spawn(async move{
                            tokio::time::timeout(Duration::from_millis(deadline.saturating_sub(now_ms())),async{
                                for (delay,input) in steps {if !delay.is_zero(){tokio::time::sleep(delay).await;}let result=call(&backend,json!({"action":"input","input":input}),Duration::from_secs(2)).await;if result["status"]!="ok"{return result;}}
                                tokio::time::sleep(Duration::from_millis(80)).await;
                                call(&backend,json!({"action":"observe"}),Duration::from_secs(4)).await
                            }).await.unwrap_or_else(|_|json!({"status":"cancelled"}))
                        })));
                    },
                    "frame_request"|"input"|"control"=>{},
                    _=>return Err(SessionError::Closed),
                }
            },
        }
    }
}
async fn call(backend: &ScreenBackend, input: Value, timeout: Duration) -> Value {
    match tokio::time::timeout(timeout, backend(input)).await {
        Ok(Ok(value)) => value,
        _ => json!({"status":"unavailable"}),
    }
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
fn checked_result(value: Value) -> Value {
    let status = value["status"].as_str().unwrap_or("unavailable");
    if status == "ok" && valid_frame(&value) {
        json!({"status":"ok","jpeg":value["jpeg"],"width":value["width"],"height":value["height"]})
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
fn error(value: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(value.to_string())
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
    use super::*;
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
        let generation = lease.generation.clone();
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
