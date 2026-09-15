use std::{
    collections::VecDeque,
    io::Cursor,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Connection, Method, Page,
    cdp::browser_protocol::target::SessionId,
    error::CdpError,
    types::{CallId, EventMessage, Message, MethodId},
};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::{io::AsyncWriteExt, process::Command, sync::watch, task::JoinHandle, time::timeout};
use url::Url;

use crate::{BrowserVideoArtifact, trace_serialized};

use super::{BrowserError, evaluate_typed};

const VIDEO_STOP_TIMEOUT: Duration = Duration::from_secs(15);
const CAPTURE_SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const CAPTURE_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PENDING_FRAMES: usize = 2;
const MAX_BACKFILL_SECONDS: u64 = 5;
const MAX_VIDEO_FRAMES: usize = 18_000;
const HIGH_FRAME_RATE_THRESHOLD: u8 = 30;
const WEBM_BITRATE_KBPS_AT_BASE_RATE: u32 = 1_000;
pub(super) const DEFAULT_FRAMES_PER_SECOND: u8 = 30;
pub(super) const MAX_FRAMES_PER_SECOND: u8 = 60;

pub(super) struct VideoState {
    path: PathBuf,
    started_at: Instant,
    frames: Arc<AtomicUsize>,
    captured_frames: Arc<AtomicUsize>,
    frames_per_second: u8,
    width: u32,
    height: u32,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), BrowserError>>>,
    stopped: bool,
}

