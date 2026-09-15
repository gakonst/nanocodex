//! Desktop media ownership. ChatGPT uses the upstream helper; explicit PCM remains available.
use futures_util::future::AbortHandle;
use nanocodex::oai::realtime::{
    RealtimeAudio, RealtimeEvents, RealtimeSession, RealtimeSessionBuilder,
};
use nanocodex_voice_native::{ConnectionError, RealtimeWebrtcSession, RealtimeWebrtcSessionHandle};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::{AudioConfig, VoiceFailure, audio::VoiceAudio};

#[derive(Default)]
pub(crate) struct MediaControls(Mutex<(bool, Option<RealtimeWebrtcSessionHandle>, bool)>);

impl MediaControls {
    pub(crate) fn suppress(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.2 = true;
        if let Some(handle) = &state.1 {
            handle.set_speaker_suppressed(true);
        }
    }

    pub(crate) fn toggle(&self) -> Result<bool, nanocodex::oai::realtime::RealtimeError> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let muted = !state.0;
        if let Some(handle) = &state.1 {
            handle
                .set_microphone_muted(muted)
                .map_err(|_| nanocodex::oai::realtime::RealtimeError::Closed)?;
        }
        state.0 = muted;
        Ok(muted)
    }

    fn attach(&self, handle: RealtimeWebrtcSessionHandle) -> Result<(), VoiceFailure> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        handle
            .set_microphone_muted(state.0)
            .map_err(|error| VoiceFailure::Native(error.to_string()))?;
        handle.set_speaker_suppressed(state.2);
        state.1 = Some(handle);
        Ok(())
    }
}

pub(crate) struct CancelStartup(AbortHandle);
impl Drop for CancelStartup {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) enum VoiceMedia {
    Pcm(VoiceAudio),
    Native {
        handle: RealtimeWebrtcSessionHandle,
        _startup: CancelStartup,
    },
}

pub(crate) struct Connected {
    pub session: RealtimeSession,
    pub events: RealtimeEvents,
    pub audio: VoiceMedia,
    pub microphone: Option<mpsc::Receiver<RealtimeAudio>>,
}

impl VoiceMedia {
    pub(crate) async fn connect(
        builder: RealtimeSessionBuilder,
        native: bool,
        policy: AudioConfig,
        controls: Arc<MediaControls>,
    ) -> Result<Connected, VoiceFailure> {
        if !native {
            let (session, events) = builder.connect().await?;
            let (audio, microphone) = VoiceAudio::open(policy)?;
            return Ok(Connected {
                session,
                events,
                audio: Self::Pcm(audio),
                microphone: Some(microphone),
            });
        }
        if !RealtimeWebrtcSession::is_supported() {
            return Err(VoiceFailure::Native("Voice runtime unavailable. Run pnpm build:voice-native or install a voice-enabled package.".into()));
        }
        for attempt in 0..2 {
            tracing::info!(target: "nanocodex_voice", attempt, "starting native voice transport");
            let (abort, registration) = AbortHandle::new_pair();
            let startup = CancelStartup(abort);
            let offer =
                tokio::task::spawn_blocking(move || RealtimeWebrtcSession::start(registration))
                    .await
                    .map_err(|_| VoiceFailure::Native("voice startup worker failed".into()))?
                    .map_err(|error| VoiceFailure::Native(error.to_string()))?;
            let handle = offer.handle;
            controls.attach(handle.clone())?;
            let connection = builder.clone().connect_with_sdp(offer.offer_sdp).await?;
            let (sdp, session, events) = connection.into_parts();
            let peer = handle.clone();
            let result = tokio::task::spawn_blocking(move || peer.apply_answer_sdp(sdp))
                .await
                .map_err(|_| VoiceFailure::Native("voice negotiation worker failed".into()))?;
            match result {
                Ok(()) => {
                    return Ok(Connected {
                        session,
                        events,
                        audio: Self::Native {
                            handle,
                            _startup: startup,
                        },
                        microphone: None,
                    });
                }
                Err(error) => {
                    handle.close();
                    // A new attempt may create a call only after the previous backend is closed.
                    session.close().await?;
                    if attempt != 0 || error != ConnectionError::NegotiationTimedOut {
                        return Err(VoiceFailure::Native(error.to_string()));
                    }
                }
            }
        }
        unreachable!("bounded startup attempts return a connection or error")
    }

    pub(crate) fn play(&mut self, audio: &RealtimeAudio) {
        if let Self::Pcm(device) = self {
            device.play(audio);
        }
    }

    pub(crate) fn interrupt(&mut self) {
        match self {
            Self::Pcm(device) => device.interrupt(),
            Self::Native { handle, .. } => handle.set_speaker_suppressed(true),
        }
    }

    pub(crate) fn resume(&self) {
        if let Self::Native { handle, .. } = self {
            handle.set_speaker_suppressed(false);
        }
    }

    pub(crate) fn mute(&mut self, muted: bool) -> Result<(), VoiceFailure> {
        match self {
            // Native transitions are applied synchronously by MediaControls, including startup.
            Self::Native { .. } => Ok(()),
            Self::Pcm(device) => {
                device.set_muted(muted);
                Ok(())
            }
        }
    }

    pub(crate) fn levels(&self) -> Result<(u16, u16), VoiceFailure> {
        match self {
            Self::Native { handle, .. } => {
                if let Some(error) = handle.take_error() {
                    return Err(VoiceFailure::Native(error));
                }
                Ok((handle.take_microphone_peak(), handle.take_speaker_peak()))
            }
            Self::Pcm(_) => Ok((0, 0)),
        }
    }
}

impl Drop for VoiceMedia {
    fn drop(&mut self) {
        if let Self::Native { handle, .. } = self {
            handle.close();
        }
    }
}
