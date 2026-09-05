use nanocodex::oai::realtime::RealtimeAudio;
use tokio::sync::mpsc;

use crate::{AudioConfig, AudioError};

pub(crate) struct VoiceAudio;

impl VoiceAudio {
    pub(crate) const fn open(
        _policy: AudioConfig,
    ) -> Result<(Self, mpsc::Receiver<RealtimeAudio>), AudioError> {
        Err(AudioError::UnsupportedPlatform)
    }

    pub(crate) const fn play(&mut self, _audio: &RealtimeAudio) {}

    pub(crate) const fn interrupt(&mut self) {}
}
