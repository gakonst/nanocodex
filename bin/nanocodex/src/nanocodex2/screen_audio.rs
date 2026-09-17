//! Desktop-output PCM only, encoded as 20 ms stereo Opus for the existing peer.
//! Audio is optional: device/encoder failure never tears down the video stream.
#[cfg(any(target_os = "linux", target_os = "windows", test))]
use super::screen_video::Capture;
use super::screen_video::{Task, VideoSource};
use opusic_c::{Application, Channels, Encoder, SampleRate};
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
use webrtc::{
    media::Sample, rtp_transceiver::rtp_codec::RTCRtpCodecCapability,
    track::track_local::track_local_static_sample::TrackLocalStaticSample,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const FRAME_SAMPLES: usize = 960 * 2;

pub(crate) struct Audio {
    pub track: Arc<TrackLocalStaticSample>,
    _capture: Task,
}
impl Audio {
    pub async fn start(source: &VideoSource) -> Result<Self> {
        let mut capture = tokio::time::timeout(Duration::from_secs(3), source()).await??;
        let mut encoder = Encoder::new(Channels::Stereo, SampleRate::Hz48000, Application::Audio)
            .map_err(|e| std::io::Error::other(e.message()))?;
        encoder
            .set_bitrate(opusic_c::Bitrate::Value(128_000))
            .and_then(|()| encoder.set_inband_fec(opusic_c::InbandFec::Mode1))
            .and_then(|()| encoder.set_packet_loss(5))
            .map_err(|e| std::io::Error::other(e.message()))?;
        let track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: "audio/opus".into(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1".into(),
                ..Default::default()
            },
            "desktop-audio".into(),
            "nanocodex".into(),
        ));
        let writer = track.clone();
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let task = Task(tokio::spawn(async move {
            let _owner = capture.owner;
            let mut ready = Some(ready);
            let mut bytes = [0u8; FRAME_SAMPLES * 2];
            let mut pcm = [0f32; FRAME_SAMPLES];
            let mut encoded = [0u8; 4000];
            let result: Result<()> = async {
                loop {
                    tokio::time::timeout(
                        Duration::from_secs(5),
                        capture.reader.read_exact(&mut bytes),
                    )
                    .await??;
                    for (sample, pair) in pcm.iter_mut().zip(bytes.chunks_exact(2)) {
                        *sample = f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0;
                    }
                    let count = encoder
                        .encode_float_to_slice(&pcm, &mut encoded)
                        .map_err(|e| std::io::Error::other(e.message()))?;
                    tokio::time::timeout(
                        Duration::from_millis(250),
                        writer.write_sample(&Sample {
                            data: encoded[..count].to_vec().into(),
                            duration: Duration::from_millis(20),
                            ..Default::default()
                        }),
                    )
                    .await??;
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(());
                    }
                }
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(%error, "desktop audio capture stopped");
            }
        }));
        tokio::time::timeout(Duration::from_secs(3), waiting).await??;
        Ok(Self {
            track,
            _capture: task,
        })
    }
}

pub(crate) fn native_source() -> Option<VideoSource> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        Some(Arc::new(|| Box::pin(native_capture())))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
async fn native_capture() -> Result<Capture> {
    use std::process::Stdio;
    let output = tokio::process::Command::new("pactl")
        .arg("get-default-sink")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err("desktop playback sink unavailable".into());
    }
    let name = std::str::from_utf8(&output.stdout)?.trim();
    let output = tokio::process::Command::new("pactl")
        .args(["--format=json", "list", "sinks"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err("desktop playback monitors unavailable".into());
    }
    let monitor = monitor_source(&output.stdout, name)?;
    let child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "pulse",
            "-fragment_size",
            "3840",
            "-i",
            &monitor,
            "-vn",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    Capture::child(child)
}

#[cfg(any(target_os = "linux", test))]
fn monitor_source(data: &[u8], name: &str) -> Result<String> {
    let sinks: Vec<serde_json::Value> = serde_json::from_slice(data)?;
    sinks
        .iter()
        .find(|sink| !name.is_empty() && sink["name"] == name)
        .and_then(|sink| {
            sink["monitor_source_name"]
                .as_str()
                .or_else(|| sink["monitor_source"].as_str())
        })
        .filter(|monitor| !monitor.is_empty() && monitor.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| "desktop playback monitor unavailable".into())
}

#[cfg(target_os = "windows")]
async fn native_capture() -> Result<Capture> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncWriteExt;
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    struct Writer {
        pipe: tokio::io::DuplexStream,
        runtime: tokio::runtime::Handle,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.runtime.block_on(self.pipe.write(data))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (reader, pipe) = tokio::io::duplex(FRAME_SAMPLES * 2 * 4);
    let stop = Arc::new(AtomicBool::new(false));
    let guard = Stop(stop.clone());
    let runtime = tokio::runtime::Handle::current();
    let thread = tokio::task::spawn_blocking(move || {
        nanocodex_hand::capture_audio(Writer { pipe, runtime }, stop)
    });
    Ok(Capture {
        reader: Box::new(reader),
        owner: Task(tokio::spawn(async move {
            let _stop = guard;
            match thread.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "desktop audio source stopped"),
                Err(error) => tracing::warn!(%error, "desktop audio worker stopped"),
            }
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_monitor_never_falls_back_to_microphone() {
        let data = br#"[{"name":"speaker","monitor_source_name":"speaker.monitor"}]"#;
        assert_eq!(monitor_source(data, "speaker").unwrap(), "speaker.monitor");
        for name in ["", "default", "microphone"] {
            assert!(monitor_source(data, name).is_err());
        }
        assert!(monitor_source(br#"[{"name":"speaker"}]"#, "speaker").is_err());
    }
    #[tokio::test]
    async fn opus_track_starts_from_pcm_and_stops_its_source_on_drop() {
        let source: VideoSource = Arc::new(|| {
            Box::pin(async {
                use tokio::io::AsyncWriteExt;
                let (reader, mut writer) = tokio::io::duplex(FRAME_SAMPLES * 2);
                Ok(Capture {
                    reader: Box::new(reader),
                    owner: Task(tokio::spawn(async move {
                        while writer.write_all(&[0u8; FRAME_SAMPLES * 2]).await.is_ok() {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    })),
                })
            })
        });
        let audio = Audio::start(&source).await.unwrap();
        assert_eq!(audio.track.codec().mime_type, "audio/opus");
        drop(audio);
    }
}
