//! Private HLS upload sink for portable playback links.
//! Upload URLs and bearer tokens never enter logs, status values or errors.
use serde_json::Value;
use std::{collections::VecDeque, path::Path, time::Duration};
use url::Url;

pub(crate) const MAX_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PLAYLIST_BYTES: usize = 16 * 1024;
pub(crate) const WINDOW: usize = 6;
pub(crate) const SEGMENT_SECONDS: u32 = 2;
const MAX_LIFETIME_MS: u64 = 8 * 3600 * 1000 + 60_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
pub(crate) const LOCAL_PLAYLIST: &str = "local.m3u8";

#[derive(Clone)]
pub(crate) struct Target {
    pub stream_id: String,
    base: Url,
    token: String,
    pub expires_at: u64,
}
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("stream_id", &self.stream_id)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
/// Server-issued playback stream ID: `sp_` followed by 32 lowercase hex digits.
pub(crate) fn valid_stream_id(value: &str) -> bool {
    value.len() == 35
        && value.starts_with("sp_")
        && value[3..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
/// Exact broker result shape: `type,target,request_id,stream_id,status[,error]`.
pub(crate) fn result(
    request_id: &Value,
    stream_id: &str,
    status: &str,
    error: Option<&str>,
) -> Value {
    let mut value = serde_json::json!({"type":"broadcast_result","target":"hls",
        "request_id":request_id,"stream_id":stream_id,"status":status});
    if let Some(error) = error {
        value["error"] = error.into();
    }
    value
}
fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(host)) => host == "localhost",
        None => false,
    }
}
/// Upload base URL: HTTPS (or loopback HTTP for local tests), path ending in `/`,
/// no credentials, query or fragment. The caller additionally pins its origin.
pub(crate) fn valid_upload_url(value: &str) -> Option<Url> {
    if value.len() > 2048
        || value
            .bytes()
            .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
    {
        return None;
    }
    let url = Url::parse(value).ok()?;
    let scheme = match url.scheme() {
        "https" => true,
        "http" => loopback(&url),
        _ => false,
    };
    (scheme
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port() != Some(0)
        && url.path().ends_with('/')
        && url.path().len() > 1)
        .then_some(url)
}
fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'~' | b'-'))
}
/// Parse a broker `start` request. `origin` is the authenticated publisher origin.
pub(crate) fn parse_start(
    request: &Value,
    origin: &url::Origin,
    now: u64,
) -> Result<Target, &'static str> {
    let stream_id = request["stream_id"]
        .as_str()
        .filter(|s| valid_stream_id(s))
        .ok_or("invalid_request")?;
    let upload = &request["upload"];
    let base = upload["url"]
        .as_str()
        .and_then(valid_upload_url)
        .ok_or("invalid_request")?;
    // Exact authenticated publisher origin and the stream's own upload path.
    if &base.origin() != origin || base.path() != format!("/v1/screen-playback/{stream_id}/upload/")
    {
        return Err("invalid_request");
    }
    let token = upload["token"]
        .as_str()
        .filter(|s| valid_token(s))
        .ok_or("invalid_request")?;
    let expires_at = upload["expires_at"].as_u64().ok_or("invalid_request")?;
    if expires_at <= now {
        return Err("expired");
    }
    if expires_at > now + MAX_LIFETIME_MS {
        return Err("invalid_request");
    }
    Ok(Target {
        stream_id: stream_id.into(),
        base,
        token: token.into(),
        expires_at,
    })
}

