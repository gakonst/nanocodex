//! Platform boundary: owned asynchronous byte streams, without pixel copies in the core.
//!
//! Video sources emit NCH264F1 length-prefixed Annex B packets (preferred), or legacy
//! Annex B with access-unit delimiters. Speaker sources emit 48 kHz stereo signed
//! 16-bit little-endian PCM. The owner must stop capture when dropped. A source is
//! a restartable factory; opening one never grants permission to capture another.
use crate::Result;
use futures_util::future::BoxFuture;
use std::sync::Arc;
use tokio::{io::AsyncRead, task::JoinHandle};

pub type CaptureSource = Arc<dyn Fn() -> BoxFuture<'static, Result<Capture>> + Send + Sync>;
pub struct Capture {
    pub reader: Box<dyn AsyncRead + Unpin + Send>,
    pub owner: Task,
}
pub struct Task(pub JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Capture {
    /// Native FFmpeg capture reports packet lengths before writing H.264 bytes.
    /// Rebuild only the known capture command, preserving its environment/cwd.
    pub fn ffmpeg(command: std::process::Command) -> Result<Self> {
        let legacy = std::env::var("NANOCODEX_SCREEN_FRAME_BOUNDARIES").as_deref() == Ok("annexb");
        if legacy {
            return Self::child(
                tokio::process::Command::from(command)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .spawn()?,
            );
        }
        let args: Vec<_> = command.get_args().collect();
        if args.len() < 3 || args[args.len() - 3..] != ["-f", "h264", "pipe:1"] {
            return Err("unsupported native FFmpeg output".into());
        }
        let mut framed = tokio::process::Command::new(command.get_program());
        if let Some(directory) = command.get_current_dir() {
            framed.current_dir(directory);
        }
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                framed.env(key, value);
            } else {
                framed.env_remove(key);
            }
        }
        // Native frameworks may write directly to stderr despite -loglevel
        // quiet (AVFoundation does this). Keep frame metadata on a private Unix socket
        // on Unix rather than letting diagnostics corrupt packet boundaries.
        #[cfg(unix)]
        let directory = tempfile::Builder::new()
            .prefix("nanocodex-video-")
            .tempdir()?;
        #[cfg(unix)]
        let metadata_path = directory.path().join("frames");
        #[cfg(unix)]
        let listener = tokio::net::UnixListener::bind(&metadata_path)?;
        #[cfg(unix)]
        let output = format!(
            "[f=framecrc:flush_packets=1]unix://{}|[f=h264:flush_packets=1]pipe:1",
            metadata_path.display()
        );
        #[cfg(not(unix))]
        let output = "[f=framecrc:flush_packets=1]pipe:2|[f=h264:flush_packets=1]pipe:1".to_owned();
        framed
            .args(["-probesize", "32", "-analyzeduration", "0"])
            .args(&args[..args.len() - 3])
            .args(["-loglevel", "quiet", "-map", "0:v:0", "-f", "tee", &output])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        framed.stderr(std::process::Stdio::null());
        #[cfg(target_os = "windows")]
        framed.creation_flags(0x08000000); // CREATE_NO_WINDOW.
        let mut child = framed.spawn()?;
        #[cfg(not(unix))]
        let metadata = child.stderr.take().ok_or("encoder metadata unavailable")?;
        let video = child.stdout.take().ok_or("encoder stdout unavailable")?;
        let (writer, reader) = tokio::io::duplex(64 * 1024);
        Ok(Self {
            reader: Box::new(reader),
            owner: Task(tokio::spawn(async move {
                #[cfg(unix)]
                let _directory = directory;
                #[cfg(unix)]
                let metadata = tokio::select! {
                    biased;
                    result = listener.accept() => match result {
                        Ok((stream, _)) => stream,
                        Err(error) => { tracing::warn!(%error, "encoder metadata connection failed"); return; }
                    },
                    _ = child.wait() => return,
                };
                if let Err(error) =
                    crate::frames::forward_encoded_frames(metadata, video, writer).await
                {
                    tracing::warn!(%error, "encoded frame forwarding stopped");
                    let _ = child.kill().await;
                }
                let _ = child.wait().await;
            })),
        })
    }
    pub fn child(mut child: tokio::process::Child) -> Result<Self> {
        let reader = child.stdout.take().ok_or("encoder stdout unavailable")?;
        Ok(Self {
            reader: Box::new(reader),
            owner: Task(tokio::spawn(async move {
                let _ = child.wait().await;
            })),
        })
    }
}
