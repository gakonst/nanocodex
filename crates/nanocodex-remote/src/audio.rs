//! Desktop-output PCM only, encoded as 20 ms stereo Opus for the existing peer.
//! Audio is optional: device/encoder failure never tears down the video stream.
#[cfg(test)]
use crate::video::Capture;
use crate::video::{Task, VideoSource};
use opusic_c::{Application, Channels, Encoder, SampleRate};
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
use webrtc::{
    media::Sample, rtp_transceiver::rtp_codec::RTCRtpCodecCapability,
    track::track_local::track_local_static_sample::TrackLocalStaticSample,
};

use crate::Result;
pub const FRAME_SAMPLES: usize = 960 * 2;

pub struct Audio {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn opus_track_starts_from_pcm_and_stops_its_source_on_drop() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Stopped(Arc<AtomicBool>);
        impl Drop for Stopped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let observed = stopped.clone();
        let source: VideoSource = Arc::new(move || {
            let stopped = stopped.clone();
            Box::pin(async move {
                use tokio::io::AsyncWriteExt;
                let (reader, mut writer) = tokio::io::duplex(FRAME_SAMPLES * 2);
                Ok(Capture {
                    reader: Box::new(reader),
                    owner: Task(tokio::spawn(async move {
                        let _stopped = Stopped(stopped);
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
        tokio::time::timeout(Duration::from_secs(1), async {
            while !observed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping audio must cancel its capture owner");
    }
}