impl Drop for VideoState {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "encoder, screencast, acknowledgement, and cleanup ordering form one lifecycle"
)]
pub(super) async fn start(
    page: &Page,
    websocket_address: &str,
    output_dir: &Path,
    sequence: u64,
    frames_per_second: u8,
    quality: u8,
    executable: Option<&Path>,
) -> Result<VideoState, BrowserError> {
    page.bring_to_front().await?;
    let viewport: Viewport = evaluate_typed(
        page,
        "({ width: window.innerWidth, height: window.innerHeight })",
    )
    .await?;
    let path = output_dir.join(format!("browser-video-{sequence}.webm"));
    let mut command = Command::new(executable.unwrap_or_else(|| Path::new("ffmpeg")));
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-avioflags",
            "direct",
            "-fpsprobesize",
            "0",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-f",
            "image2pipe",
            "-framerate",
            &frames_per_second.to_string(),
            "-vcodec",
            "mjpeg",
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            "pad=ceil(iw/2)*2:ceil(ih/2)*2",
            "-c:v",
            "libvpx",
            "-crf",
            "30",
            "-b:v",
            &format!(
                "{}k",
                WEBM_BITRATE_KBPS_AT_BASE_RATE.max(
                    WEBM_BITRATE_KBPS_AT_BASE_RATE.saturating_mul(u32::from(frames_per_second))
                        / u32::from(HIGH_FRAME_RATE_THRESHOLD)
                )
            ),
            "-pix_fmt",
            "yuv420p",
            "-threads",
            if frames_per_second > HIGH_FRAME_RATE_THRESHOLD {
                "2"
            } else {
                "1"
            },
            "-y",
        ])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(BrowserError::VideoEncoderStart)?;
    let mut stdin = child.stdin.take().ok_or(BrowserError::VideoEncoderStdin)?;
    let (mut connection, capture_session, mut queued_events, encoded_width, encoded_height) =
        start_capture(
            websocket_address,
            page.target_id().as_ref(),
            quality,
            viewport.width,
            viewport.height,
        )
        .await?;
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let frames = Arc::new(AtomicUsize::new(0));
    let captured_frames = Arc::new(AtomicUsize::new(0));
    let task_frames = Arc::clone(&frames);
    let task_captured_frames = Arc::clone(&captured_frames);
    let task = tokio::spawn(async move {
        let period = frame_period(frames_per_second);
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let started_at = tokio::time::Instant::now();
        let max_frames_per_tick = u64::from(frames_per_second) * MAX_BACKFILL_SECONDS + 1;
        let mut pending_frames = VecDeque::<Vec<u8>>::new();
        let mut last_frame = None::<Vec<u8>>;
        let mut written_slots = 0_u64;
        loop {
            tokio::select! {
                biased;
                _ = ticker.tick() => {
                    if pending_frames.is_empty() && last_frame.is_none() {
                        continue;
                    }
                    let due_frames = frames_due(
                        started_at.elapsed(),
                        frames_per_second,
                        written_slots,
                    );
                    let retained_frames = MAX_VIDEO_FRAMES
                        .saturating_sub(task_frames.load(Ordering::Relaxed));
                    let emit_frames = due_frames
                        .min(max_frames_per_tick)
                        .min(u64::try_from(retained_frames).unwrap_or(u64::MAX));
                    for _ in 0..emit_frames {
                        if let Some(frame) = pending_frames.pop_front() {
                            last_frame = Some(frame);
                        }
                        if let Some(frame) = last_frame.as_ref() {
                            stdin.write_all(frame).await?;
                        }
                    }
                    written_slots = written_slots.saturating_add(emit_frames);
                    task_frames.fetch_add(
                        usize::try_from(emit_frames).unwrap_or(usize::MAX),
                        Ordering::Relaxed,
                    );
                    if task_frames.load(Ordering::Relaxed) >= MAX_VIDEO_FRAMES {
                        break;
                    }
                }
                message = next_message(&mut connection, &mut queued_events) => {
                    let Some(message) = message else {
                        break;
                    };
                    let message = message?;
                    if let Message::Event(event) = message {
                        if event.method == "Page.screencastFrame"
                            && capture_event_matches(&event, capture_session.as_ref())
                        {
                            if let Some(session_id) = event.params.get("sessionId").and_then(serde_json::Value::as_i64) {
                                let _ = submit_raw(
                                    &mut connection,
                                    capture_session.clone(),
                                    "Page.screencastFrameAck",
                                    serde_json::json!({ "sessionId": session_id }),
                                );
                            }
                            let encoded = event
                                .params
                                .get("data")
                                .and_then(serde_json::Value::as_str)
                                .ok_or(BrowserError::VideoFrameMissing)?;
                            let frame = STANDARD
                                .decode(encoded)
                                .map_err(BrowserError::VideoFrameDecode)?;
                            pending_frames.push_back(frame);
                            if pending_frames.len() > MAX_PENDING_FRAMES {
                                pending_frames.pop_front();
                            }
                            let captured_frame_count = task_captured_frames
                                .fetch_add(1, Ordering::Relaxed)
                                .saturating_add(1);
                            trace_serialized(
                                "devtools.Page.screencastFrame",
                                &serde_json::json!({ "capturedFrameCount": captured_frame_count }),
                            );
                        } else if event.method == "Inspector.detached"
                            && capture_event_matches(&event, capture_session.as_ref())
                        {
                            break;
                        }
                    }
                }
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        if task_frames.load(Ordering::Relaxed) < MAX_VIDEO_FRAMES
                            && let Some(frame) = pending_frames.pop_back()
                        {
                            stdin.write_all(&frame).await?;
                            task_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        break;
                    }
                }
            }
        }
        teardown_capture(&mut connection, capture_session.as_ref()).await;
        stdin.shutdown().await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(BrowserError::VideoEncoderFailed {
                status: output.status.code(),
                stderr: format!(
                    "{}\nChromium frames captured: {}; timeline frames written: {}",
                    String::from_utf8_lossy(&output.stderr)
                        .chars()
                        .take(4_000)
                        .collect::<String>(),
                    task_captured_frames.load(Ordering::Relaxed),
                    task_frames.load(Ordering::Relaxed),
                ),
            });
        }
        Ok(())
    });
    Ok(VideoState {
        path,
        started_at: Instant::now(),
        frames,
        captured_frames,
        frames_per_second,
        width: encoded_width,
        height: encoded_height,
        stop: stop_tx,
        task: Some(task),
        stopped: false,
    })
}