struct Entry {
    n: u64,
    duration: f64,
    discontinuity: bool,
}
enum Put {
    Ok,
    Drop,
    Missing(Vec<u64>),
}
/// Uploads completed local segments, then a manifest naming only accepted ones.
pub(crate) struct Uploader {
    target: Target,
    http: reqwest::Client,
    window: VecDeque<Entry>,
    pub next_n: u64,
    media_sequence: u64,
    discontinuity_sequence: u64,
    pending_discontinuity: bool,
    failures: u32,
}
impl Uploader {
    pub fn new(target: Target) -> Result<Self, &'static str> {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| "broadcast_failed")?;
        Ok(Self {
            target,
            http,
            window: VecDeque::new(),
            next_n: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            pending_discontinuity: false,
            failures: 0,
        })
    }
    /// A restarted encoder resets timestamps; mark the next accepted segment.
    pub fn restart(&mut self) {
        if !self.window.is_empty() {
            self.pending_discontinuity = true;
        }
    }
    async fn put(
        &self,
        name: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Put, &'static str> {
        let url = self
            .target
            .base
            .join(name)
            .map_err(|_| "broadcast_failed")?;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
            }
            if now_ms() >= self.target.expires_at {
                return Err("expired");
            }
            let response = self
                .http
                .put(url.clone())
                .bearer_auth(&self.target.token)
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body.clone())
                .send()
                .await;
            let Ok(response) = response else { continue };
            match response.status().as_u16() {
                200..=299 => return Ok(Put::Ok),
                401 | 403 | 404 | 410 | 413 => return Err("upload_rejected"),
                408 | 429 | 500..=599 => continue,
                409 => {
                    let body: Value = response.json().await.unwrap_or_default();
                    if body["error"] == "missing_segments" {
                        let missing = body["missing"].as_array().map(|values| {
                            values.iter().filter_map(Value::as_u64).take(64).collect()
                        });
                        return Ok(Put::Missing(missing.unwrap_or_default()));
                    }
                    return Ok(Put::Drop);
                }
                _ => return Ok(Put::Drop),
            }
        }
        Ok(Put::Drop)
    }
    fn failure(&mut self) -> Result<(), &'static str> {
        self.failures += 1;
        if self.failures >= MAX_CONSECUTIVE_FAILURES {
            return Err("broadcast_failed");
        }
        Ok(())
    }
    fn manifest(&self) -> String {
        use std::fmt::Write;
        let target = self
            .window
            .iter()
            .map(|e| e.duration.ceil() as u32)
            .max()
            .unwrap_or(SEGMENT_SECONDS)
            .max(SEGMENT_SECONDS);
        let mut text = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-DISCONTINUITY-SEQUENCE:{}\n",
            self.media_sequence, self.discontinuity_sequence
        );
        for entry in &self.window {
            if entry.discontinuity {
                text.push_str("#EXT-X-DISCONTINUITY\n");
            }
            let _ = write!(text, "#EXTINF:{:.3},\ns{}.ts\n", entry.duration, entry.n);
        }
        text
    }
    async fn segment(&self, dir: &Path, n: u64) -> Result<Put, &'static str> {
        match tokio::fs::read(dir.join(format!("s{n}.ts"))).await {
            Ok(bytes) if bytes.len() as u64 <= MAX_SEGMENT_BYTES => {
                self.put(&format!("s{n}.ts"), "video/mp2t", bytes).await
            }
            _ => Ok(Put::Drop),
        }
    }
    /// Publish the manifest; a server that lost its RAM buffer names missing
    /// segments, which are refilled from the retained window before one resend.
    /// `Ok(true)` only when the server accepted the playlist; a rejected
    /// playlist counts toward the consecutive-failure limit.
    async fn publish(&mut self, dir: &Path) -> Result<bool, &'static str> {
        let manifest = self.manifest();
        if manifest.len() > MAX_PLAYLIST_BYTES {
            return Err("broadcast_failed");
        }
        let missing = match self
            .put(
                "index.m3u8",
                "application/vnd.apple.mpegurl",
                manifest.into_bytes(),
            )
            .await?
        {
            Put::Ok => return Ok(true),
            Put::Drop => return self.failure().map(|()| false),
            Put::Missing(missing) => missing,
        };
        let mut lost = Vec::new();
        for entry in &self.window {
            if (missing.is_empty() || missing.contains(&entry.n))
                && !matches!(self.segment(dir, entry.n).await?, Put::Ok)
            {
                lost.push(entry.n);
            }
        }
        // Never advertise a segment the server does not hold: keep the newest
        // contiguous run after the last unrecoverable entry.
        if let Some(last) = lost.iter().max() {
            while self.window.front().is_some_and(|e| e.n <= *last) {
                self.pop(dir);
            }
            if let Some(front) = self.window.front_mut() {
                if !front.discontinuity {
                    front.discontinuity = true;
                }
            }
        }
        if self.window.is_empty() {
            return self.failure().map(|()| false);
        }
        match self
            .put(
                "index.m3u8",
                "application/vnd.apple.mpegurl",
                self.manifest().into_bytes(),
            )
            .await?
        {
            Put::Ok => Ok(true),
            _ => self.failure().map(|()| false),
        }
    }
    fn pop(&mut self, dir: &Path) {
        if let Some(entry) = self.window.pop_front() {
            self.media_sequence += 1;
            if entry.discontinuity {
                self.discontinuity_sequence += 1;
            }
            let _ = std::fs::remove_file(dir.join(format!("s{}.ts", entry.n)));
        }
    }
    /// Upload each newly completed local segment in order. Returns whether any
    /// segment was accepted. Errors are protocol codes only.
    pub async fn poll(&mut self, dir: &Path) -> Result<bool, &'static str> {
        let Ok(text) = tokio::fs::read_to_string(dir.join(LOCAL_PLAYLIST)).await else {
            return Ok(false);
        };
        let mut accepted = false;
        for (n, duration) in local_entries(&text) {
            if n < self.next_n {
                continue;
            }
            if n > self.next_n {
                self.pending_discontinuity |= !self.window.is_empty();
            }
            self.next_n = n + 1;
            if !(0.0..=10.0).contains(&duration) {
                let _ = std::fs::remove_file(dir.join(format!("s{n}.ts")));
                self.pending_discontinuity = true;
                self.failure()?;
                continue;
            }
            if !matches!(self.segment(dir, n).await?, Put::Ok) {
                let _ = std::fs::remove_file(dir.join(format!("s{n}.ts")));
                self.pending_discontinuity |= !self.window.is_empty();
                self.failure()?;
                continue;
            }
            let discontinuity = std::mem::take(&mut self.pending_discontinuity);
            self.window.push_back(Entry {
                n,
                duration,
                discontinuity,
            });
            while self.window.len() > WINDOW {
                self.pop(dir);
            }
            if self.publish(dir).await? {
                self.failures = 0;
                accepted = true;
            }
        }
        Ok(accepted)
    }
    /// Graceful end: the server marks the stream ended and drops its buffer.
    pub async fn finish(&self) {
        if now_ms() >= self.target.expires_at {
            return;
        }
        let _ = self
            .http
            .delete(self.target.base.clone())
            .bearer_auth(&self.target.token)
            .timeout(Duration::from_secs(2))
            .send()
            .await;
    }
}
/// `(N, duration)` for each completed segment listed by FFmpeg's local playlist.
fn local_entries(text: &str) -> Vec<(u64, f64)> {
    let mut entries = Vec::new();
    let mut duration = None;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            duration = value.split(',').next().and_then(|v| v.parse::<f64>().ok());
        } else if let Some(n) = line
            .strip_prefix('s')
            .and_then(|v| v.strip_suffix(".ts"))
            .and_then(|v| v.parse::<u64>().ok())
            && let Some(duration) = duration.take()
        {
            entries.push((n, duration));
        }
    }
    entries
}
/// Owner-private scratch directory; removed on drop, including task abort.
fn scratch() -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("nanocodex-hls-")
        .tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}
