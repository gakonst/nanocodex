//! Independent native RTMP encoder. URLs never enter diagnostics or status.
use super::screen_video::{Capture, Task, VideoSource};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::watch,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) type Source = Arc<dyn Fn() -> BoxFuture<'static, Result<Command>> + Send + Sync>;
pub(crate) type RawSource =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(Capture, usize, usize)>> + Send + Sync>;
pub(crate) struct Broadcast {
    source: Option<Source>,
    encoded: Option<VideoSource>,
    raw: Option<RawSource>,
    audio: Option<VideoSource>,
    task: Option<Task>,
    status: watch::Receiver<Value>,
    stop: Option<watch::Sender<bool>>,
}
impl Broadcast {
    pub fn new(source: Option<Source>, audio: Option<VideoSource>) -> Self {
        let (_, status) = watch::channel(json!({"status":"idle"}));
        Self {
            source,
            encoded: None,
            raw: None,
            audio,
            task: None,
            stop: None,
            status,
        }
    }
    #[cfg(target_os = "macos")]
    pub fn with_raw(mut self, source: RawSource) -> Self {
        self.raw = Some(source);
        self
    }
    pub fn with_encoded(mut self, encoded: Option<VideoSource>) -> Self {
        if self.source.is_none() {
            self.encoded = encoded;
        }
        self
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
        let (_, status) = watch::channel(json!({"status":"stopped"}));
        self.status = status;
    }
    pub fn supported(&self) -> bool {
        self.source.is_some() || self.encoded.is_some() || self.raw.is_some()
    }
    pub async fn request(&mut self, request: &Value) -> Value {
        let error = match request["action"].as_str() {
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
                } else if self.task.as_ref().is_some_and(|task| !task.0.is_finished()) {
                    Some("busy")
                } else if self.supported() {
                    let source = self.source.clone();
                    let encoded = self.encoded.clone();
                    let raw = self.raw.clone();
                    let (sender, receiver) =
                        watch::channel(json!({"status":"starting", "preset":preset}));
                    self.status = receiver;
                    let audio = self.audio.clone();
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
                            let _ = run(
                                source.as_ref(),
                                raw.as_ref(),
                                encoded.as_ref(),
                                audio.as_ref(),
                                &url,
                                &preset,
                                &sender,
                                &mut stopped,
                            )
                            .await;
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
fn output(
    command: Command,
    url: &str,
    preset: &str,
    audio: Option<&str>,
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
    let (width, height, bitrate) = match preset {
        "720p" => (1280, 720, 4500),
        "1080p" => (1920, 1080, 8000),
        "twitch" => (1920, 1080, 6000),
        "x" => (1920, 1080, 9000),
        _ => (3840, 2160, 24000),
    };
    // min(iw/ih, bound) prevents upscaling, including portrait displays.
    out.args(["-map","0:v:0","-vf", &format!("scale=w='min(iw,{width})':h='min(ih,{height})':force_original_aspect_ratio=decrease:force_divisible_by=2"),
        "-r",if preset == "x" {"30"} else {"60"},"-fps_mode","cfr","-pix_fmt","yuv420p","-profile:v","high",
        "-b:v", &format!("{bitrate}k"),"-maxrate", &format!("{bitrate}k"),"-bufsize", &format!("{}k",bitrate*2),"-g",if preset == "x" {"90"} else {"120"}]);
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
            if preset == "x" { "128k" } else { "192k" },
            "-af",
            "aresample=async=1:first_pts=0",
        ]);
    } else {
        out.arg("-an");
    }
    if url.starts_with("rtmps:") {
        out.args(["-tls_verify", "1"]);
    }
    out.args([
        "-nostdin",
        "-loglevel",
        "quiet",
        "-progress",
        "pipe:1",
        "-stats_period",
        "0.5",
        "-rw_timeout",
        "10000000",
        "-f",
        "flv",
        "-flvflags",
        "no_duration_filesize",
        url,
    ]);
    out.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    out.creation_flags(0x08000000);
    Ok(out)
}
async fn run(
    source: Option<&Source>,
    raw: Option<&RawSource>,
    encoded: Option<&VideoSource>,
    audio: Option<&VideoSource>,
    url: &str,
    preset: &str,
    status: &watch::Sender<Value>,
    stopped: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut video_task = None;
    let command = if let Some(raw) = raw {
        let (mut capture, width, height) =
            tokio::time::timeout(Duration::from_secs(8), raw()).await??;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = format!("tcp://{}", listener.local_addr()?);
        video_task = Some(Task(tokio::spawn(async move {
            let _owner = capture.owner;
            if let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_secs(15), listener.accept()).await
            {
                let _ = tokio::io::copy(&mut capture.reader, &mut stream).await;
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
    } else if let Some(source) = source {
        tokio::time::timeout(Duration::from_secs(8), source()).await??
    } else {
        // VM guest --desktop-video emits Annex B, independently of preview peers.
        let mut capture = tokio::time::timeout(
            Duration::from_secs(8),
            encoded.ok_or("capture unavailable")?(),
        )
        .await??;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = format!("tcp://{}", listener.local_addr()?);
        video_task = Some(Task(tokio::spawn(async move {
            let _owner = capture.owner;
            if let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_secs(15), listener.accept()).await
            {
                let _ = tokio::io::copy(&mut capture.reader, &mut stream).await;
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
    let _video = video_task;
    // A private loopback PCM socket supports native WASAPI and PulseAudio equally.
    let capture = if let Some(audio) = audio {
        Some(tokio::time::timeout(Duration::from_secs(3), audio()).await??)
    } else {
        None
    };
    let mut audio_task = None;
    let mut address = None;
    if let Some(mut capture) = capture {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        address = Some(format!("tcp://{}", listener.local_addr()?));
        audio_task = Some(Task(tokio::spawn(async move {
            let _owner = capture.owner;
            if let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_secs(15), listener.accept()).await
            {
                let _ = tokio::io::copy(&mut capture.reader, &mut stream).await;
            }
        })));
    }
    let _audio = audio_task;
    let mut child = output(command, url, preset, address.as_deref())?.spawn()?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("progress unavailable")?).lines();
    let mut advanced = tokio::time::Instant::now();
    let mut timestamp = 0u64;
    loop {
        tokio::select! {
            _ = stopped.changed() => { let _ = child.kill().await; return Ok(()); },
            _ = tokio::time::sleep_until(advanced + Duration::from_secs(15)) => { let _ = child.kill().await; return Err("encoder stalled".into()); },
            _ = child.wait() => return Err("encoder stopped".into()),
            line = tokio::time::timeout(Duration::from_secs(15), lines.next_line()) => {
                let Some(line) = line?? else { return Err("encoder stopped".into()); };
                if let Some(n) = line.strip_prefix("out_time_us=").and_then(|v|v.trim().parse::<u64>().ok()).filter(|n| *n > timestamp) {
                    timestamp = n; advanced = tokio::time::Instant::now();
                    status.send_replace(json!({"status":"live","audio":address.is_some(),"preset":preset,"fps":if preset == "x" {30} else {60},"bitrate_kbps":match preset {"720p"=>4500,"1080p"=>8000,"twitch"=>6000,"x"=>9000,_=>24000}}));
                }
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
        let out = output(source, "rtmp://localhost/live/secret", "source", None).unwrap();
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
            "rtmps://localhost/live/secret",
            "x",
            Some("tcp://127.0.0.1:1"),
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