pub(super) async fn stop(mut state: VideoState) -> Result<BrowserVideoArtifact, BrowserError> {
    state.stopped = true;
    let _ = state.stop.send(true);
    let mut task = state.task.take().ok_or(BrowserError::VideoUnavailable)?;
    match timeout(VIDEO_STOP_TIMEOUT, &mut task).await {
        Ok(result) => result.map_err(BrowserError::VideoTask)??,
        Err(_) => {
            task.abort();
            let _ = task.await;
            return Err(BrowserError::VideoStopTimeout);
        }
    }
    let frame_count = state.frames.load(Ordering::Relaxed);
    if frame_count == 0 {
        return Err(BrowserError::VideoNoFrames);
    }
    Ok(BrowserVideoArtifact {
        path: state.path.clone(),
        frame_count,
        captured_frame_count: state.captured_frames.load(Ordering::Relaxed),
        duration_ms: u64::try_from(state.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        width: state.width,
        height: state.height,
        frames_per_second: state.frames_per_second,
    })
}

#[derive(Debug, Deserialize)]
struct RawCaptureEvent {
    method: MethodId,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    params: serde_json::Value,
}

impl Method for RawCaptureEvent {
    fn identifier(&self) -> MethodId {
        self.method.clone()
    }
}

impl EventMessage for RawCaptureEvent {
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

async fn start_capture(
    websocket_address: &str,
    target_id: &str,
    quality: u8,
    width: u32,
    height: u32,
) -> Result<
    (
        Connection<RawCaptureEvent>,
        Option<SessionId>,
        VecDeque<Message<RawCaptureEvent>>,
        u32,
        u32,
    ),
    BrowserError,
> {
    let (mut connection, session_id, mut queued_events) =
        if let Some(address) = page_websocket_address(websocket_address, target_id) {
            (
                Connection::<RawCaptureEvent>::connect(address).await?,
                None,
                VecDeque::new(),
            )
        } else {
            let mut connection = Connection::<RawCaptureEvent>::connect(websocket_address).await?;
            let attach = submit_raw(
                &mut connection,
                None,
                "Target.attachToTarget",
                serde_json::json!({ "targetId": target_id, "flatten": true }),
            )?;
            let (response, queued_events) =
                wait_for_response(&mut connection, attach, CAPTURE_SETUP_TIMEOUT).await?;
            if let Some(error) = response.error {
                return Err(BrowserError::VideoCaptureProtocol {
                    message: format!("Target.attachToTarget: {error}"),
                });
            }
            let session_id = response
                .result
                .as_ref()
                .and_then(|result| result.get("sessionId"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| BrowserError::VideoCaptureProtocol {
                    message: "Target.attachToTarget omitted sessionId".to_owned(),
                })?;
            (
                connection,
                Some(SessionId::new(session_id.to_owned())),
                queued_events,
            )
        };
    let enable = submit_raw(
        &mut connection,
        session_id.clone(),
        "Page.enable",
        serde_json::json!({}),
    )?;
    let (response, mut enable_events) =
        wait_for_response(&mut connection, enable, CAPTURE_SETUP_TIMEOUT).await?;
    if let Some(error) = response.error {
        return Err(BrowserError::VideoCaptureProtocol {
            message: format!("Page.enable: {error}"),
        });
    }
    queued_events.append(&mut enable_events);
    let start = submit_raw(
        &mut connection,
        session_id.clone(),
        "Page.startScreencast",
        serde_json::json!({
            "format": "jpeg",
            "quality": quality,
            "maxWidth": width,
            "maxHeight": height,
            "everyNthFrame": 1,
        }),
    )?;
    let (response, mut start_events) =
        wait_for_response(&mut connection, start, CAPTURE_SETUP_TIMEOUT).await?;
    if let Some(error) = response.error {
        return Err(BrowserError::VideoCaptureProtocol {
            message: format!("Page.startScreencast: {error}"),
        });
    }
    queued_events.append(&mut start_events);
    wait_for_initial_frame(
        &mut connection,
        session_id.as_ref(),
        &mut queued_events,
        INITIAL_FRAME_TIMEOUT,
    )
    .await?;
    let (source_width, source_height) =
        initial_frame_dimensions(&queued_events, session_id.as_ref())?;
    Ok((
        connection,
        session_id,
        queued_events,
        even_dimension(source_width),
        even_dimension(source_height),
    ))
}

fn page_websocket_address(browser_address: &str, target_id: &str) -> Option<Url> {
    let mut address = Url::parse(browser_address).ok()?;
    let prefix = address.path().split_once("/devtools/browser/")?.0;
    address.set_path(&format!("{prefix}/devtools/page/{target_id}"));
    Some(address)
}

async fn wait_for_response(
    connection: &mut Connection<RawCaptureEvent>,
    call: CallId,
    maximum: Duration,
) -> Result<
    (
        chromiumoxide::types::Response,
        VecDeque<Message<RawCaptureEvent>>,
    ),
    BrowserError,
> {
    let deadline = tokio::time::Instant::now() + maximum;
    let mut events = VecDeque::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(BrowserError::VideoCaptureTimeout);
        }
        let message = timeout(remaining, connection.next())
            .await
            .map_err(|_| BrowserError::VideoCaptureTimeout)?
            .ok_or_else(|| BrowserError::VideoCaptureProtocol {
                message: "the DevTools capture connection closed".to_owned(),
            })??;
        match message {
            Message::Response(response) if response.id == call => return Ok((response, events)),
            Message::Event(event) => events.push_back(Message::Event(event)),
            Message::Response(_) => {}
        }
    }
}

async fn wait_for_initial_frame(
    connection: &mut Connection<RawCaptureEvent>,
    session_id: Option<&SessionId>,
    queued_events: &mut VecDeque<Message<RawCaptureEvent>>,
    maximum: Duration,
) -> Result<(), BrowserError> {
    if queued_events.iter().any(|message| {
        matches!(
            message,
            Message::Event(event)
                if event.method == "Page.screencastFrame"
                    && capture_event_matches(event, session_id)
        )
    }) {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + maximum;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(BrowserError::VideoCaptureTimeout);
        }
        let message = timeout(remaining, connection.next())
            .await
            .map_err(|_| BrowserError::VideoCaptureTimeout)?
            .ok_or_else(|| BrowserError::VideoCaptureProtocol {
                message: "the DevTools capture connection closed before its first frame".to_owned(),
            })??;
        let is_initial_frame = matches!(
            &message,
            Message::Event(event)
                if event.method == "Page.screencastFrame"
                    && capture_event_matches(event, session_id)
        );
        if matches!(&message, Message::Event(_)) {
            queued_events.push_back(message);
        }
        if is_initial_frame {
            return Ok(());
        }
    }
}

