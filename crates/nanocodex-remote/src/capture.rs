//! Owned platform capture: complete encoded packets, or raw/legacy byte streams.
//!
//! Native video keeps packet boundaries in memory. Only process/VM transports use
//! NCH264 framing or legacy Annex B with access-unit delimiters. Speaker sources
//! emit 48 kHz stereo signed 16-bit little-endian PCM. Dropping a capture stops its
//! owned producer; shared platform producers retain their own lifecycle.
use crate::Result;
use futures_util::{
    Stream, StreamExt, TryStreamExt,
    future::BoxFuture,
    stream::{self, BoxStream},
};
use std::sync::Arc;
use tokio::{io::AsyncRead, task::JoinHandle};

pub use bytes::Bytes as EncodedPacket;
pub type ByteStream = Box<dyn AsyncRead + Unpin + Send>;
pub type PacketStream = BoxStream<'static, Result<EncodedPacket>>;
pub type CaptureSource = Arc<dyn Fn() -> BoxFuture<'static, Result<Capture>> + Send + Sync>;

pub enum CaptureData {
    Bytes(ByteStream),
    Packets(PacketStream),
}
pub struct Capture {
    pub data: CaptureData,
    pub owner: Option<Task>,
}
pub struct Task(pub JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Capture {
    pub fn bytes(reader: impl AsyncRead + Unpin + Send + 'static, owner: Task) -> Self {
        Self {
            data: CaptureData::Bytes(Box::new(reader)),
            owner: Some(owner),
        }
    }
    pub fn packets(
        packets: impl Stream<Item = Result<EncodedPacket>> + Send + 'static,
        owner: Option<Task>,
    ) -> Self {
        Self {
            data: CaptureData::Packets(packets.boxed()),
            owner,
        }
    }
    /// Raw pixels and PCM consumers must never reinterpret encoded packet sources.
    pub fn into_bytes(self) -> Result<(ByteStream, Option<Task>)> {
        match self.data {
            CaptureData::Bytes(reader) => Ok((reader, self.owner)),
            CaptureData::Packets(_) => Err("capture requires a byte stream".into()),
        }
    }
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
        let (finished, exited) = tokio::sync::oneshot::channel();
        let owner = Task(tokio::spawn(async move {
            let _ = finished.send(child.wait().await);
        }));
        // Pull metadata and its matching payload only when the publisher asks for
        // a packet. No framing pipe, forwarding task, or encoded packet queue.
        let packets = stream::once(async move {
            #[cfg(unix)]
            let _directory = directory;
            #[cfg(unix)]
            let metadata = tokio::select! {
                biased;
                result = listener.accept() => result?.0,
                _ = exited => return Err::<_, Box<dyn std::error::Error + Send + Sync>>(
                    "encoder exited before metadata".into()
                ),
            };
            #[cfg(not(unix))]
            let _ = exited;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(crate::frames::EncodedFrames::new(
                metadata, video,
            ))
        })
        .map_ok(|frames| {
            stream::try_unfold(frames, |mut frames| async move {
                Ok(frames.next().await?.map(|packet| (packet, frames)))
            })
        })
        .try_flatten();
        Ok(Self::packets(packets, Some(owner)))
    }
    pub fn child(mut child: tokio::process::Child) -> Result<Self> {
        let reader = child.stdout.take().ok_or("encoder stdout unavailable")?;
        Ok(Self::bytes(
            reader,
            Task(tokio::spawn(async move {
                let _ = child.wait().await;
            })),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn ffmpeg_native_transport_delivers_single_packet() {
        let configured = std::env::var_os("NANOCODEX_TEST_FFMPEG");
        let executable = configured.clone().unwrap_or_else(|| "ffmpeg".into());
        let mut command = std::process::Command::new(executable);
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=60",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "aud=1:repeat-headers=1",
            "-f",
            "h264",
            "pipe:1",
        ]);
        let capture = match Capture::ffmpeg(command) {
            Err(error)
                if configured.is_none()
                    && error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                eprintln!("skipping native capture test: FFmpeg is not installed");
                return;
            }
            result => result.expect("start native FFmpeg capture (configured paths must exist)"),
        };
        let CaptureData::Packets(mut packets) = capture.data else {
            panic!("native capture must return complete encoded packets");
        };
        let packet = tokio::time::timeout(Duration::from_secs(5), packets.try_next())
            .await
            .expect("first packet required a following frame")
            .unwrap()
            .unwrap();
        crate::frames::validate_packet(&packet).unwrap();
        assert!(packet.len() > 10);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), packets.try_next())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        drop(capture.owner);
    }

    #[tokio::test]
    async fn dropping_pending_packet_capture_stops_owned_producer() {
        struct Stopped(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Stopped {
            fn drop(&mut self) {
                let _ = self.0.take().unwrap().send(());
            }
        }
        let (started, running) = tokio::sync::oneshot::channel();
        let (stopped, finished) = tokio::sync::oneshot::channel();
        let owner = Task(tokio::spawn(async move {
            let _stopped = Stopped(Some(stopped));
            let _ = started.send(());
            std::future::pending::<()>().await;
        }));
        running.await.unwrap();
        let mut capture = Capture::packets(stream::pending(), Some(owner));
        let CaptureData::Packets(packets) = &mut capture.data else {
            unreachable!()
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), packets.try_next())
                .await
                .is_err()
        );
        drop(capture);
        tokio::time::timeout(Duration::from_secs(1), finished)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn raw_consumers_reject_packet_sources() {
        let capture = Capture::packets(stream::empty(), None);
        assert!(capture.into_bytes().is_err());
    }
}
