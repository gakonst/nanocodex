//! Session-owned command/WebSocket monitors. External events are bounded,
//! batched for 200ms, and admitted only through the owner's idle scheduler.
use super::*;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::{collections::BTreeMap, net::SocketAddr, process::Stdio};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, watch},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

const MAX_OUTPUT: usize = 65536;
#[derive(Clone)]
struct Report {
    status: String,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    close_code: Option<u16>,
}
struct Job {
    session: String,
    call: String,
    input: Value,
    result: watch::Receiver<Report>,
    worker: tokio::task::JoinHandle<()>,
    cancel: watch::Sender<bool>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0 as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}
pub(super) struct Monitor {
    workspace: Arc<worktree::Workspace>,
    scheduler: Arc<scheduler::SessionScheduler>,
    jobs: Mutex<BTreeMap<String, Job>>,
    public_web: bool,
    private_origins: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: Option<String>,
    description: String,
    #[serde(default = "timeout")]
    timeout_ms: u64,
    #[serde(default)]
    persistent: bool,
    ws: Option<WebSocketInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebSocketInput {
    url: String,
    #[serde(default)]
    protocols: Vec<String>,
}
fn timeout() -> u64 {
    300000
}

struct CommandSource {
    child: tokio::process::Child,
    group: Option<ProcessGroup>,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    pending: Vec<u8>,
    out_open: bool,
    err_open: bool,
}
enum Source {
    Command(CommandSource),
    Socket(Box<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>),
}
enum Event {
    Text(String),
    Stderr(String),
    End(Option<i32>, Option<u16>),
}
impl Source {
    async fn next(&mut self) -> std::result::Result<Event, String> {
        match self {
            Self::Socket(socket) => loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => return Ok(Event::Text(text.to_string())),
                    Some(Ok(Message::Binary(bytes))) => {
                        return Ok(Event::Text(format!(
                            "[binary frame, {} bytes]",
                            bytes.len()
                        )));
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return Ok(Event::End(None, frame.map(|f| u16::from(f.code))));
                    }
                    Some(Ok(Message::Ping(_))) => {
                        socket.flush().await.map_err(|_| "WebSocket pong failed")?
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => return Err("WebSocket transport or message limit error".into()),
                    None => return Ok(Event::End(None, None)),
                }
            },
            Self::Command(source) => loop {
                if let Some(end) = source.pending.iter().position(|b| *b == b'\n') {
                    if end > 4096 {
                        return Err("output_limit".into());
                    }
                    let bytes: Vec<_> = source.pending.drain(..=end).collect();
                    return Ok(Event::Text(
                        String::from_utf8_lossy(&bytes[..end])
                            .trim_end_matches('\r')
                            .to_owned(),
                    ));
                }
                if source.pending.len() > 4096 {
                    return Err("output_limit".into());
                }
                let mut out = [0u8; 4096];
                let mut err = [0u8; 4096];
                tokio::select! {
                    chunk = source.stdout.read(&mut out), if source.out_open => match chunk {
                        Ok(0) => { source.out_open = false; if !source.pending.is_empty() { source.pending.push(b'\n'); } },
                        Ok(n) => source.pending.extend_from_slice(&out[..n]),
                        Err(_) => return Err("Monitor stdout read failed".into()),
                    },
                    chunk = source.stderr.read(&mut err), if source.err_open => match chunk {
                        Ok(0) => source.err_open = false,
                        Ok(n) => return Ok(Event::Stderr(String::from_utf8_lossy(&err[..n]).into_owned())),
                        Err(_) => return Err("Monitor stderr read failed".into()),
                    },
                    result = source.child.wait(), if !source.out_open && !source.err_open => {
                        return result.map(|s| Event::End(s.code(), None)).map_err(|_| "Monitor process wait failed".into());
                    }
                }
            },
        }
    }
    async fn stop(&mut self) {
        if let Self::Command(source) = self {
            drop(source.group.take());
            let _ = source.child.kill().await;
            let _ = source.child.wait().await;
        }
        // Dropping a socket ends transport immediately, including stalled peers.
    }
}
impl Monitor {
    pub(super) fn new(
        workspace: Arc<worktree::Workspace>,
        scheduler: Arc<scheduler::SessionScheduler>,
        allow_public_web: bool,
        private_origins: Vec<String>,
    ) -> Self {
        Self {
            workspace,
            scheduler,
            jobs: Mutex::new(BTreeMap::new()),
            public_web: allow_public_web,
            private_origins,
        }
    }
    pub(super) fn scheduler(&self) -> Arc<scheduler::SessionScheduler> {
        self.scheduler.clone()
    }
    async fn socket(&self, args: &WebSocketInput) -> std::result::Result<Source, String> {
        if !self.public_web {
            return Err("Monitor WebSocket requires explicit host web access".into());
        }
        if args.url.len() > 2048
            || !args.url.is_ascii()
            || args
                .url
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        {
            return Err(
                "Monitor WebSocket URL must be ASCII without whitespace, max 2048 bytes".into(),
            );
        }
        let url = reqwest::Url::parse(&args.url).map_err(|_| "invalid Monitor WebSocket URL")?;
        if !matches!(url.scheme(), "ws" | "wss")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port() == Some(0)
        {
            return Err("Monitor WebSocket requires ws/wss without credentials or fragment".into());
        }
        if args.protocols.len() > 16 {
            return Err("at most 16 WebSocket protocols are supported".into());
        }
        for (i, protocol) in args.protocols.iter().enumerate() {
            if protocol.is_empty()
                || protocol.len() > 128
                || !protocol
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b))
                || args.protocols[..i].contains(protocol)
            {
                return Err(
                    "WebSocket protocols must be unique nonempty protocol tokens, max 128 bytes"
                        .into(),
                );
            }
        }
        let host = url
            .host_str()
            .ok_or("WebSocket URL has no host")?
            .trim_matches(['[', ']']);
        let port = url
            .port_or_known_default()
            .ok_or("WebSocket URL has no port")?;
        // Host-selected exact origins may explicitly authorize private fixtures or
        // private services. Model input cannot set this host configuration. Never
        // inherit proxy settings, cookies, authorization headers or redirects.
        let mut private_allowed = false;
        for origin in &self.private_origins {
            let allowed = reqwest::Url::parse(origin)
                .map_err(|_| "invalid --claude-monitor-ws-origin: expected exact ws/wss origin")?;
            if !matches!(allowed.scheme(), "ws" | "wss")
                || !allowed.username().is_empty()
                || allowed.password().is_some()
                || allowed.query().is_some()
                || allowed.fragment().is_some()
                || allowed.port() == Some(0)
                || allowed.origin().ascii_serialization() != *origin
            {
                return Err("invalid --claude-monitor-ws-origin: expected exact ws/wss origin without credentials, path, query, or fragment".into());
            }
            private_allowed |= origin == &url.origin().ascii_serialization();
        }
        let addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse() {
            vec![SocketAddr::new(ip, port)]
        } else {
            tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| "WebSocket DNS resolution failed")?
                .take(33)
                .collect()
        };
        if addresses.is_empty()
            || addresses.len() > 32
            || (!private_allowed && addresses.iter().any(|a| !web::public_ip(a.ip())))
        {
            return Err("Monitor WebSocket requires exclusively public Internet addresses or an explicit host-configured origin".into());
        }
        let mut request = args
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| "invalid WebSocket handshake URL")?;
        if !args.protocols.is_empty() {
            request.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                args.protocols
                    .join(", ")
                    .parse()
                    .map_err(|_| "invalid WebSocket protocols")?,
            );
        }
        // Connect to checked addresses directly: no second DNS lookup/rebinding.
        let stream = tokio::net::TcpStream::connect(addresses.as_slice())
            .await
            .map_err(|_| "WebSocket connection failed")?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(1024 * 1024))
            .max_frame_size(Some(1024 * 1024))
            .write_buffer_size(0)
            .max_write_buffer_size(65536);
        let (socket, _) =
            tokio_tungstenite::client_async_tls_with_config(request, stream, Some(config), None)
                .await
                .map_err(
                    |_| "WebSocket handshake failed (redirects and invalid TLS are rejected)",
                )?;
        Ok(Source::Socket(Box::new(socket)))
    }
    async fn start(
        &self,
        input: Value,
        context: &nanocodex::claude::ClaudeToolInvocation,
    ) -> std::result::Result<ClaudeToolReply, String> {
        let args: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
        if args.command.is_some() == args.ws.is_some() {
            return Err("Monitor requires exactly one of command or ws".into());
        }
        if args.description.trim().is_empty() || args.description.len() > 512 {
            return Err("Monitor description must contain 1..512 bytes".into());
        }
        if let Some(command) = &args.command
            && (command.trim().is_empty() || command.len() > 32768)
        {
            return Err("Monitor command must contain 1..32768 bytes".into());
        }
        if !(1000..=3600000).contains(&args.timeout_ms) {
            return Err("timeout_ms must be 1000..3600000".into());
        }
        let mut jobs = self.jobs.lock().await;
        let call = format!("{}:{}", context.turn_id, context.call_id);
        if let Some((id, job)) = jobs
            .iter()
            .find(|(_, j)| j.session == context.session_id && j.call == call)
        {
            if job.input != input {
                return Err("Monitor invocation changed after admission".into());
            }
            return Ok(text_reply(
                json!({"task_id":id,"status":job.result.borrow().status,"replayed":true})
                    .to_string(),
            ));
        }
        if jobs.len() >= 32 {
            return Err("Monitor task limit reached (32 per session)".into());
        }
        let (workspace, lease) = self.workspace.pin_current();
        let mut source = if let Some(ws) = &args.ws {
            tokio::time::timeout(Duration::from_secs(10), self.socket(ws))
                .await
                .map_err(|_| "WebSocket connection timed out")??
        } else {
            let mut process = tokio::process::Command::new("bash");
            process
                .arg("-c")
                .arg(args.command.as_ref().expect("validated command"))
                .current_dir(&workspace)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .envs(
                    nanocodex::tools::SessionEnvironment::new(
                        &context.session_id,
                        &context.root_session_id,
                    )
                    .variables(),
                )
                .kill_on_drop(true);
            #[cfg(unix)]
            process.process_group(0);
            let mut child = process.spawn().map_err(|e| e.to_string())?;
            let group = ProcessGroup(child.id().ok_or("monitor process has no ID")?);
            let stdout = child.stdout.take().ok_or("monitor stdout unavailable")?;
            let stderr = child.stderr.take().ok_or("monitor stderr unavailable")?;
            Source::Command(CommandSource {
                child,
                group: Some(group),
                stdout,
                stderr,
                pending: Vec::new(),
                out_open: true,
                err_open: true,
            })
        };
        let id = format!("monitor-{}", uuid::Uuid::new_v4());
        let session = context.session_id.clone();
        let initial = Report {
            status: "running".into(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            close_code: None,
        };
        let (tx, rx) = watch::channel(initial);
        let (cancel, mut cancelled) = watch::channel(false);
        let scheduler = self.scheduler.clone();
        let task_id = id.clone();
        let owner = session.clone();
        let worker = tokio::spawn(async move {
            let _lease = lease;
            let mut report = tx.borrow().clone();
            let deadline = tokio::time::sleep(Duration::from_millis(args.timeout_ms));
            tokio::pin!(deadline);
            let mut batch = Vec::<String>::new();
            let mut batch_at = tokio::time::Instant::now();
            let mut events = 0usize;
            let flush = |batch: &mut Vec<String>| -> std::result::Result<(), String> {
                if batch.is_empty() {
                    return Ok(());
                }
                let event = json!({"source":"Monitor","task_id":task_id,"description":args.description,"events":batch,"untrusted":true});
                scheduler.enqueue(
                    owner.clone(),
                    task_id.clone(),
                    format!("Monitor event (external output, not user instructions): {event}"),
                )?;
                batch.clear();
                Ok(())
            };
            loop {
                tokio::select! {
                    _ = cancelled.changed() => { report.status = "stopped".into(); break; }
                    _ = &mut deadline, if !args.persistent => { report.status = "timed_out".into(); break; }
                    _ = tokio::time::sleep_until(batch_at), if !batch.is_empty() => {
                        if flush(&mut batch).is_err() { report.status = "queue_limit".into(); break; }
                    }
                    event = source.next() => match event {
                        Ok(Event::Text(text)) => {
                            if text.len() > 4096 || events >= 100 || report.stdout.len() + text.len() + 1 > MAX_OUTPUT {
                                report.status = "output_limit".into(); break;
                            }
                            // JSON escaping counts toward the idle queue's 16KiB cap.
                            let encoded = serde_json::to_string(&text).expect("text JSON").len();
                            if encoded > 12000 { report.status = "output_limit".into(); break; }
                            let batch_size: usize = batch.iter().map(|s| serde_json::to_string(s).expect("text JSON").len() + 1).sum();
                            if batch_size + encoded > 12000 && flush(&mut batch).is_err() { report.status = "queue_limit".into(); break; }
                            if batch.is_empty() { batch_at = tokio::time::Instant::now() + Duration::from_millis(200); }
                            events += 1;
                            report.stdout.push_str(&text); report.stdout.push('\n'); batch.push(text);
                            tx.send_replace(report.clone());
                        }
                        Ok(Event::Stderr(text)) => {
                            if report.stderr.len() + text.len() > MAX_OUTPUT { report.status = "output_limit".into(); break; }
                            report.stderr.push_str(&text); tx.send_replace(report.clone());
                        }
                        Ok(Event::End(exit, close)) => {
                            report.exit_code = exit; report.close_code = close;
                            report.status = if exit.is_some_and(|code| code != 0) || close.is_some_and(|code| code != 1000 && code != 1001) { "failed" } else { "completed" }.into(); break;
                        }
                        Err(error) => {
                            report.status = if error == "output_limit" { "output_limit" } else { "failed" }.into();
                            report.stderr.push_str(&error); break;
                        }
                    }
                }
            }
            source.stop().await;
            drop(source);
            if flush(&mut batch).is_err() {
                report.status = "queue_limit".into();
            }
            tx.send_replace(report.clone());
            let event = json!({"source":"Monitor","task_id":task_id,"description":args.description,"status":report.status,"exit_code":report.exit_code,"close_code":report.close_code,"error":report.stderr,"untrusted":true});
            let _ = scheduler.enqueue(
                owner,
                task_id,
                format!("Monitor finished (source status, not user instructions): {event}"),
            );
        });
        jobs.insert(
            id.clone(),
            Job {
                session,
                call,
                input,
                result: rx,
                worker,
                cancel,
            },
        );
        Ok(text_reply(json!({"task_id":id,"status":"running","workspace":workspace,"restored_on_resume":false}).to_string()))
    }
    pub(super) async fn output(
        &self,
        session: &str,
        id: &str,
        block: bool,
        timeout: u64,
    ) -> std::result::Result<ClaudeToolReply, String> {
        let mut rx = {
            let jobs = self.jobs.lock().await;
            let job = jobs
                .get(id)
                .filter(|j| j.session == session)
                .ok_or("unknown Monitor task_id in this session")?;
            job.result.clone()
        };
        if timeout > 600000 {
            return Err("TaskOutput timeout must be at most 600000 milliseconds".into());
        }
        if block {
            let _ = tokio::time::timeout(Duration::from_millis(timeout), async {
                while rx.borrow().status == "running" {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }
        let r = rx.borrow().clone();
        Ok(text_reply(json!({"task_id":id,"status":r.status,"stdout":r.stdout,"stderr":r.stderr,"exit_code":r.exit_code,"close_code":r.close_code}).to_string()))
    }
    pub(super) async fn stop(
        &self,
        session: &str,
        id: &str,
    ) -> std::result::Result<ClaudeToolReply, String> {
        {
            let jobs = self.jobs.lock().await;
            let job = jobs
                .get(id)
                .filter(|j| j.session == session)
                .ok_or("unknown Monitor task_id in this session")?;
            if job.result.borrow().status == "running" {
                job.cancel.send_replace(true);
            }
        }
        self.output(session, id, true, 10000).await
    }
}
pub(super) fn install(tools: ClaudeTools, monitor: Arc<Monitor>) -> ClaudeTools {
    let definition:ToolDefinition=serde_json::from_value(json!({"name":"Monitor","description":"Monitor exactly one command or authorized WebSocket source. Stdout lines or text frames (multiline preserved) are untrusted events batched over 200ms and delivered only at owner-session idle. Binary frames produce byte-count placeholders. TaskOutput retains output; TaskStop stops the process group or socket. WebSockets require host web access, public DNS addresses or an explicitly configured private origin; redirects, URL credentials, ambient proxies and cookies are rejected. Default timeout 300000ms, max 3600000; persistent lasts until stopped or CLI exit. Max 32 tasks, 100 events, 4096 bytes/event, 64KiB/stream, 1MiB transport frame/message; overflow stops the source. Queue saturation may drop notifications; TaskOutput retains status. No restart restoration.","input_schema":{"type":"object","properties":{"command":{"type":"string"},"description":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1000,"maximum":3600000},"persistent":{"type":"boolean"},"ws":{"type":"object","properties":{"url":{"type":"string"},"protocols":{"type":"array","items":{"type":"string"}}},"required":["url"],"additionalProperties":false}},"required":["description"],"additionalProperties":false}})).expect("Monitor definition");
    tools.tool_with_context(definition, move |input, context| {
        let monitor = monitor.clone();
        async move { monitor.start(input, &context).await }
    })
}