fn capture_event_matches(event: &RawCaptureEvent, session_id: Option<&SessionId>) -> bool {
    event.session_id() == session_id.map(AsRef::as_ref)
}

fn initial_frame_dimensions(
    queued_events: &VecDeque<Message<RawCaptureEvent>>,
    session_id: Option<&SessionId>,
) -> Result<(u32, u32), BrowserError> {
    let event = queued_events
        .iter()
        .find_map(|message| match message {
            Message::Event(event)
                if event.method == "Page.screencastFrame"
                    && capture_event_matches(event, session_id) =>
            {
                Some(event)
            }
            _ => None,
        })
        .ok_or(BrowserError::VideoFrameMissing)?;
    let encoded = event
        .params
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or(BrowserError::VideoFrameMissing)?;
    let frame = STANDARD
        .decode(encoded)
        .map_err(BrowserError::VideoFrameDecode)?;
    Ok(image::ImageReader::new(Cursor::new(frame))
        .with_guessed_format()?
        .into_dimensions()?)
}

async fn next_message(
    connection: &mut Connection<RawCaptureEvent>,
    queued_events: &mut VecDeque<Message<RawCaptureEvent>>,
) -> Option<Result<Message<RawCaptureEvent>, CdpError>> {
    match queued_events.pop_front() {
        Some(message) => Some(Ok(message)),
        None => connection.next().await,
    }
}

