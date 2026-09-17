//! Opt-in real desktop capture -> VideoToolbox -> WebRTC -> Chromium verification.
//! Run from an application granted macOS Screen Recording permission:
//! `cargo test -p nanocodex2-bin --bin nanocodex2 macos_live_webrtc -- --ignored --nocapture`
//! Requires FFmpeg and Chrome; NANOCODEX_TEST_CHROME may select another Chromium binary.
use super::super::screen_video::Video;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::Html,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Metrics {
    frames_decoded: u64,
    width: u32,
    height: u32,
    elapsed_ms: f64,
    #[serde(default)]
    frames_dropped: u64,
    #[serde(default)]
    total_decode_time: f64,
    #[serde(default)]
    codec: String,
    #[serde(default)]
    error: Option<String>,
}

enum Incoming {
    Signal(Value),
    Result(Metrics),
}
#[derive(Clone)]
struct Bridge {
    outgoing: Arc<Mutex<mpsc::Receiver<Value>>>,
    incoming: mpsc::Sender<Incoming>,
}
async fn events(State(state): State<Bridge>) -> Json<Vec<Value>> {
    let mut queue = state.outgoing.lock().await;
    let mut signals = Vec::new();
    while let Ok(signal) = queue.try_recv() {
        signals.push(signal);
    }
    Json(signals)
}
async fn signal(State(state): State<Bridge>, Json(value): Json<Value>) -> StatusCode {
    match state.incoming.try_send(Incoming::Signal(value)) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
async fn result(State(state): State<Bridge>, Json(metrics): Json<Metrics>) -> StatusCode {
    match state.incoming.try_send(Incoming::Result(metrics)) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[tokio::test]
#[ignore = "real macOS screen capture; requires Screen Recording permission, FFmpeg, and Chrome"]
async fn macos_live_webrtc() -> Result<()> {
    let started = Instant::now();
    let (width, height) = nanocodex_hand::main_display_pixel_dimensions()?;
    let expected = nanocodex_hand::VideoSettings::from_environment(width, height, 3840, 24000)?;
    let source = super::native_video();
    let mut video = Video::start(&source, None).await?;
    let (outgoing, receiver) = mpsc::channel(128);
    let (incoming, mut messages) = mpsc::channel(128);
    let token = uuid::Uuid::new_v4().to_string();
    let app = Router::new()
        .route(&format!("/{token}/"), get(|| async { Html(PAGE) }))
        .route(&format!("/{token}/events"), get(events))
        .route(&format!("/{token}/signal"), post(signal))
        .route(&format!("/{token}/result"), post(result))
        .with_state(Bridge {
            outgoing: Arc::new(Mutex::new(receiver)),
            incoming,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/{token}/", listener.local_addr()?);
    let server = super::super::screen_video::Task(tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("local verification server: {error}");
        }
    }));
    outgoing
        .send(video.add("local-chromium", Vec::new()).await?["signal"].clone())
        .await?;
    eprintln!("macOS live WebRTC verification: {url}");
    let profile = tempfile::Builder::new()
        .prefix("nanocodex-live-chrome-")
        .tempdir()?;
    let chrome = std::env::var_os("NANOCODEX_TEST_CHROME")
        .unwrap_or_else(|| "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
    let mut browser = tokio::process::Command::new(chrome)
        .args([
            "--headless",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            // The isolated loopback fixture has no mDNS resolver or TURN server.
            "--disable-features=WebRtcHideLocalIpsWithMdns",
            "--autoplay-policy=no-user-gesture-required",
        ])
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            tokio::select! {
                message = messages.recv() => match message.ok_or("browser signaling closed")? {
                    Incoming::Signal(signal) => video.signal("local-chromium", &signal).await?,
                    Incoming::Result(metrics) => {
                        eprintln!("macOS live WebRTC metrics: {} (wall {:.2}s)",
                            serde_json::to_string(&metrics)?, started.elapsed().as_secs_f64());
                        if let Some(error) = &metrics.error {
                            return Err(format!("Chromium verification failed: {error}").into());
                        }
                        if metrics.frames_decoded < 60 || metrics.width == 0 || metrics.height == 0 {
                            return Err("Chromium did not decode 60 nonempty frames".into());
                        }
                        if (metrics.width, metrics.height) != (expected.width, expected.height) {
                            return Err("Chromium decoded a different resolution than the configured native stream".into());
                        }
                        if !metrics.codec.eq_ignore_ascii_case("video/H264") {
                            return Err("Chromium did not report H.264 decoding".into());
                        }
                        return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
                    }
                },
                event = video.next() => {
                    let event = event.ok_or("video signaling closed")?;
                    if event.outgoing {
                        outgoing.try_send(event.value["signal"].clone())?;
                    }
                },
                status = browser.wait() => return Err(format!("Chrome exited before verification: {:?}", status?).into()),
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    if video.failed() { return Err("native video capture failed".into()); }
                }
            }
        }
    }).await;
    let _ = browser.kill().await;
    let _ = browser.wait().await;
    drop(server);
    drop(video);
    drop(profile);
    outcome.map_err(|_| "timed out after 120s waiting for Chromium to decode 60 frames; check Screen Recording permission" )?
}

