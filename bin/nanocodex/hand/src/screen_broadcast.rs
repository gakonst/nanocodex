//! Independent native RTMP encoder. URLs never enter diagnostics or status.
use super::screen_video::{Capture, Task, VideoSource};
use futures_util::{TryStreamExt, future::BoxFuture};
use nanocodex_remote::video::packet_stream;
use serde_json::{Value, json};
use std::{
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::watch,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) type Source = Arc<dyn Fn() -> BoxFuture<'static, Result<Command>> + Send + Sync>;
pub(crate) type RawSource =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(Capture, usize, usize)>> + Send + Sync>;
pub(crate) struct Broadcast {
    inputs: Inputs,
    task: Option<Task>,
    status: watch::Receiver<Value>,
    stop: Option<watch::Sender<bool>>,
    /// Stream ID of the current HLS playback, if the shared slot holds one.
    hls: Option<String>,
    events: watch::Sender<Value>,
}
impl Broadcast {
    pub fn new(source: Option<Source>, audio: Option<VideoSource>) -> Self {
        let (_, status) = watch::channel(json!({"status":"idle"}));
        Self {
            inputs: Inputs {
                source,
                raw: None,
                encoded: None,
                audio,
            },
            task: None,
            stop: None,
            status,
            hls: None,
            events: watch::channel(Value::Null).0,
        }
    }
    #[cfg(target_os = "macos")]
    pub fn with_raw(mut self, source: RawSource) -> Self {
        self.inputs.raw = Some(source);
        self
    }
    pub fn with_encoded(mut self, encoded: Option<VideoSource>) -> Self {
        if self.inputs.source.is_none() {
            self.inputs.encoded = encoded;
        }
        self
    }
    /// Asynchronous HLS status transitions, already in the exact broker shape.
    pub fn events(&self) -> watch::Receiver<Value> {
        self.events.subscribe()
    }
    pub async fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send_replace(true);
        }
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(3), &mut task.0)
                .await
                .is_err()
        {
            task.0.abort();
            let _ = (&mut task.0).await;
        }
        if let Some(stream) = self.hls.take() {
            let last = self.events.borrow().clone();
            // The stream task reports its own terminal status after removing its
            // scratch segments. Without one it was aborted (or panicked), so
            // cleanup is only TempDir's best-effort drop: never claim a clean stop.
            if last["stream_id"] == stream.as_str()
                && !matches!(last["status"].as_str(), Some("stopped" | "failed"))
            {
                self.events.send_replace(super::screen_hls::result(
                    &last["request_id"],
                    &stream,
                    "failed",
                    Some("broadcast_failed"),
                ));
            }
        }
        let (_, status) = watch::channel(json!({"status":"stopped"}));
        self.status = status;
    }
    pub fn supported(&self) -> bool {
        self.inputs.source.is_some() || self.inputs.encoded.is_some() || self.inputs.raw.is_some()
    }
    fn running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.0.is_finished())
    }
    /// Broker-originated HLS command. `origin` is the authenticated publisher origin;
    /// the upload URL must match it exactly. Replies never contain the URL or token.
    pub async fn hls_request(&mut self, request: &Value, origin: &url::Origin) -> Value {
        use super::screen_hls::{parse_start, result};
        let stream = request["stream_id"].as_str().unwrap_or_default().to_owned();
        let id = &request["request_id"];
        let current = self.hls.as_deref() == Some(stream.as_str());
        match request["action"].as_str() {
            Some("status") => {
                let last = self.events.borrow().clone();
                if current && last["stream_id"] == stream.as_str() {
                    let mut last = last;
                    last["request_id"] = id.clone();
                    last
                } else {
                    result(id, &stream, "stopped", None)
                }
            }
            Some("stop") => {
                if current {
                    self.stop().await;
                    // A stream that failed, including one whose scratch
                    // segments could not be removed, never reads as stopped.
                    let last = self.events.borrow().clone();
                    if last["stream_id"] == stream.as_str() && last["status"] == "failed" {
                        let mut last = last;
                        last["request_id"] = id.clone();
                        return last;
                    }
                }
                result(id, &stream, "stopped", None)
            }
            Some("start") if current && self.running() => {
                let mut last = self.events.borrow().clone();
                last["request_id"] = id.clone();
                last
            }
            Some("start") if self.running() => result(id, &stream, "failed", Some("busy")),
            Some("start") if !self.supported() => {
                result(id, &stream, "failed", Some("unsupported"))
            }
            Some("start") => {
                let preset = request["preset"].as_str().unwrap_or("720p");
                if !["720p", "1080p"].contains(&preset) {
                    return result(id, &stream, "failed", Some("invalid_request"));
                }
                let target = match parse_start(request, origin, super::screen_hls::now_ms()) {
                    Ok(target) => target,
                    Err(error) => return result(id, &stream, "failed", Some(error)),
                };
                self.stop().await;
                let starting = result(id, &stream, "starting", None);
                self.events.send_replace(starting.clone());
                let (stop, stopped) = watch::channel(false);
                self.stop = Some(stop);
                self.hls = Some(stream);
                self.task = Some(Task(tokio::spawn(super::screen_hls::run(
                    self.inputs.clone(),
                    target,
                    preset.to_owned(),
                    id.clone(),
                    self.events.clone(),
                    stopped,
                ))));
                starting
            }
            _ => result(id, &stream, "failed", Some("invalid_request")),
        }
    }
    pub async fn request(&mut self, request: &Value) -> Value {
        let error = match request["action"].as_str() {
            // The viewer RTMP path can never select a playback target or upload.
            _ if ["target", "upload", "stream_id"]
                .iter()
                .any(|key| request.get(key).is_some()) =>
            {
                Some("invalid_request")
            }
            Some("status") => None,
            Some("stop") => {
                self.stop().await;
                None
            }
            Some("start") => {
                let preset = request["preset"].as_str().unwrap_or("source");
                let url = request["url"].as_str().unwrap_or("");
                if !valid_url(url) || !["source", "1080p", "720p", "twitch", "x"].contains(&preset)
                {
                    Some("invalid_request")
                } else if self.running() {
                    Some("busy")
                } else if self.supported() {
                    self.stop().await;
                    let (sender, receiver) =
                        watch::channel(json!({"status":"starting", "preset":preset}));
                    self.status = receiver;
                    let inputs = self.inputs.clone();
                    let url = url.to_owned();
                    let preset = preset.to_owned();
                    let (stop, mut stopped) = watch::channel(false);
                    self.stop = Some(stop);
                    self.task = Some(Task(tokio::spawn(async move {
                        for attempt in 0..4 {
                            if attempt > 0 {
                                sender.send_replace(
                                    json!({"status":"reconnecting", "preset":preset}),
                                );
                                tokio::select! { _ = stopped.changed() => return, _ = tokio::time::sleep(Duration::from_secs(1 << attempt)) => {} }
                            }
                            let _ = run(&inputs, &url, &preset, &sender, &mut stopped).await;
                            if *stopped.borrow() {
                                return;
                            }
                        }
                        sender.send_replace(
                            json!({"status":"failed", "preset":preset,"error":"broadcast_failed"}),
                        );
                    })));
                    None
                } else {
                    Some("unsupported")
                }
            }
            _ => Some("invalid_request"),
        };
        let mut result = if let Some(error) = error {
            json!({"status":"failed","error":error})
        } else if self.hls.is_some() && self.running() {
            // The shared slot holds a playback stream; never describe it to a viewer.
            json!({"status":"failed","error":"busy"})
        } else {
            self.status.borrow().clone()
        };
        result["type"] = json!("broadcast_result");
        result["viewer_id"] = request["viewer_id"].clone();
        result["request_id"] = request["request_id"].clone();
        result
    }
}
fn valid_url(value: &str) -> bool {
    value.len() <= 4096
        && !value
            .bytes()
            .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
        && url::Url::parse(value).is_ok_and(|url| {
            matches!(url.scheme(), "rtmp" | "rtmps")
                && url.host_str().is_some()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && url.port() != Some(0)
                && !url.path().trim_matches('/').is_empty()
        })
}
/// `fps_mode` is the encoder's frame-timing option ([`nanocodex_bin_shared::ffmpeg::fps_mode_option`]).
fn output(
    command: Command,
    sink: &Sink<'_>,
    preset: &str,
    audio: Option<&str>,
    fps_mode: &str,
) -> Result<tokio::process::Command> {
    // Retain only native input options. Preview scaling/bitrate is never inherited.
    let args: Vec<_> = command.get_args().collect();
    let end = args
        .iter()
        .rposition(|arg| *arg == "-i")
        .ok_or("missing capture input")?
        + 2;
    if end > args.len() {
        return Err("missing capture input".into());
    }
    let mut out = tokio::process::Command::new(command.get_program());
    if let Some(cwd) = command.get_current_dir() {
        out.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            out.env(key, value);
        } else {
            out.env_remove(key);
        }
    }
    out.args(&args[..end]);
    if let Some(audio) = audio {
        out.args([
            "-thread_queue_size",
            "64",
            "-f",
            "s16le",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-i",
            audio,
        ]);
    }
    let hls = matches!(sink, Sink::Hls { .. });
    // HLS: 30 fps, 2 s GOPs, bitrates that keep 2 s segments far below 4 MiB.
    let (width, height, bitrate) = match (hls, preset) {
        (true, "1080p") => (1920, 1080, 5000),
        (true, _) => (1280, 720, 2500),
        (_, "720p") => (1280, 720, 4500),
        (_, "1080p") => (1920, 1080, 8000),
        (_, "twitch") => (1920, 1080, 6000),
        (_, "x") => (1920, 1080, 9000),
        _ => (3840, 2160, 24000),
    };
    let (fps, gop) = if hls || preset == "x" {
        ("30", "60")
    } else {
        ("60", "120")
    };
    let gop = if hls {
        "60"
    } else if preset == "x" {
        "90"
    } else {
        gop
    };
    // min(iw/ih, bound) prevents upscaling, including portrait displays.
    out.args(["-map","0:v:0","-vf", &format!("scale=w='min(iw,{width})':h='min(ih,{height})':force_original_aspect_ratio=decrease:force_divisible_by=2"),
        "-r",fps,fps_mode,"cfr","-pix_fmt","yuv420p","-profile:v","high",
        "-b:v", &format!("{bitrate}k"),"-maxrate", &format!("{bitrate}k"),"-bufsize", &format!("{}k",bitrate*2),"-g",gop]);
    if hls {
        out.args(["-force_key_frames", "expr:gte(t,n_forced*2)"]);
    }
    if cfg!(target_os = "macos") {
        out.args([
            "-c:v",
            "h264_videotoolbox",
            "-realtime",
            "1",
            "-allow_sw",
            "0",
        ]);
    } else {
        out.args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-tune",
            "zerolatency",
            "-sc_threshold",
            "0",
        ]);
    }
    if audio.is_some() {
        out.args([
            "-map",
            "1:a:0",
            "-c:a",
            "aac",
            "-b:a",
            if hls || preset == "x" { "128k" } else { "192k" },
            "-af",
            "aresample=async=1:first_pts=0",
        ]);
    } else {
        out.arg("-an");
    }
    out.args([
        "-nostdin",
        "-loglevel",
        "quiet",
        "-progress",
        "pipe:1",
        "-stats_period",
        "0.5",
    ]);
    match sink {
        Sink::Rtmp(url) => {
            if url.starts_with("rtmps:") {
                out.args(["-tls_verify", "1"]);
            }
            out.args([
                "-rw_timeout",
                "10000000",
                "-f",
                "flv",
                "-flvflags",
                "no_duration_filesize",
                url,
            ]);
        }
        Sink::Hls { dir, start } => {
            out.args([
                "-f",
                "hls",
                "-hls_time",
                "2",
                "-hls_list_size",
                "6",
                "-hls_flags",
                "temp_file+independent_segments+omit_endlist",
                "-hls_segment_type",
                "mpegts",
                "-start_number",
                &start.to_string(),
                "-hls_segment_filename",
            ]);
            out.arg(dir.join("s%d.ts"));
            out.arg(dir.join(super::screen_hls::LOCAL_PLAYLIST));
        }
    }
    out.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    out.creation_flags(0x08000000);
    Ok(out)
}
/// A running capture->FFmpeg pipeline. Dropping it kills FFmpeg and its feeders.
pub(crate) struct Encoder {
    child: tokio::process::Child,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    advanced: tokio::time::Instant,
    timestamp: u64,
    pub audio: bool,
    _video: Option<Task>,
    _audio: Option<Task>,
}
impl Encoder {
    /// Resolves with `Ok(())` whenever encoded output time advances; errors when
    /// FFmpeg exits or stalls for 15 s. Cancellation-safe.
    pub async fn progress(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(self.advanced + Duration::from_secs(15)) => { let _ = self.child.kill().await; return Err("encoder stalled".into()); },
                _ = self.child.wait() => return Err("encoder stopped".into()),
                line = self.lines.next_line() => {
                    let Some(line) = line? else { return Err("encoder stopped".into()); };
                    if let Some(n) = line.strip_prefix("out_time_us=").and_then(|v|v.trim().parse::<u64>().ok()).filter(|n| *n > self.timestamp) {
                        self.timestamp = n; self.advanced = tokio::time::Instant::now();
                        return Ok(());
                    }
                }
            }
        }
    }
    pub async fn kill(mut self) {
        let _ = self.child.kill().await;
    }
}
/// Capture inputs shared by RTMP and HLS. Cloning shares the same sources.
#[derive(Clone)]
pub(crate) struct Inputs {
    source: Option<Source>,
    raw: Option<RawSource>,
    encoded: Option<VideoSource>,
    audio: Option<VideoSource>,
}
pub(crate) enum Sink<'a> {
    Rtmp(&'a str),
    /// Local 2 s MPEG-TS segments `s<N>.ts` plus `local.m3u8`, numbered from `start`.
    Hls {
        dir: &'a std::path::Path,
        start: u64,
    },
}
pub(crate) async fn spawn_encoder(
    inputs: &Inputs,
    sink: &Sink<'_>,
    preset: &str,
) -> Result<Encoder> {
    let mut video_task = None;
    let command = if let Some(raw) = &inputs.raw {
        let (capture, width, height) =
            tokio::time::timeout(Duration::from_secs(8), raw()).await??;
        let (mut reader, owner) = capture.into_bytes()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = format!("tcp://{}", listener.local_addr()?);
        video_task = Some(Task(tokio::spawn(async move {
            let _owner = owner;
            if let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_secs(15), listener.accept()).await
            {
                let _ = tokio::io::copy(&mut reader, &mut stream).await;
            }
        })));
        let mut command = Command::new("ffmpeg");
        command.args([
            "-thread_queue_size",
            "2",
            "-use_wallclock_as_timestamps",
            "1",
            "-f",
            "rawvideo",
            "-pixel_format",
            "bgra",
            "-video_size",
            &format!("{width}x{height}"),
            "-framerate",
            "60",
            "-i",
            &address,
        ]);
        command
    } else if let Some(source) = &inputs.source {
        tokio::time::timeout(Duration::from_secs(8), source()).await??
    } else {
        // Encoded capture is independent of preview peers; its wire transport may be framed.
        let capture = tokio::time::timeout(
            Duration::from_secs(8),
            inputs.encoded.as_ref().ok_or("capture unavailable")?(),
        )
        .await??;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = format!("tcp://{}", listener.local_addr()?);
        video_task = Some(Task(tokio::spawn(async move {
            let _owner = capture.owner;
            if let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_secs(15), listener.accept()).await
            {
                // This is an actual FFmpeg process boundary: concatenate Annex B
                // packets here, without rebuilding the internal framing protocol.
                let mut packets = packet_stream(capture.data);
                while let Ok(Some(packet)) = packets.try_next().await {
                    if stream.write_all(&packet).await.is_err() {
                        break;
                    }
                }
            }
        })));
        let mut command = Command::new("ffmpeg");
        command.args([
            "-use_wallclock_as_timestamps",
            "1",
            "-f",
            "h264",
            "-framerate",
            "60",
            "-i",
            &address,
        ]);
        command
    };
    // A private loopback PCM socket supports native WASAPI and PulseAudio equally.
    let capture = if let Some(audio) = &inputs.audio {
        Some(tokio::time::timeout(Duration::from_secs(3), audio()).await??)
    } else {
        None
    };
    let mut audio_task = None;
    let mut address = None;
    if let Some(capture) = capture {
        let (mut reader, owner) = capture.into_bytes()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        address = Some(format!("tcp://{}", listener.local_addr()?));
        audio_task = Some(Task(tokio::spawn(async move {
            let _owner = owner;
            if let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_secs(15), listener.accept()).await
            {
                let _ = tokio::io::copy(&mut reader, &mut stream).await;
            }
        })));
    }
    let fps_mode = nanocodex_bin_shared::ffmpeg::fps_mode_option(&command).await;
    let mut child = output(command, sink, preset, address.as_deref(), fps_mode)?.spawn()?;
    let lines = BufReader::new(child.stdout.take().ok_or("progress unavailable")?).lines();
    Ok(Encoder {
        child,
        lines,
        advanced: tokio::time::Instant::now(),
        timestamp: 0,
        audio: address.is_some(),
        _video: video_task,
        _audio: audio_task,
    })
}
async fn run(
    inputs: &Inputs,
    url: &str,
    preset: &str,
    status: &watch::Sender<Value>,
    stopped: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut encoder = spawn_encoder(inputs, &Sink::Rtmp(url), preset).await?;
    loop {
        tokio::select! {
            _ = stopped.changed() => { encoder.kill().await; return Ok(()); },
            progress = encoder.progress() => {
                progress?;
                status.send_replace(json!({"status":"live","audio":encoder.audio,"preset":preset,"fps":if preset == "x" {30} else {60},"bitrate_kbps":match preset {"720p"=>4500,"1080p"=>8000,"twitch"=>6000,"x"=>9000,_=>24000}}));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destinations_and_preview_separation() {
        assert!(valid_url("rtmps://localhost/live/private-key"));
        for url in [
            "https://host/key",
            "rtmp://host/key\n",
            "rtmp://host/key#fragment",
            "rtmp://user:pass@host/key",
            "rtmp://host/",
        ] {
            assert!(!valid_url(url));
        }
        let mut source = Command::new("ffmpeg");
        source.args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=60",
            "-vf",
            "scale=1280:1280",
            "-f",
            "h264",
            "pipe:1",
        ]);
        let out = output(
            source,
            &Sink::Rtmp("rtmp://localhost/live/secret"),
            "source",
            None,
            "-fps_mode",
        )
        .unwrap();
        let args: Vec<_> = out
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|s| s == "scale=1280:1280"));
        assert!(args.iter().any(|s| s.contains("min(iw,3840)")));
        let mut source = Command::new("ffmpeg");
        source.args(["-f", "lavfi", "-i", "testsrc2"]);
        let out = output(
            source,
            &Sink::Rtmp("rtmps://localhost/live/secret"),
            "x",
            Some("tcp://127.0.0.1:1"),
            "-fps_mode",
        )
        .unwrap();
        let args: Vec<_> = out
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        for pair in [
            ["-r", "30"],
            ["-g", "90"],
            ["-b:a", "128k"],
            ["-tls_verify", "1"],
        ] {
            assert!(args.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1]));
        }
        assert!(args.iter().any(|s| s.contains("min(iw,1920)")));
    }
    #[tokio::test]
    async fn lifecycle_and_errors_never_return_destination() {
        let source: Source = Arc::new(|| Box::pin(async { Err("capture unavailable".into()) }));
        let mut broadcast = Broadcast::new(Some(source), None);
        let start = json!({"action":"start","url":"rtmp://localhost/live/private-key", "preset":"x","viewer_id":"v","request_id":"r"});
        assert_eq!(broadcast.request(&start).await["status"], "starting");
        let busy = broadcast.request(&start).await;
        assert_eq!(busy["error"], "busy");
        assert!(!busy.to_string().contains("private-key"));
        let stopped = broadcast.request(&json!({"action":"stop"})).await;
        assert_eq!(stopped["status"], "stopped");
        assert!(broadcast.task.is_none());
        assert_eq!(
            broadcast.request(&json!({"action":"status"})).await["status"],
            "stopped"
        );
        let invalid = broadcast
            .request(&json!({"action":"start","url":"https://host/private-key"}))
            .await;
        assert_eq!(invalid["error"], "invalid_request");
        assert!(!invalid.to_string().contains("private-key"));
    }
    #[tokio::test]
    #[ignore = "requires a local RTMP receiver"]
    async fn local_rtmp_sink() {
        let url = std::env::var("NANOCODEX_RTMP_TEST_URL").unwrap();
        let preset = std::env::var("NANOCODEX_RTMP_TEST_PRESET").unwrap_or("source".into());
        let seconds = std::env::var("NANOCODEX_RTMP_TEST_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let source: Source = Arc::new(|| {
            Box::pin(async {
                let mut c = Command::new("ffmpeg");
                let size = std::env::var("NANOCODEX_RTMP_TEST_SIZE").unwrap_or("640x360".into());
                assert!(["640x360", "1920x1080", "3840x2160"].contains(&size.as_str()));
                c.args([
                    "-re",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size={size}:rate=60"),
                ]);
                Ok(c)
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
                super::super::screen_video::Capture::child(child)
            })
        });
        let encoded: VideoSource = Arc::new(|| {
            Box::pin(async {
                let child = tokio::process::Command::new("ffmpeg")
                    .args([
                        "-v",
                        "quiet",
                        "-re",
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=640x360:rate=60",
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-tune",
                        "zerolatency",
                        "-f",
                        "h264",
                        "pipe:1",
                    ])
                    .stdout(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()?;
                super::super::screen_video::Capture::child(child)
            })
        });
        let mut broadcast = if std::env::var("NANOCODEX_RTMP_TEST_ENCODED").as_deref() == Ok("1") {
            Broadcast::new(None, Some(audio)).with_encoded(Some(encoded))
        } else {
            Broadcast::new(Some(source), Some(audio))
        };
        let reply = broadcast
            .request(&json!({"action":"start","url":url,"preset":preset}))
            .await;
        assert_eq!(reply["status"], "starting");
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if broadcast.status.borrow()["status"] == "live" {
                    break;
                }
                broadcast.status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        assert_eq!(broadcast.status.borrow()["status"], "live");
        assert_eq!(
            broadcast.request(&json!({"action":"stop"})).await["status"],
            "stopped"
        );
    }
}