async fn teardown_capture(
    connection: &mut Connection<RawCaptureEvent>,
    session_id: Option<&SessionId>,
) {
    if let Ok(stop) = submit_raw(
        connection,
        session_id.cloned(),
        "Page.stopScreencast",
        serde_json::json!({}),
    ) {
        let _ = wait_for_response(connection, stop, CAPTURE_TEARDOWN_TIMEOUT).await;
    }
    if let Some(session_id) = session_id
        && let Ok(detach) = submit_raw(
            connection,
            None,
            "Target.detachFromTarget",
            serde_json::json!({ "sessionId": session_id.as_ref() }),
        )
    {
        let _ = wait_for_response(connection, detach, CAPTURE_TEARDOWN_TIMEOUT).await;
    }
}

fn submit_raw(
    connection: &mut Connection<RawCaptureEvent>,
    session_id: Option<SessionId>,
    method: &'static str,
    params: serde_json::Value,
) -> Result<CallId, CdpError> {
    Ok(connection.submit_command(method.into(), session_id, params)?)
}

fn frame_period(frames_per_second: u8) -> Duration {
    Duration::from_nanos(1_000_000_000 / u64::from(frames_per_second.max(1)))
}

fn frames_due(elapsed: Duration, frames_per_second: u8, written_slots: u64) -> u64 {
    let elapsed_slot = elapsed
        .as_nanos()
        .saturating_mul(u128::from(frames_per_second.max(1)))
        / 1_000_000_000;
    let elapsed_slot = u64::try_from(elapsed_slot).unwrap_or(u64::MAX);
    elapsed_slot.saturating_add(1).saturating_sub(written_slots)
}

const fn even_dimension(dimension: u32) -> u32 {
    dimension.saturating_add(dimension % 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_rates_cover_smooth_and_high_frame_rate_capture() {
        assert_eq!(DEFAULT_FRAMES_PER_SECOND, 30);
        assert_eq!(MAX_FRAMES_PER_SECOND, 60);
        assert_eq!(frame_period(60), Duration::from_nanos(16_666_666));
        assert_eq!(frame_period(30), Duration::from_nanos(33_333_333));
    }

    #[test]
    fn due_frames_follow_wall_clock_slots() {
        let period = frame_period(30);
        assert_eq!(frames_due(Duration::ZERO, 30, 0), 1);
        assert_eq!(frames_due(period, 30, 1), 0);
        assert_eq!(frames_due(Duration::from_millis(34), 30, 1), 1);
        assert_eq!(frames_due(Duration::from_millis(100), 30, 1), 3);
        assert_eq!(frames_due(Duration::from_millis(100), 30, 4), 0);
    }

    #[test]
    fn catch_up_work_is_bounded_without_discarding_timeline_slots() {
        let fps = 60_u64;
        let maximum = fps * MAX_BACKFILL_SECONDS + 1;
        let due = frames_due(Duration::from_secs(20), 60, 0);
        assert!(due > maximum);
        assert_eq!(due.min(maximum), 301);
        assert_eq!(
            frames_due(Duration::from_secs(20), 60, maximum),
            due - maximum
        );
    }

    #[test]
    fn encoded_dimensions_are_even() {
        assert_eq!(even_dimension(393), 394);
        assert_eq!(even_dimension(394), 394);
    }

    #[test]
    fn standard_browser_websocket_maps_to_the_page_target() {
        let address = page_websocket_address(
            "wss://chrome.example/prefix/devtools/browser/browser-id?token=secret",
            "target-id",
        )
        .unwrap();
        assert_eq!(
            address.as_str(),
            "wss://chrome.example/prefix/devtools/page/target-id?token=secret"
        );
        assert!(page_websocket_address("wss://chrome.example/connect", "target-id").is_none());
    }
}

#[derive(Deserialize)]
struct Viewport {
    width: u32,
    height: u32,
}