// Only signaling and aggregate decoder metrics cross HTTP. No screenshots or media are logged.
// Data channels are never used for input; the browser consumes only the video track.
const PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>Local macOS WebRTC verification</title>
<video autoplay muted playsinline></video><pre id="status">Connecting…</pre>
<script>
(async () => {
  const video = document.querySelector('video');
  const status = document.querySelector('#status');
  const pc = new RTCPeerConnection({iceServers: []});
  const started = performance.now();
  let finished = false;
  let latest = {framesDecoded: 0, width: 0, height: 0, elapsedMs: 0};
  async function post(path, value) {
    const reply = await fetch(path, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(value)});
    if (!reply.ok) throw new Error(`${path}: HTTP ${reply.status}`);
  }
  async function finish(error) {
    if (finished) return;
    finished = true;
    latest.elapsedMs = performance.now() - started;
    if (error) latest.error = String(error);
    status.textContent = JSON.stringify(latest, null, 2);
    await post('result', latest);
  }
  pc.ontrack = event => {
    video.srcObject = new MediaStream([event.track]);
    video.play().catch(finish);
  };
  pc.onicecandidate = event => {
    if (event.candidate) post('signal', {type: 'candidate', ...event.candidate.toJSON()}).catch(finish);
  };
  pc.onconnectionstatechange = () => {
    if (pc.connectionState === 'failed') finish('WebRTC connection failed');
  };
  try {
    while (!finished) {
      const response = await fetch('events', {cache: 'no-store'});
      if (!response.ok) throw new Error(`events: HTTP ${response.status}`);
      for (const signal of await response.json()) {
        if (signal.type === 'offer') {
          await pc.setRemoteDescription(signal);
          await pc.setLocalDescription(await pc.createAnswer());
          await post('signal', {type: 'answer', sdp: pc.localDescription.sdp});
        } else if (signal.type === 'candidate') {
          await pc.addIceCandidate(signal);
        }
      }
      const stats = await pc.getStats();
      for (const stat of stats.values()) {
        if (stat.type === 'inbound-rtp' && stat.kind === 'video') {
          latest = {framesDecoded: stat.framesDecoded || 0,
            width: video.videoWidth, height: video.videoHeight,
            elapsedMs: performance.now() - started,
            framesDropped: stat.framesDropped || 0, totalDecodeTime: stat.totalDecodeTime || 0,
            codec: stats.get(stat.codecId)?.mimeType || ''};
          status.textContent = JSON.stringify(latest, null, 2);
          if (latest.framesDecoded >= 60 && latest.width > 0 && latest.height > 0) await finish();
        }
      }
      if (performance.now() - started > 115000) await finish('decoder deadline exceeded');
      await new Promise(resolve => setTimeout(resolve, 100));
    }
  } catch (error) { await finish(error); }
})();
</script>"#;
