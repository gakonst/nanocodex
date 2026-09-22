//! Read-only Hand video receiver. Media and terminal work never block the chat loop.
use futures_util::{SinkExt, StreamExt};
use image::DynamicImage;
use nanocodex_managed::ManagedClient;
use nanocodex_tools::attachment::AttachmentTarget;
use ratatui::layout::Size;
use ratatui_image::picker::Picker;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Notify, mpsc, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use webrtc::{
    api::{
        APIBuilder,
        interceptor_registry::{configure_rtcp_reports, configure_twcc_receiver_only},
        media_engine::MediaEngine,
    },
    ice_transport::ice_candidate::RTCIceCandidateInit,
    interceptor::registry::Registry,
    media::io::sample_builder::SampleBuilder,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        sdp::session_description::RTCSessionDescription,
    },
    rtp::codecs::h264::H264Packet,
};

#[path = "screen_audio_playback.rs"]
mod audio;
#[path = "screen_graphics.rs"]
mod graphics;
pub(crate) use graphics::VideoFrame;

const VIDEO_REORDER_WINDOW: usize = 4096;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
// A small bounded queue absorbs decoder bursts without dropping every second
// frame against the 60 Hz presentation clock. Drop oldest frames when behind.
#[derive(Default)]
struct VideoFrames {
    queue: Mutex<VecDeque<Arc<DynamicImage>>>,
    available: Notify,
    seen: AtomicBool,
}
impl VideoFrames {
    fn push(&self, image: DynamicImage) {
        let mut queue = self.queue.lock().unwrap();
        if queue.len() == 3 {
            queue.pop_front();
        }
        queue.push_back(Arc::new(image));
        self.seen.store(true, Ordering::Relaxed);
        self.available.notify_one();
    }
    fn has_frame(&self) -> bool {
        self.seen.load(Ordering::Relaxed)
    }
    async fn next(&self) -> Arc<DynamicImage> {
        loop {
            let available = self.available.notified();
            if let Some(image) = self.queue.lock().unwrap().pop_front() {
                return image;
            }
            available.await;
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct Surface {
    pub id: String,
    pub machine_id: String,
    pub machine_name: String,
    pub name: String,
    pub generation: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub transport: Option<String>,
}
#[derive(Clone, Default)]
pub(crate) struct Snapshot {
    pub surfaces: Vec<Surface>,
    pub status: String,
    pub frame: Option<Arc<VideoFrame>>,
    pub audio: String,
    pub audio_packets: u64,
    pub source_size: (u32, u32),
}
#[derive(Clone, Debug)]
pub(crate) enum Command {
    List,
    Watch(Surface),
    Close,
    ToggleAudio,
}
struct Task(JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Peer(Arc<RTCPeerConnection>);
impl Drop for Peer {
    fn drop(&mut self) {
        let peer = self.0.clone();
        tokio::spawn(async move {
            let _ = peer.close().await;
        });
    }
}

pub(crate) struct Controller {
    pub updates: watch::Receiver<Snapshot>,
    sender: watch::Sender<Snapshot>,
    pub size: watch::Sender<Size>,
    task: Option<Task>,
    picker: Picker,
    muted: watch::Sender<bool>,
}
impl Controller {
    pub fn new(picker: Picker) -> Self {
        let (sender, updates) = watch::channel(Snapshot::default());
        let (size, _) = watch::channel(Size::new(80, 30));
        Self {
            updates,
            sender,
            size,
            task: None,
            picker,
            muted: watch::channel(false).0,
        }
    }
    fn reset(&mut self) {
        self.task = None;
        self.muted.send_replace(false);
        // A cancelled worker may finish a synchronous encode on another CPU.
        // Retire its channel so it cannot display the previous Hand in a new pane.
        let (sender, mut updates) = watch::channel(Snapshot {
            status: "Connecting…".into(),
            ..Default::default()
        });
        updates.mark_changed();
        self.sender = sender;
        self.updates = updates;
    }
    pub fn command(&mut self, client: &ManagedClient, command: Command) {
        if matches!(command, Command::ToggleAudio) {
            self.muted.send_modify(|muted| *muted = !*muted);
            return;
        }
        self.reset();
        if matches!(command, Command::Close) {
            return;
        }
        let target = match client.account_attachment_target() {
            Ok(target) => target,
            Err(error) => {
                self.sender.send_modify(|s| s.status = error.to_string());
                return;
            }
        };
        let output = self.sender.clone();
        let picker = self.picker.clone();
        let size = self.size.subscribe();
        let muted = self.muted.subscribe();
        self.task = Some(Task(tokio::spawn(async move {
            let result: Result<()> = async {
                let http = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(10))
                    .build()?;
                match command {
                    Command::List => {
                        let value = request(&http, &target, "screens", None).await?;
                        let surfaces = catalog(value)?;
                        output.send_modify(|s| {
                            s.status = if surfaces.is_empty() {
                                "No Hands are publishing a screen.".into()
                            } else {
                                String::new()
                            };
                            s.surfaces = surfaces;
                        });
                    }
                    Command::Watch(surface) => {
                        let mut failures = 0;
                        loop {
                            let frames = Arc::new(VideoFrames::default());
                            let encoder = Task(tokio::spawn(encode_frames(
                                frames.clone(),
                                size.clone(),
                                picker.clone(),
                                output.clone(),
                            )));
                            let started = Instant::now();
                            let result = session(
                                &http,
                                &target,
                                &surface,
                                &frames,
                                muted.clone(),
                                output.clone(),
                                size.clone(),
                                picker.font_size(),
                            )
                            .await;
                            drop(encoder);
                            let Err(error) = result else {
                                break;
                            };
                            if started.elapsed() > Duration::from_secs(60) && frames.has_frame() {
                                failures = 0;
                            }
                            if failures >= 2 || !retryable(error.as_ref()) {
                                return Err(error);
                            }
                            failures += 1;
                            output.send_modify(|s| {
                                s.frame = None;
                                s.status = format!("Reconnecting… ({failures}/2)");
                            });
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                    Command::Close | Command::ToggleAudio => {}
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                output.send_modify(|s| {
                    s.frame = None;
                    s.status = format!("Screen: {error}. Press r to retry or Esc to close.");
                });
            }
        })));
    }
}
fn retryable(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(error) = error.downcast_ref::<reqwest::Error>() {
        return error.is_connect()
            || error.is_timeout()
            || error
                .status()
                .is_some_and(|status| status.is_server_error());
    }
    if let Some(error) = error.downcast_ref::<tokio_tungstenite::tungstenite::Error>() {
        use tokio_tungstenite::tungstenite::Error;
        return matches!(
            error,
            Error::ConnectionClosed | Error::AlreadyClosed | Error::Io(_)
        );
    }
    matches!(
        error.to_string().as_str(),
        "Screen disconnected"
            | "Screen authorization expired"
            | "No video frames received"
            | "Video stream stalled"
    )
}

fn catalog(value: Value) -> Result<Vec<Surface>> {
    let surfaces: Vec<Surface> = serde_json::from_value(
        value
            .get("surfaces")
            .ok_or("Invalid screen catalog")?
            .clone(),
    )?;
    if surfaces.len() > 512
        || surfaces.iter().any(|s| {
            s.width == 0
                || s.height == 0
                || s.width > 16384
                || s.height > 16384
                || !matches!(s.transport.as_deref(), None | Some("webrtc" | "frames-v1"))
                || [
                    &s.id,
                    &s.machine_id,
                    &s.machine_name,
                    &s.name,
                    &s.generation,
                ]
                .iter()
                .any(|s| s.is_empty() || s.len() > 512)
        })
    {
        return Err("Invalid screen catalog".into());
    }
    Ok(surfaces)
}
fn endpoint(target: &AttachmentTarget, path: &str) -> Result<url::Url> {
    let mut url = target.endpoint().clone();
    url.set_scheme(if url.scheme() == "wss" {
        "https"
    } else {
        "http"
    })
    .map_err(|_| "Invalid screen origin")?;
    url.set_path(&format!("/v1/account/hands/{path}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}
async fn request(
    http: &reqwest::Client,
    target: &AttachmentTarget,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let url = endpoint(target, path)?;
    let req = if let Some(body) = body {
        http.post(url).json(&body)
    } else {
        http.get(url)
    };
    Ok(req
        .bearer_auth(target.bearer())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
fn viewer_interceptors(engine: &mut MediaEngine) -> Result<Registry> {
    use webrtc::{
        interceptor::nack::generator::Generator,
        rtp_transceiver::{RTCPFeedback, rtp_codec::RTPCodecType},
    };
    for parameter in ["", "pli"] {
        engine.register_feedback(
            RTCPFeedback {
                typ: "nack".into(),
                parameter: parameter.into(),
            },
            RTPCodecType::Video,
        );
    }
    let mut registry = Registry::new();
    // The default 8192-packet window can create an RTCP NACK larger than
    // this library can marshal, disabling recovery after a burst of loss.
    // A 2048-packet window fits a single report and retries within our 250 ms
    // frame deadline. This receiver never sends media, so needs no responder.
    registry.add(Box::new(
        Generator::builder()
            .with_log2_size_minus_6(5)
            .with_skip_last_n(2)
            .with_interval(Duration::from_millis(20)),
    ));
    Ok(configure_twcc_receiver_only(
        configure_rtcp_reports(registry),
        engine,
    )?)
}

async fn session(
    http: &reqwest::Client,
    target: &AttachmentTarget,
    surface: &Surface,
    frames: &Arc<VideoFrames>,
    muted: watch::Receiver<bool>,
    output: watch::Sender<Snapshot>,
    size: watch::Receiver<Size>,
    font: ratatui_image::FontSize,
) -> Result<()> {
    // Resolve the current generation only for this explicitly selected identity.
    let current = catalog(request(http, target, "screens", None).await?)?
        .into_iter()
        .find(|s| s.machine_id == surface.machine_id && s.id == surface.id)
        .ok_or("This Hand is no longer publishing that screen")?;
    let fallback = current.transport.as_deref() == Some("frames-v1");
    output.send_modify(|s| s.audio = if fallback { "not published" } else { "waiting" }.into());
    let mut url = endpoint(target, "view")?;
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| "Invalid screen origin")?;
    url.query_pairs_mut()
        .append_pair("machine_id", &current.machine_id)
        .append_pair("surface_id", &current.id)
        .append_pair("generation", &current.generation);
    let mut req = url.as_str().into_client_request()?;
    let mut auth = format!("Bearer {}", target.bearer())
        .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()?;
    auth.set_sensitive(true);
    req.headers_mut().insert("authorization", auth);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(750_000))
        .max_frame_size(Some(750_000));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(req, Some(config), false),
    )
    .await??;
    let (mut sink, mut stream) = socket.split();
    let (signals, mut outgoing) = mpsc::channel::<Value>(128);
    let mut tasks = Vec::new();
    let peer = if fallback {
        None
    } else {
        let ice = request(http, target, "ice", Some(json!({}))).await?;
        let mut engine = MediaEngine::default();
        engine.register_default_codecs()?;
        let registry = viewer_interceptors(&mut engine)?;
        let mut settings = webrtc::api::setting_engine::SettingEngine::default();
        // A retransmitted 4K packet can arrive hundreds of packets behind the
        // newest one. The default 64-packet SRTP window rejected those repairs
        // before the frame reorder buffer could see them. Keep replay checks,
        // with a window matching the video assembler.
        settings.set_srtp_replay_protection_window(VIDEO_REORDER_WINDOW);
        let peer = Arc::new(
            APIBuilder::new()
                .with_media_engine(engine)
                .with_setting_engine(settings)
                .with_interceptor_registry(registry)
                .build()
                .new_peer_connection(RTCConfiguration {
                    ice_servers: crate::screen_ice::ice_servers(&ice)?,
                    ..Default::default()
                })
                .await?,
        );
        let sender = signals.clone();
        peer.on_ice_candidate(Box::new(move |candidate| { let sender = sender.clone(); Box::pin(async move { if let Some(candidate) = candidate && let Ok(candidate) = candidate.to_json() { let _ = sender.send(json!({"type":"candidate", "candidate":candidate.candidate,"sdpMid":candidate.sdp_mid,"sdpMLineIndex":candidate.sdp_mline_index})).await; } }) }));
        // The session owns both tracks, so closing or switching Hands stops sound too.
        let (tracks, mut incoming) = mpsc::channel(2);
        peer.on_track(Box::new(move |track, _, _| {
            let tracks = tracks.clone();
            Box::pin(async move {
                let _ = tracks.try_send(track);
            })
        }));
        let frames = frames.clone();
        let decoder_peer = peer.clone();
        let sender = signals.clone();
        tasks.push(Task(tokio::spawn(async move {
            let mut decoders = Vec::new();
            let mut video_seen = false;
            let mut audio_seen = false;
            while let Some(track) = incoming.recv().await {
                use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
                match track.kind() {
                    RTPCodecType::Video if !video_seen => {
                        video_seen = true;
                        let frames = frames.clone();
                        let sender = sender.clone();
                        let size = size.clone();
                        let peer = decoder_peer.clone();
                        decoders.push(Task(tokio::spawn(async move {
                            if let Err(error) = decode_track(track, frames, size, font, peer).await
                            {
                                let _ = sender
                                    .send(json!({"decoder_error":error.to_string()}))
                                    .await;
                            }
                        })));
                    }
                    RTPCodecType::Audio if !audio_seen => {
                        audio_seen = true;
                        let muted = muted.clone();
                        let output = output.clone();
                        decoders.push(Task(tokio::spawn(async move {
                            if let Err(error) = audio::play(track, muted, output.clone()).await {
                                output.send_modify(|s| s.audio = format!("unavailable: {error}"));
                            }
                        })));
                    }
                    _ => {}
                }
            }
        })));
        Some(Peer(peer))
    };
    let mut candidates = Vec::new();
    let mut connection = None;
    let mut renewal = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(10),
        Duration::from_secs(10),
    );
    let connected_at = Instant::now();
    let mut authorized = connected_at;
    loop {
        tokio::select! {
            _ = renewal.tick() => {
                if !frames.has_frame() && connected_at.elapsed() > Duration::from_secs(25) { return Err("No video frames received".into()); }
                if authorized.elapsed() > Duration::from_secs(30) { return Err("Screen authorization expired".into()); }
                if let Some(id) = &connection {
                    request(http, target, "renew", Some(json!({"connection_id":id}))).await?;
                    authorized = Instant::now();
                    sink.send(Message::Text(json!({"type":"ping"}).to_string().into())).await?;
                }
            }
            signal = outgoing.recv() => {
                let signal = signal.ok_or("Screen decoder stopped")?;
                if let Some(error) = signal.get("decoder_error").and_then(Value::as_str) { return Err(error.to_owned().into()); }
                if signal["type"] == "candidate" { tracing::debug!(target: "nanocodex2::screen", candidate = signal["candidate"].as_str().unwrap_or_default(), "local ICE candidate"); }
                sink.send(Message::Text(json!({"type":"signal", "signal":signal}).to_string().into())).await?;
            }
            message = stream.next() => {
                let message = message.ok_or("Screen disconnected")??;
                let Message::Text(text) = message else { if matches!(message, Message::Close(_)) { return Err("Screen disconnected".into()); } continue; };
                let value: Value = serde_json::from_str(&text)?;
                match value["type"].as_str() {
                    Some("ready") => { connection = Some(value["connection_id"].as_str().filter(|v| !v.is_empty() && v.len() <= 128).ok_or("Invalid screen lease")?.to_owned()); authorized = Instant::now(); if fallback { sink.send(Message::Text(json!({"type":"frame_request"}).to_string().into())).await?; } }
                    Some("renewed") => authorized = Instant::now(),
                    Some("pong") | Some("control") => {},
                    Some("frame") if fallback => {
                        use base64::Engine;
                        let bytes = base64::engine::general_purpose::STANDARD.decode(value["jpeg"].as_str().ok_or("Invalid screen frame")?)?;
                        let image = decode_jpeg(&bytes)?;
                        frames.push(image);
                        sink.send(Message::Text(json!({"type":"frame_request"}).to_string().into())).await?;
                    }
                    Some("signal") => {
                        let peer = &peer.as_ref().ok_or("Unexpected video signal")?.0;
                        let signal = &value["signal"];
                        match signal["type"].as_str() {
                            Some("offer") => {
                                let sdp = signal["sdp"].as_str().filter(|s| s.len() <= 65536).ok_or("Invalid video offer")?;
                                peer.set_remote_description(RTCSessionDescription::offer(sdp.to_owned())?).await?;
                                for candidate in candidates.drain(..) { peer.add_ice_candidate(candidate).await?; }
                                let answer = peer.create_answer(None).await?;
                                peer.set_local_description(answer.clone()).await?;
                                sink.send(Message::Text(json!({"type":"signal","signal":{"type":"answer","sdp":answer.sdp}}).to_string().into())).await?;
                            }
                            Some("candidate") => {
                                let candidate: RTCIceCandidateInit = serde_json::from_value(signal.clone())?;
                                tracing::debug!(target: "nanocodex2::screen", candidate = %candidate.candidate, "remote ICE candidate");
                                if peer.remote_description().await.is_some() { peer.add_ice_candidate(candidate).await?; } else if candidates.len() < 128 { candidates.push(candidate); } else { return Err("Too many ICE candidates".into()); }
                            }
                            _ => return Err("Invalid video signal".into()),
                        }
                    }
                    _ => return Err("Unexpected screen message".into()),
                }
            }
        }
    }
}
fn decode_jpeg(bytes: &[u8]) -> Result<DynamicImage> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1280);
    limits.max_image_height = Some(1280);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}
// A 4K keyframe spans hundreds of RTP packets. A 64-packet window discards
// those frames before their final fragment arrives, even on a lossless LAN.
struct VideoSamples {
    builder: SampleBuilder<H264Packet>,
    previous: Option<(u16, u32)>,
    ready: bool,
}
fn video_samples() -> VideoSamples {
    VideoSamples {
        builder: SampleBuilder::new(VIDEO_REORDER_WINDOW as u16, H264Packet::default(), 90_000)
            .with_max_time_delay(Duration::from_millis(250)),
        previous: None,
        ready: false,
    }
}
impl VideoSamples {
    fn push(&mut self, packet: webrtc::rtp::packet::Packet) {
        let current = (packet.header.sequence_number, packet.header.timestamp);
        // SampleBuilder scans the entire incomplete frame on every pop. At
        // 4K this became quadratic in hundreds of fragments. Drain at frame
        // boundaries and after reordering; retain the same loss/reorder policy.
        self.ready |= packet.header.marker
            || self.previous.is_none_or(|(sequence, timestamp)| {
                current.0 != sequence.wrapping_add(1) || current.1 != timestamp
            });
        self.previous = Some(current);
        self.builder.push(packet);
    }
    fn pop(&mut self) -> Option<webrtc::media::Sample> {
        if !self.ready {
            return None;
        }
        let sample = self.builder.pop();
        if sample.is_none() {
            self.ready = false;
        }
        sample
    }
}

async fn decode_track(
    track: Arc<webrtc::track::track_remote::TrackRemote>,
    frames: Arc<VideoFrames>,
    mut size: watch::Receiver<Size>,
    font: ratatui_image::FontSize,
    peer: Arc<RTCPeerConnection>,
) -> Result<()> {
    if !track
        .codec()
        .capability
        .mime_type
        .eq_ignore_ascii_case("video/h264")
    {
        return Err("This screen requires an H.264 video track".into());
    }
    loop {
        let area = *size.borrow_and_update();
        let pixels = (
            (u32::from(area.width) * u32::from(font.width)).clamp(16, 4096),
            (u32::from(area.height) * u32::from(font.height)).clamp(16, 4096),
        );
        let mut child = spawn_decoder_scaled(&decoder_candidates(), Some(pixels))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or("Video decoder stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Video decoder stdout unavailable")?;
        // A resized decoder needs parameter sets and a fresh reference frame.
        peer.write_rtcp(&[Box::new(
            webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc: track.ssrc(),
            },
        )])
        .await?;
        let stderr = child
            .stderr
            .take()
            .ok_or("Video decoder diagnostics unavailable")?;
        let _diagnostics = Task(tokio::spawn(async move {
            let mut stderr = stderr;
            let mut bytes = [0u8; 2048];
            while let Ok(count) = stderr.read(&mut bytes).await {
                if count == 0 {
                    break;
                }
                tracing::debug!(target: "nanocodex2::screen", message = %String::from_utf8_lossy(&bytes[..count]), "video decoder");
            }
        }));
        let input = track.clone();
        let _feed = Task(tokio::spawn(async move {
            let mut samples = video_samples();
            let mut packets = 0u32;
            let mut decoded = 0u32;
            let mut missing = 0u32;
            let mut previous_sequence: Option<u16> = None;
            let mut report = Instant::now();
            while let Ok((packet, _)) = input.read_rtp().await {
                packets += 1;
                if let Some(previous) = previous_sequence {
                    let gap = packet
                        .header
                        .sequence_number
                        .wrapping_sub(previous.wrapping_add(1));
                    if gap < 32768 {
                        missing += u32::from(gap);
                    }
                }
                previous_sequence = Some(packet.header.sequence_number);
                samples.push(packet);
                while let Some(sample) = samples.pop() {
                    decoded += 1;
                    if stdin.write_all(&sample.data).await.is_err() {
                        return;
                    }
                }
                if report.elapsed() >= Duration::from_secs(2) {
                    tracing::debug!(target: "nanocodex2::screen", packets, sequence_gaps = missing, samples = decoded, seconds = report.elapsed().as_secs_f64(), "video ingress");
                    packets = 0;
                    missing = 0;
                    decoded = 0;
                    report = Instant::now();
                }
            }
        }));
        let mut reader = BufReader::with_capacity(256 * 1024, stdout);
        loop {
            tokio::select! {
                result = size.changed() => {
                    if result.is_err() { return Ok(()); }
                    // read_ppm is not cancellation-safe: every resize retires
                    // this decoder, including hiding the pane.
                    break;
                }
                frame = tokio::time::timeout(Duration::from_secs(15), read_ppm(&mut reader)) => {
                    frames.push(frame.map_err(|_| "Video stream stalled")??);
                }
            }
        }
    }
}

fn decoder_candidates() -> Vec<PathBuf> {
    media_candidates("ffmpeg")
}

fn media_candidates(name: &str) -> Vec<PathBuf> {
    // GUI apps and long-lived tmux servers may not inherit the shell's PATH.
    // Prefer PATH, but also find standard installations without shell startup.
    let mut candidates = vec![PathBuf::from(name)];
    if cfg!(target_os = "macos") {
        candidates
            .extend(["/opt/homebrew/bin", "/usr/local/bin"].map(|dir| Path::new(dir).join(name)));
    } else if cfg!(target_os = "linux") {
        candidates
            .extend(["/usr/local/bin", "/usr/bin", "/bin"].map(|dir| Path::new(dir).join(name)));
    }
    candidates
}

fn decoder_command(program: &Path, pixels: Option<(u32, u32)>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-flags",
        "low_delay",
        "-f",
        "h264",
        "-i",
        "pipe:0",
        "-an",
    ]);
    if let Some((width, height)) = pixels {
        command.args(["-filter_threads", "2", "-vf", &format!("scale=w=\'min(iw,{width})\':h=\'min(ih,{height})\':force_original_aspect_ratio=decrease:flags=bilinear")]);
    }
    command
        .args([
            "-f",
            "image2pipe",
            "-vcodec",
            "ppm",
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
            "1:90000",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x08000000);
    command
}

#[cfg(test)]
fn spawn_decoder(candidates: &[PathBuf]) -> Result<tokio::process::Child> {
    spawn_decoder_scaled(candidates, None)
}
fn spawn_decoder_scaled(
    candidates: &[PathBuf],
    pixels: Option<(u32, u32)>,
) -> Result<tokio::process::Child> {
    for program in candidates {
        match decoder_command(program, pixels).spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "Could not start video decoder {}: {error}",
                    program.display()
                )
                .into());
            }
        }
    }
    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "FFmpeg was not found (searched {searched}). Install ffmpeg locally to decode Hand video."
    )
    .into())
}
async fn read_ppm<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<DynamicImage> {
    let mut header = String::new();
    reader.read_line(&mut header).await?;
    if header != "P6\n" {
        return Err("Video decoder stopped or returned an invalid frame".into());
    }
    header.clear();
    reader.read_line(&mut header).await?;
    let dimensions: Vec<u32> = header
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    if dimensions.len() != 2
        || dimensions.contains(&0)
        || dimensions[0] > 16384
        || dimensions[1] > 16384
        || u64::from(dimensions[0]) * u64::from(dimensions[1]) > 33_554_432
    {
        return Err("Video dimensions exceed limit".into());
    }
    header.clear();
    reader.read_line(&mut header).await?;
    if header != "255\n" {
        return Err("Unsupported video pixel format".into());
    }
    let mut bytes = vec![0; dimensions[0] as usize * dimensions[1] as usize * 3];
    reader.read_exact(&mut bytes).await?;
    Ok(DynamicImage::ImageRgb8(
        image::RgbImage::from_raw(dimensions[0], dimensions[1], bytes).ok_or("Invalid frame")?,
    ))
}
async fn encode_frames(
    images: Arc<VideoFrames>,
    mut size: watch::Receiver<Size>,
    picker: Picker,
    output: watch::Sender<Snapshot>,
) {
    let mut previous_image: Option<Arc<DynamicImage>> = None;
    let graphics = graphics::LocalGraphics::new(&picker);
    let mut cadence = tokio::time::interval(Duration::from_nanos(16_666_667));
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let image = tokio::select! {
            image = images.next() => image,
            result = size.changed() => {
                if result.is_err() { return; }
                let Some(image) = previous_image.clone() else { continue; };
                image
            }
        };
        cadence.tick().await;
        previous_image = Some(image.clone());
        let area = *size.borrow_and_update();
        if area.width == 0 || area.height == 0 {
            continue;
        }
        let source_size = (image.width(), image.height());
        let text_preview =
            picker.protocol_type() == ratatui_image::picker::ProtocolType::Halfblocks;
        let picker = picker.clone();
        let graphics = graphics.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<VideoFrame> {
            use ratatui_image::picker::ProtocolType;
            if let Some(graphics) = graphics
                && let Some(frame) = graphics.prepare(&image, area, picker.font_size())?
            {
                return Ok(frame);
            }
            if picker.protocol_type() == ProtocolType::Kitty {
                let font = picker.font_size();
                let image = image.resize(
                    u32::from(area.width) * u32::from(font.width),
                    u32::from(area.height) * u32::from(font.height),
                    image::imageops::FilterType::Triangle,
                );
                Ok(graphics::inline(
                    &image,
                    area,
                    font,
                    std::env::var_os("TMUX").is_some(),
                ))
            } else {
                Ok(VideoFrame::Protocol(picker.new_protocol(
                    (*image).clone(),
                    area,
                    ratatui_image::Resize::Fit(None),
                )?))
            }
        })
        .await;
        match result {
            Ok(Ok(protocol)) => {
                output.send_modify(|s| {
                    s.frame = Some(Arc::new(protocol));
                    s.status = if text_preview {
                        "Watching · text preview"
                    } else {
                        "Watching"
                    }
                    .into();
                    s.source_size = source_size;
                });
            }
            _ => {
                output.send_modify(|s| s.status = "Could not render video in this terminal".into());
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn decoder_falls_back_when_first_executable_is_missing() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let decoder = directory.path().join("ffmpeg");
        std::fs::write(&decoder, "#!/bin/sh\nprintf 'P6\\n1 1\\n255\\nRGB'\n").unwrap();
        std::fs::set_permissions(&decoder, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = spawn_decoder(&[directory.path().join("missing"), decoder]).unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        assert_eq!(
            read_ppm(&mut output).await.unwrap().to_rgb8().into_raw(),
            b"RGB"
        );
        assert!(child.wait().await.unwrap().success());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn decoder_reports_launch_failure_without_claiming_it_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let error = spawn_decoder(std::slice::from_ref(&missing))
            .unwrap_err()
            .to_string();
        assert!(error.contains("FFmpeg was not found"));
        assert!(error.contains(&missing.display().to_string()));
        let denied = directory.path().join("not-executable");
        std::fs::write(&denied, "not executable").unwrap();
        let error = spawn_decoder(&[denied.clone(), missing])
            .unwrap_err()
            .to_string();
        assert!(error.contains("Could not start video decoder"));
        assert!(error.contains(&denied.display().to_string()));
        assert!(
            error
                .to_lowercase()
                .contains(&std::io::Error::from(std::io::ErrorKind::PermissionDenied).to_string())
        );
        assert!(!error.contains("Install"));
    }
    #[tokio::test]
    async fn video_bursts_preserve_cadence_with_bounded_latency() {
        let frames = VideoFrames::default();
        for width in 1..=5 {
            frames.push(DynamicImage::new_rgb8(width, 1));
        }
        assert!(frames.has_frame());
        assert_eq!(frames.next().await.width(), 3);
        assert_eq!(frames.next().await.width(), 4);
        assert_eq!(frames.next().await.width(), 5);
        assert!(frames.queue.lock().unwrap().is_empty());
    }
    #[test]
    fn delayed_video_repairs_are_accepted_but_duplicates_are_rejected() {
        let mut detector = webrtc::srtp::option::srtp_replay_protection(VIDEO_REORDER_WINDOW)();
        assert!(detector.check(1000));
        detector.accept();
        assert!(detector.check(1500));
        detector.accept();
        assert!(detector.check(1001));
        detector.accept();
        assert!(!detector.check(1001));
        assert!(!detector.check(1500));
    }
    #[test]
    fn nack_window_fits_wire_format_after_a_burst_of_loss() {
        use webrtc::{
            rtcp::transport_feedbacks::transport_layer_nack::{
                TransportLayerNack, nack_pairs_from_sequence_numbers,
            },
            util::Marshal,
        };
        let missing: Vec<u16> = (0..2048).map(|n| 65000u16.wrapping_add(n)).collect();
        let report = TransportLayerNack {
            sender_ssrc: 1,
            media_ssrc: 2,
            nacks: nack_pairs_from_sequence_numbers(&missing),
        };
        assert!(report.marshal().is_ok());
    }
    #[test]
    fn retired_viewer_cannot_publish_into_the_next_selection() {
        let mut controller = Controller::new(Picker::halfblocks());
        let previous = controller.sender.clone();
        controller.reset();
        previous.send_modify(|snapshot| snapshot.status = "stale Hand".into());
        assert_eq!(controller.updates.borrow().status, "Connecting…");
    }
    #[test]
    fn retry_never_repeats_invalid_media_or_missing_identity() {
        for message in [
            "Screen disconnected",
            "No video frames received",
            "Video stream stalled",
        ] {
            let error: Box<dyn std::error::Error + Send + Sync> = message.into();
            assert!(retryable(error.as_ref()));
        }
        for message in [
            "Invalid video signal",
            "This Hand is no longer publishing that screen",
            "FFmpeg was not found",
            "Could not start video decoder",
        ] {
            let error: Box<dyn std::error::Error + Send + Sync> = message.into();
            assert!(!retryable(error.as_ref()));
        }
    }
    #[test]
    fn high_resolution_keyframes_can_span_hundreds_of_rtp_packets() {
        let mut samples = video_samples();
        for sequence in 0..300u16 {
            let mut payload = vec![
                0x7c,
                if sequence == 0 {
                    0x85
                } else if sequence == 299 {
                    0x45
                } else {
                    0x05
                },
            ];
            payload.extend_from_slice(&[42; 1000]);
            samples.push(webrtc::rtp::packet::Packet {
                header: webrtc::rtp::header::Header {
                    sequence_number: sequence,
                    timestamp: 90_000,
                    marker: sequence == 299,
                    ..Default::default()
                },
                payload: payload.into(),
            });
        }
        samples.push(webrtc::rtp::packet::Packet {
            header: webrtc::rtp::header::Header {
                sequence_number: 300,
                timestamp: 91_500,
                marker: true,
                ..Default::default()
            },
            payload: vec![0x61, 1].into(),
        });
        let frame = samples
            .pop()
            .expect("large keyframe survives the reorder window");
        assert_eq!(frame.data.len(), 300_005);
        assert_eq!(&frame.data[..5], &[0, 0, 0, 1, 0x65]);
    }
    #[test]
    fn catalog_rejects_invalid_dimensions_and_identity() {
        let surface = json!({"id":"desktop","machine_id":"hand","machine_name":"Hand","name":"Desktop","generation":"one","width":1920,"height":1080});
        assert_eq!(catalog(json!({"surfaces":[surface]})).unwrap().len(), 1);
        for field in ["id", "machine_id", "generation"] {
            let mut bad = surface.clone();
            bad[field] = json!("");
            assert!(catalog(json!({"surfaces":[bad]})).is_err());
        }
        let mut bad = surface;
        bad["width"] = json!(0);
        assert!(catalog(json!({"surfaces":[bad]})).is_err());
    }
    #[tokio::test]
    async fn decoder_reads_frame_boundaries_and_rejects_oversize_frames() {
        let mut bytes = &b"P6\n2 1\n255\n\xff\0\0\0\xff\0P6\n1 1\n255\n\0\0\xff"[..];
        let first = read_ppm(&mut bytes).await.unwrap();
        assert_eq!((first.width(), first.height()), (2, 1));
        assert_eq!(
            read_ppm(&mut bytes).await.unwrap().to_rgb8().into_raw(),
            [0, 0, 255]
        );
        assert!(read_ppm(&mut &b"P6\n16384 16384\n255\n"[..]).await.is_err());
    }
    #[test]
    fn endpoints_preserve_origin_and_keep_credentials_out_of_url() {
        let target =
            AttachmentTarget::new("wss://example.com/v1/account/tool-host", "secret").unwrap();
        assert_eq!(
            endpoint(&target, "screens").unwrap().as_str(),
            "https://example.com/v1/account/hands/screens"
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires explicit live account credentials and a publishing Hand"]
    async fn live_video_receiver() {
        let _logs = std::env::var("NANOCODEX_SCREEN_TEST_LOG").ok().map(|path| {
            nanocodex_observability::ObservabilityBuilder::new("screen-test", "1")
                .filter("warn,webrtc_ice=info,nanocodex2::screen=debug")
                .output(nanocodex_observability::LogOutput::File(path.into()))
                .install()
                .unwrap()
        });
        let client = ManagedClient::new(
            std::env::var("NANOCODEX_MANAGED_URL").unwrap(),
            nanocodex_managed::ManagedApiKey::parse(std::env::var("NANOCODEX_API_KEY").unwrap())
                .unwrap(),
        )
        .unwrap();
        let performance = std::env::var_os("NANOCODEX_SCREEN_TEST_PERFORMANCE").is_some();
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize(ratatui_image::FontSize {
            width: 8,
            height: 16,
        });
        if performance {
            picker.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        }
        let mut controller = Controller::new(picker);
        controller.size.send_replace(Size::new(240, 70));
        controller.command(&client, Command::List);
        let surface = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                controller.updates.changed().await.unwrap();
                let state = controller.updates.borrow_and_update().clone();
                if let Some(surface) = state.surfaces.iter().find(|s| {
                    s.machine_name
                        .to_lowercase()
                        .eq(&std::env::var("NANOCODEX_SCREEN_TEST_HAND")
                            .unwrap_or_else(|_| "omarchy-desktop".into()))
                }) {
                    break surface.clone();
                }
                assert!(!state.status.starts_with("Screen:"), "{}", state.status);
            }
        })
        .await
        .unwrap();
        controller.command(&client, Command::Watch(surface));
        tokio::time::timeout(Duration::from_secs(40), async {
            let mut frames = 0;
            let mut previous = None;
            let mut started = None;
            let area = ratatui::layout::Rect::new(0, 0, 240, 70);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(240, 70)).unwrap();
            loop {
                controller.updates.changed().await.unwrap();
                let state = controller.updates.borrow_and_update().clone();
                assert!(!state.status.starts_with("Screen:"), "{}", state.status);
                let Some(frame) = state.frame else { continue; };
                if previous.as_ref().is_some_and(|old| Arc::ptr_eq(old, &frame)) { continue; }
                if performance {
                    terminal.draw(|f| f.render_widget(frame.as_ref(), area)).unwrap();
                    frame.consume_local_pixels();
                }
                previous = Some(frame);
                frames += 1;
                let start = *started.get_or_insert_with(Instant::now);
                if performance && start.elapsed() >= Duration::from_secs(10) {
                    let fps = f64::from(frames - 1) / start.elapsed().as_secs_f64();
                    eprintln!("Prepared/rendered {frames} distinct {}x{} frames at {fps:.1} fps; audio={}, packets={}", state.source_size.0, state.source_size.1, state.audio, state.audio_packets);
                    assert!(fps >= 55.0, "60 fps target not reached: {fps:.1}");
                    assert_eq!(state.audio, "on");
                    assert!(state.audio_packets >= 250);
                    break;
                } else if !performance && frames == 30 {
                    eprintln!("Received 30 live frames at {}x{}; audio={}, packets={}", state.source_size.0, state.source_size.1, state.audio, state.audio_packets);
                    break;
                }
            }
        })
        .await
        .unwrap();
        controller.command(&client, Command::Close);
    }
}
