//! Rust host for libWebRTC's native audio device, echo canceller and adaptive
//! NetEq playout. Reuses the application's bounded, credential-free control pipe.
mod pcm;

use anyhow::{Context, Result, bail, ensure};
use codex_realtime_webrtc::{AudioControls, AudioState, Message, encode_frame, read_message};
use libwebrtc::{
    audio_track::RtcAudioTrack,
    data_channel::{DataChannel, DataChannelInit},
    peer_connection::{OfferOptions, PeerConnection, PeerConnectionState},
    peer_connection_factory::{
        PeerConnectionFactory, RtcConfiguration, native::PeerConnectionFactoryExt,
    },
    session_description::{SdpType, SessionDescription},
    stats::RtcStats,
};
use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
const BUILD_COMMIT: &str = match option_env!("STABLE_GIT_COMMIT") {
    Some(commit) => commit,
    None => "dev",
};

fn main() {
    codex_process_hardening::pre_main_hardening();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--build-commit"] {
        println!("{BUILD_COMMIT}");
        return;
    }
    if args == ["--licenses"] {
        print!("{}", include_str!(env!("NANOCODEX_WEBRTC_LICENSE")));
        return;
    }
    if !args.is_empty() {
        std::process::exit(2);
    }
    // Parent loss kills the helper even if a device driver or SDP call blocks.
    let (sender, mut messages) = tokio::sync::mpsc::channel(8);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            match read_message(&mut input) {
                Ok(Some(message)) => {
                    if sender.try_send(message).is_err() {
                        std::process::exit(1);
                    }
                }
                Ok(None) => std::process::exit(0),
                Err(_) => std::process::exit(1),
            }
        }
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    if runtime
        .block_on(async {
            ensure!(
                messages.recv().await
                    == Some(Message::Hello {
                        protocol: 1,
                        build_commit: BUILD_COMMIT.into()
                    }),
                "incompatible helper"
            );
            reply(Message::Ready {})?;
            let mut phase = 1;
            let mut media = None;
            while let Some(message) = messages.recv().await {
                let response = match message {
                    Message::InitializeRuntime {} if phase == 1 => {
                        phase = 2;
                        Message::RuntimeReady {}
                    }
                    Message::StartTransport {} if phase == 2 => {
                        let (transport, offer) = Media::start().await?;
                        media = Some(transport);
                        phase = 3;
                        Message::Offer {
                            sdp: offer.try_into().map_err(anyhow::Error::msg)?,
                        }
                    }
                    Message::ApplyAnswer { sdp } if phase == 3 => {
                        match tokio::time::timeout(
                            Duration::from_secs(15),
                            media.as_ref().unwrap().answer(sdp.into_sdp()),
                        )
                        .await
                        {
                            Ok(result) => {
                                result?;
                                phase = 4;
                                Message::TransportReady {}
                            }
                            Err(_) => {
                                reply(Message::TransportTimedOut {})?;
                                break;
                            }
                        }
                    }
                    Message::OpenDevices {} if phase == 4 => {
                        media.as_mut().unwrap().open_devices()?;
                        phase = 5;
                        Message::DevicesOpened {}
                    }
                    Message::SetAudioControls { controls } if phase == 5 => {
                        media.as_ref().unwrap().controls(controls)?;
                        Message::AudioControlsApplied {}
                    }
                    Message::BeginPcm {
                        generation,
                        sample_rate,
                    } if phase == 5 => {
                        let status = media.as_mut().unwrap().pcm.begin(generation, sample_rate);
                        Message::PcmState { generation, status }
                    }
                    Message::WritePcm {
                        generation,
                        samples,
                    } if phase == 5 => {
                        let status = media.as_mut().unwrap().pcm.write(generation, &samples);
                        Message::PcmState { generation, status }
                    }
                    Message::DrainPcm { generation } if phase == 5 => {
                        let status = media.as_mut().unwrap().pcm.drain(generation);
                        Message::PcmState { generation, status }
                    }
                    Message::CancelPcm { generation } if phase == 5 => {
                        let status = media.as_mut().unwrap().pcm.cancel(generation);
                        Message::PcmState { generation, status }
                    }
                    Message::InspectAudio {} if phase >= 2 => {
                        let state = match &media {
                            Some(media) => media.levels().await?,
                            None => AudioState::default(),
                        };
                        Message::AudioState { state }
                    }
                    Message::Close {} => {
                        drop(media.take());
                        reply(Message::Closed {})?;
                        return Ok(());
                    }
                    _ => bail!("invalid control sequence"),
                };
                reply(response)?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .is_err()
    {
        std::process::exit(1);
    }
}

fn reply(message: Message) -> Result<()> {
    let mut output = io::stdout().lock();
    output.write_all(&encode_frame(&message)?)?;
    output.flush()?;
    Ok(())
}

struct Media {
    factory: PeerConnectionFactory,
    pcm: pcm::Pcm,
    peer: PeerConnection,
    microphone: RtcAudioTrack,
    channel: DataChannel,
    playback: Arc<AtomicBool>,
    devices: bool,
}
impl Media {
    async fn start() -> Result<(Self, String)> {
        let pcm = pcm::Pcm::new();
        let factory = PeerConnectionFactory::default();
        factory.set_adm_recording_enabled(false);
        factory.set_adm_playout_enabled(false);
        let peer = factory.create_peer_connection(RtcConfiguration::default())?;
        let microphone = factory.create_device_audio_track("nanocodex-microphone");
        microphone.set_enabled(false);
        peer.add_track(microphone.clone().into(), &["nanocodex-voice"])?;
        let playback = Arc::new(AtomicBool::new(false));
        let enabled = playback.clone();
        peer.on_track(Some(Box::new(move |event| {
            event.track.set_enabled(enabled.load(Ordering::Acquire));
        })));
        let channel = peer.create_data_channel("oai-events", DataChannelInit::default())?;
        // The caller uses an authenticated sideband; no provider messages are
        // retained or forwarded through this audio-only child process.
        channel.on_message(Some(Box::new(|_| {})));
        let media = Self {
            factory,
            pcm,
            peer,
            microphone,
            channel,
            playback,
            devices: false,
        };
        let offer = media
            .peer
            .create_offer(OfferOptions {
                offer_to_receive_audio: true,
                ..Default::default()
            })
            .await?;
        let sdp = offer.to_string();
        media.peer.set_local_description(offer).await?;
        Ok((media, sdp))
    }
    async fn answer(&self, sdp: String) -> Result<()> {
        let answer = SessionDescription::parse(&sdp, SdpType::Answer).context("invalid SDP")?;
        self.peer.set_remote_description(answer).await?;
        loop {
            match self.peer.connection_state() {
                PeerConnectionState::Connected => return Ok(()),
                PeerConnectionState::Failed | PeerConnectionState::Closed => {
                    bail!("transport failed")
                }
                _ => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    }
    fn open_devices(&mut self) -> Result<()> {
        ensure!(
            self.factory.acquire_platform_adm(),
            "audio device unavailable"
        );
        self.devices = true;
        Ok(())
    }
    fn controls(&self, controls: AudioControls) -> Result<()> {
        self.microphone.set_enabled(!controls.microphone_muted);
        let was_recording = self.factory.adm_recording_enabled();
        self.factory
            .set_adm_recording_enabled(!controls.microphone_muted);
        // Enabling an ADM after negotiation (or unmuting) must explicitly start
        // capture: the synthetic ADM has no pending recording operation.
        if !controls.microphone_muted && !was_recording {
            ensure!(
                self.factory.init_recording() && self.factory.start_recording(),
                "microphone unavailable"
            );
        }
        self.playback
            .store(!controls.speaker_suppressed, Ordering::Release);
        for receiver in self.peer.receivers() {
            if let Some(track) = receiver.track() {
                track.set_enabled(!controls.speaker_suppressed);
            }
        }
        // Keep the full-duplex device clock running while speech is suppressed.
        // Switching to the synthetic ADM here tears down CoreAudio playout,
        // disrupting microphone capture and resetting the echo reference.
        // The receiver track above supplies silence without restarting devices.
        self.factory.set_adm_playout_enabled(true);
        Ok(())
    }
    async fn levels(&self) -> Result<AudioState> {
        ensure!(
            !matches!(
                self.peer.connection_state(),
                PeerConnectionState::Failed | PeerConnectionState::Closed
            ),
            "transport closed"
        );
        let mut state = AudioState::default();
        for stats in self.peer.get_stats().await? {
            match stats {
                RtcStats::MediaSource(source) if self.microphone.enabled() => {
                    state.microphone_peak = peak(source.audio.audio_level)
                }
                RtcStats::InboundRtp(received) if self.playback.load(Ordering::Acquire) => {
                    state.speaker_peak = peak(received.inbound.audio_level)
                }
                _ => {}
            }
        }
        state.speaker_peak = state.speaker_peak.max(self.pcm.take_peak());
        Ok(state)
    }
}
impl Drop for Media {
    fn drop(&mut self) {
        self.microphone.set_enabled(false);
        self.factory.set_adm_recording_enabled(false);
        self.factory.set_adm_playout_enabled(false);
        self.channel.close();
        self.peer.close();
        if self.devices {
            self.factory.release_platform_adm();
        }
    }
}
fn peak(value: f64) -> u16 {
    if value.is_finite() {
        (value.clamp(0.0, 1.0) * 32767.0) as u16
    } else {
        0
    }
}

#[cfg(test)]
mod tests;