/// One playback stream: FFmpeg -> local segments -> authenticated uploads.
/// Status transitions are published on `events`; nothing here is logged.
pub(crate) async fn run(
    inputs: super::screen_broadcast::Inputs,
    target: Target,
    preset: String,
    request_id: Value,
    events: tokio::sync::watch::Sender<Value>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    let stream = target.stream_id.clone();
    let emit = |status: &str, error: Option<&str>| {
        events.send_replace(result(&request_id, &stream, status, error));
    };
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(target.expires_at.saturating_sub(now_ms()));
    let Ok(dir) = scratch() else {
        return emit("failed", Some("capture_failed"));
    };
    let Ok(mut uploader) = Uploader::new(target) else {
        return emit("failed", Some("broadcast_failed"));
    };
    let mut live = false;
    let mut attempts = 0u32;
    let outcome: Option<&'static str> = 'outer: loop {
        if attempts > 0 {
            emit("reconnecting", None);
            live = false;
            tokio::select! {
                _ = stopped.changed() => break None,
                _ = tokio::time::sleep_until(deadline) => break Some("expired"),
                _ = tokio::time::sleep(Duration::from_secs(1 << attempts.min(3))) => {}
            }
        }
        let sink = super::screen_broadcast::Sink::Hls {
            dir: dir.path(),
            start: uploader.next_n,
        };
        let encoder = tokio::select! {
            _ = stopped.changed() => break None,
            _ = tokio::time::sleep_until(deadline) => break Some("expired"),
            encoder = super::screen_broadcast::spawn_encoder(&inputs, &sink, &preset) => encoder,
        };
        let mut encoder = match encoder {
            Ok(encoder) => encoder,
            Err(_) if attempts >= 3 => break Some("capture_failed"),
            Err(_) => {
                attempts += 1;
                continue;
            }
        };
        uploader.restart();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        'cycle: loop {
            // Uploads are never cancelled by encoder progress; only stop,
            // deadline or encoder failure abandon an in-flight request.
            let poll = async {
                tick.tick().await;
                uploader.poll(dir.path()).await
            };
            tokio::pin!(poll);
            let polled = loop {
                tokio::select! {
                    _ = stopped.changed() => { encoder.kill().await; break 'outer None; },
                    _ = tokio::time::sleep_until(deadline) => { encoder.kill().await; break 'outer Some("expired"); },
                    progress = encoder.progress() => if progress.is_err() {
                        encoder.kill().await;
                        if attempts >= 3 { break 'outer Some("encoder_failed"); }
                        attempts += 1;
                        break 'cycle;
                    },
                    polled = &mut poll => break polled,
                }
            };
            match polled {
                Err(error) => {
                    encoder.kill().await;
                    break 'outer Some(error);
                }
                Ok(true) => {
                    attempts = 0;
                    if !live {
                        live = true;
                        emit("live", None);
                    }
                }
                Ok(false) => {}
            }
        }
    };
    match outcome {
        None => {
            tokio::time::timeout(Duration::from_secs(2), uploader.finish())
                .await
                .ok();
            emit("stopped", None);
        }
        Some(error) => {
            if !matches!(error, "upload_rejected" | "expired") {
                tokio::time::timeout(Duration::from_secs(2), uploader.finish())
                    .await
                    .ok();
            }
            emit("failed", Some(error));
        }
    }
    drop(dir);
}

#[cfg(test)]
mod tests {
    //! Real FFmpeg capture -> HLS uploader -> loopback HTTP receiver implementing
    //! the server upload contract -> ffprobe/ffmpeg decode of what was received.
    use super::super::screen_broadcast::{Broadcast, Source};
    use super::super::screen_video::{Capture, VideoSource};
    use super::*;
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        process::{Command, Stdio},
        sync::{Arc, Mutex},
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    const TOKEN: &str = "nsu_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const STREAM: &str = "sp_0123456789abcdef0123456789abcdef";
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[derive(Default)]
    struct Receiver {
        segments: BTreeMap<u64, Vec<u8>>,
        last_n: Option<u64>,
        manifest: String,
        accepted: usize,
        refills: usize,
        missing_replies: usize,
        deleted: bool,
        unauthorized: usize,
        lose_ram: bool,
        manifest_status: Option<u16>,
        always_missing: bool,
        all_status: Option<u16>,
        targets: Vec<String>,
    }
    type Shared = Arc<Mutex<Receiver>>;

    fn handle(
        state: &mut Receiver,
        method: &str,
        path: &str,
        auth: &str,
        kind: &str,
        body: &[u8],
    ) -> (u16, String) {
        state.targets.push(path.to_owned());
        let Some(file) = path.strip_prefix(&format!("/v1/screen-playback/{STREAM}/upload/")) else {
            return (404, String::new());
        };
        if auth != format!("Bearer {TOKEN}") {
            state.unauthorized += 1;
            return (401, String::new());
        }
        if let Some(status) = state.all_status {
            return (status, String::new());
        }
        if method == "DELETE" && file.is_empty() {
            state.deleted = true;
            return (204, String::new());
        }
        if method != "PUT" {
            return (400, String::new());
        }
        if file == "index.m3u8" {
            if state.lose_ram {
                state.lose_ram = false;
                state.segments.clear();
            }
            if let Some(status) = state.manifest_status {
                return (status, String::new());
            }
            let text = String::from_utf8_lossy(body).into_owned();
            let refs: Vec<u64> = local_entries(&text).into_iter().map(|(n, _)| n).collect();
            let missing: Vec<u64> = refs
                .iter()
                .copied()
                .filter(|n| state.always_missing || !state.segments.contains_key(n))
                .collect();
            if refs.is_empty() || !missing.is_empty() {
                state.missing_replies += 1;
                return (
                    409,
                    json!({"error":"missing_segments","missing":missing}).to_string(),
                );
            }
            state.manifest = text;
            state.accepted += 1;
            return (204, String::new());
        }
        let Some(n) = file
            .strip_prefix('s')
            .and_then(|f| f.strip_suffix(".ts"))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            return (400, String::new());
        };
        if kind != "video/mp2t"
            || body.is_empty()
            || body.len() % 188 != 0
            || body.chunks(188).any(|p| p[0] != 0x47)
        {
            return (400, String::new());
        }
        if body.len() as u64 > MAX_SEGMENT_BYTES {
            return (413, String::new());
        }
        match state.last_n {
            Some(last) if n <= last => {
                if state.segments.contains_key(&n) {
                    return (204, String::new());
                }
                if last - n >= WINDOW as u64 {
                    return (409, json!({"error":"sequence"}).to_string());
                }
                state.refills += 1;
            }
            _ => state.last_n = Some(n),
        }
        state.segments.insert(n, body.to_vec());
        while state.segments.len() > WINDOW {
            state.segments.pop_first();
        }
        (204, String::new())
    }

    /// Minimal HTTP/1.1 keep-alive server; returns its loopback origin.
    async fn serve(state: Shared) -> Url {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = state.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut read = BufReader::new(read);
                    loop {
                        let mut line = String::new();
                        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let mut parts = line.split_whitespace();
                        let (method, path) = (
                            parts.next().unwrap_or("").to_owned(),
                            parts.next().unwrap_or("").to_owned(),
                        );
                        let (mut length, mut auth, mut kind) =
                            (0usize, String::new(), String::new());
                        loop {
                            let mut header = String::new();
                            read.read_line(&mut header).await.unwrap();
                            let header = header.trim_end();
                            if header.is_empty() {
                                break;
                            }
                            let (name, value) = header.split_once(':').unwrap();
                            match name.to_ascii_lowercase().as_str() {
                                "content-length" => length = value.trim().parse().unwrap(),
                                "authorization" => auth = value.trim().to_owned(),
                                "content-type" => kind = value.trim().to_owned(),
                                _ => {}
                            }
                        }
                        let mut body = vec![0; length];
                        read.read_exact(&mut body).await.unwrap();
                        let (status, reply) = handle(
                            &mut state.lock().unwrap(),
                            &method,
                            &path,
                            &auth,
                            &kind,
                            &body,
                        );
                        let response = format!(
                            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                            reply.len()
                        );
                        if write.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        origin
    }

    fn synthetic() -> Broadcast {
        let video: Source = Arc::new(|| {
            Box::pin(async {
                let mut command = Command::new("ffmpeg");
                command.args(["-re", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30"]);
                Ok(command)
            })
        });
        let audio: VideoSource = Arc::new(|| {
            Box::pin(async {
                let child = tokio::process::Command::new("ffmpeg")
                    .args([
                        "-v",
                        "quiet",
                        "-re",
                        "-f",
                        "lavfi",
                        "-i",
                        "sine=frequency=440:sample_rate=48000",
                        "-ac",
                        "2",
                        "-f",
                        "s16le",
                        "pipe:1",
                    ])
                    .stdout(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()?;
                Capture::child(child)
            })
        });
        Broadcast::new(Some(video), Some(audio))
    }
    fn start(origin: &Url, expires_in_ms: u64) -> Value {
        json!({"type":"broadcast","target":"hls","action":"start","request_id":"r1","surface_id":"desktop",
            "stream_id":STREAM,"preset":"720p","upload":{"url":origin.join(&format!("/v1/screen-playback/{STREAM}/upload/")).unwrap().as_str(),
            "token":TOKEN,"expires_at":now_ms() + expires_in_ms}})
    }
    fn scratch_dirs() -> Vec<PathBuf> {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("nanocodex-hls-")
            })
            .collect()
    }
    /// Wait for a terminal or matching status; every observed result is checked for secrets.
    async fn wait(
        events: &mut tokio::sync::watch::Receiver<Value>,
        seen: &mut Vec<Value>,
        status: &str,
        secs: u64,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(secs), async {
            loop {
                let value = events.borrow_and_update().clone();
                if !value.is_null() && seen.last() != Some(&value) {
                    seen.push(value.clone());
                }
                if value["status"] == status || value["status"] == "failed" {
                    return value;
                }
                events.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {status}: {seen:?}"))
    }
    fn assert_tokenless(values: &[Value]) {
        for value in values {
            let text = value.to_string();
            assert!(
                !text.contains(TOKEN)
                    && !text.contains("/v1/screen-playback/")
                    && !text.contains("http"),
                "{text}"
            );
            let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
            assert!(
                keys.iter().all(|k| [
                    "type",
                    "target",
                    "request_id",
                    "stream_id",
                    "status",
                    "error"
                ]
                .contains(&k.as_str())),
                "{keys:?}"
            );
        }
    }
    fn evidence() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../output/screen-connect-20261008/hls-evidence");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ffmpeg_hls_upload_decodes_recovers_and_stops_by_stream_id() {
        let _serial = SERIAL.lock().await;
        let state: Shared = Default::default();
        let origin = serve(state.clone()).await;
        let before = scratch_dirs();
        let mut broadcast = synthetic();
        let mut events = broadcast.events();
        let mut seen = Vec::new();
        let reply = broadcast
            .hls_request(&start(&origin, 120_000), &origin.origin())
            .await;
        assert_eq!(reply["status"], "starting");
        assert_eq!(
            wait(&mut events, &mut seen, "live", 20).await["status"],
            "live"
        );
        let created: Vec<_> = scratch_dirs()
            .into_iter()
            .filter(|d| !before.contains(d))
            .collect();
        assert_eq!(created.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&created[0]).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        tokio::time::sleep(Duration::from_secs(8)).await;
        // Durable-object RAM loss: the uploader refills its retained window once.
        let accepted = state.lock().unwrap().accepted;
        state.lock().unwrap().lose_ram = true;
        tokio::time::timeout(Duration::from_secs(10), async {
            while state.lock().unwrap().accepted <= accepted + 1 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("manifest accepted after RAM loss");

        // The RTMP viewer path cannot see or control playback, nor inject a target.
        let rtmp = broadcast
            .request(&json!({"action":"status","viewer_id":"v","request_id":"x"}))
            .await;
        assert_eq!(rtmp["error"], "busy");
        let rtmp = broadcast
            .request(&json!({"action":"stop","target":"hls","stream_id":STREAM,"viewer_id":"v"}))
            .await;
        assert_eq!(rtmp["error"], "invalid_request");
        // Another stream ID neither stops nor describes this stream.
        let other = "sp_ffffffffffffffffffffffffffffffff";
        let stop = broadcast
            .hls_request(
                &json!({"action":"stop","request_id":"s0","stream_id":other}),
                &origin.origin(),
            )
            .await;
        assert_eq!(stop["status"], "stopped");
        assert_eq!(stop["stream_id"], other);
        let status = broadcast
            .hls_request(
                &json!({"action":"status","request_id":"s1","stream_id":STREAM}),
                &origin.origin(),
            )
            .await;
        assert_eq!(status["status"], "live");
        assert_eq!(status["request_id"], "s1");
        let busy = broadcast.hls_request(&json!({"action":"start","request_id":"s2","stream_id":other,"preset":"720p","upload":start(&origin, 60_000)["upload"]}), &origin.origin()).await;
        assert_eq!(busy["error"], "busy");

        // Persist what the receiver holds, then decode it with the real tools.
        let dir = evidence();
        for old in std::fs::read_dir(&dir).unwrap().flatten() {
            if old.file_name().to_string_lossy().ends_with(".ts") {
                std::fs::remove_file(old.path()).unwrap();
            }
        }
        let (manifest, segments, refills, missing, targets) = {
            let s = state.lock().unwrap();
            (
                s.manifest.clone(),
                s.segments.clone(),
                s.refills,
                s.missing_replies,
                s.targets.clone(),
            )
        };
        assert!(
            refills >= 1 && missing >= 1,
            "refills={refills} missing={missing}"
        );
        assert!(
            targets
                .iter()
                .all(|t| !t.contains(TOKEN) && !t.contains('?'))
        );
        let refs = local_entries(&manifest);
        assert!((3..=WINDOW).contains(&refs.len()), "{manifest}");
        assert!(
            manifest.contains("#EXT-X-TARGETDURATION:2")
                || manifest.contains("#EXT-X-TARGETDURATION:3"),
            "{manifest}"
        );
        for (n, duration) in &refs {
            assert!(*duration <= 3.0);
            std::fs::write(dir.join(format!("s{n}.ts")), &segments[n]).unwrap();
        }
        // Offline copy of the live window: ENDLIST makes the probe finite.
        std::fs::write(
            dir.join("index.m3u8"),
            format!("{manifest}#EXT-X-ENDLIST\n"),
        )
        .unwrap();
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,codec_type,width,height,nb_read_frames",
                "-of",
                "json",
            ])
            .arg(dir.join("index.m3u8"))
            .output()
            .unwrap();
        assert!(
            probe.status.success(),
            "{}",
            String::from_utf8_lossy(&probe.stderr)
        );
        std::fs::write(dir.join("ffprobe.json"), &probe.stdout).unwrap();
        let probe: Value = serde_json::from_slice(&probe.stdout).unwrap();
        let streams = probe["streams"].as_array().unwrap();
        let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
        assert_eq!(video["codec_name"], "h264");
        assert_eq!(
            (video["width"].as_u64(), video["height"].as_u64()),
            (Some(640), Some(360))
        );
        assert!(
            video["nb_read_frames"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                >= 60
        );
        assert!(
            streams
                .iter()
                .any(|s| s["codec_type"] == "audio" && s["codec_name"] == "aac")
        );
        let decode = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(dir.join("index.m3u8"))
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            decode.status.success() && decode.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&decode.stderr)
        );

        let stopped = broadcast
            .hls_request(
                &json!({"action":"stop","request_id":"s3","stream_id":STREAM}),
                &origin.origin(),
            )
            .await;
        assert_eq!(stopped["status"], "stopped");
        assert!(state.lock().unwrap().deleted);
        assert!(!created[0].exists());
        seen.push(events.borrow().clone());
        seen.extend(
            [reply, other.into(), status, busy, stopped]
                .into_iter()
                .filter(Value::is_object),
        );
        assert_tokenless(&seen);
        assert_eq!(state.lock().unwrap().unauthorized, 0);
        std::fs::write(
            dir.join("events.json"),
            serde_json::to_vec_pretty(&seen).unwrap(),
        )
        .unwrap();
    }

    /// Terminal outcomes: the encoder is killed and its scratch directory removed.
    async fn terminal(
        configure: impl FnOnce(&mut Receiver),
        token: &str,
        expires_in_ms: u64,
        revoke_after_live: bool,
    ) -> (Vec<Value>, Shared) {
        let _serial = SERIAL.lock().await;
        let state: Shared = Default::default();
        configure(&mut state.lock().unwrap());
        let origin = serve(state.clone()).await;
        let before = scratch_dirs();
        let mut broadcast = synthetic();
        let mut events = broadcast.events();
        let mut request = start(&origin, expires_in_ms);
        request["upload"]["token"] = token.into();
        let mut seen = vec![broadcast.hls_request(&request, &origin.origin()).await];
        if revoke_after_live {
            wait(&mut events, &mut seen, "live", 20).await;
            state.lock().unwrap().all_status = Some(410);
        }
        let last = wait(&mut events, &mut seen, "never", 40).await;
        assert_eq!(last["status"], "failed");
        // The slot is released: the task ended and no scratch directory remains.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(scratch_dirs().iter().all(|d| before.contains(d)));
        assert_eq!(
            broadcast
                .hls_request(
                    &json!({"action":"status","request_id":"q","stream_id":STREAM}),
                    &origin.origin()
                )
                .await["status"],
            "failed"
        );
        broadcast.stop().await;
        assert_tokenless(&seen);
        (seen, state)
    }
    fn last_error(seen: &[Value]) -> &str {
        seen.last().unwrap()["error"].as_str().unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_token_is_terminal_upload_rejected() {
        let wrong = "nsu_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        let (seen, state) = terminal(|_| {}, wrong, 60_000, false).await;
        assert_eq!(last_error(&seen), "upload_rejected");
        assert!(!seen.iter().any(|v| v["status"] == "live"));
        let state = state.lock().unwrap();
        assert_eq!(state.unauthorized, 1, "401 is never retried");
        assert!(!state.deleted);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revocation_mid_stream_stops_encoder() {
        let (seen, _) = terminal(|_| {}, TOKEN, 60_000, true).await;
        assert_eq!(last_error(&seen), "upload_rejected");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn persistent_playlist_5xx_fails_without_reporting_live() {
        let (seen, state) = terminal(|s| s.manifest_status = Some(503), TOKEN, 60_000, false).await;
        assert_eq!(last_error(&seen), "broadcast_failed");
        assert!(!seen.iter().any(|v| v["status"] == "live"), "{seen:?}");
        let state = state.lock().unwrap();
        assert_eq!(state.accepted, 0);
        assert!(state.deleted, "best-effort end after a non-auth failure");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unrecoverable_missing_segments_fail_without_reporting_live() {
        let (seen, state) = terminal(|s| s.always_missing = true, TOKEN, 60_000, false).await;
        assert_eq!(last_error(&seen), "broadcast_failed");
        assert!(!seen.iter().any(|v| v["status"] == "live"));
        // Exactly one refill-and-resend per segment, three consecutive failures.
        assert_eq!(state.lock().unwrap().missing_replies, 6);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deadline_expires_stream() {
        let (seen, state) = terminal(|_| {}, TOKEN, 6_000, false).await;
        assert!(seen.iter().any(|v| v["status"] == "live"));
        assert_eq!(last_error(&seen), "expired");
        assert!(
            !state.lock().unwrap().deleted,
            "expired credentials are not used"
        );
    }

    #[tokio::test]
    async fn start_requires_exact_publisher_origin_and_stream_path() {
        let origin = Url::parse("https://publisher.example/").unwrap();
        let mut broadcast = synthetic();
        let valid = |url: &str| {
            json!({"action":"start","request_id":"r","stream_id":STREAM,"preset":"720p",
            "upload":{"url":url,"token":TOKEN,"expires_at":now_ms() + 60_000}})
        };
        let path = format!("/v1/screen-playback/{STREAM}/upload/");
        let mut replies = Vec::new();
        for (url, error) in [
            (format!("https://evil.example{path}"), "invalid_request"),
            (format!("http://publisher.example{path}"), "invalid_request"),
            (format!("https://publisher.example:8443{path}"), "invalid_request"),
            ("https://publisher.example/v1/screen-playback/sp_ffffffffffffffffffffffffffffffff/upload/".into(), "invalid_request"),
            ("https://publisher.example/v1/other/".into(), "invalid_request"),
            (format!("https://publisher.example{path}?token={TOKEN}"), "invalid_request"),
            (format!("https://user:pw@publisher.example{path}"), "invalid_request"),
        ] {
            let reply = broadcast.hls_request(&valid(&url), &origin.origin()).await;
            assert_eq!(reply["error"], error, "{url}");
            replies.push(reply);
        }
        let mut bad = valid(&format!("https://publisher.example{path}"));
        bad["stream_id"] = "550e8400-e29b-41d4-a716-446655440000".into();
        replies.push(broadcast.hls_request(&bad, &origin.origin()).await);
        let mut expired = valid(&format!("https://publisher.example{path}"));
        expired["upload"]["expires_at"] = (now_ms() - 1).into();
        let reply = broadcast.hls_request(&expired, &origin.origin()).await;
        assert_eq!(reply["error"], "expired");
        replies.push(reply);
        assert_eq!(replies[7]["error"], "invalid_request");
        let target = parse_start(
            &valid(&format!("https://publisher.example{path}")),
            &origin.origin(),
            now_ms(),
        )
        .unwrap();
        assert!(!format!("{target:?}").contains(TOKEN));
        assert_tokenless(&replies);
    }
}
