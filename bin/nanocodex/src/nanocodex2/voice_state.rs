//! Bounded voice presentation shared by the native session and terminal clients.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Phase {
    #[default]
    Connecting,
    Active,
    Stopping,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Status {
    pub text: String,
    pub finished: bool,
    pub phase: Phase,
    pub muted: bool,
    pub microphone: u16,
    pub speaker: u16,
    pub speaking: bool,
}

/// One accumulated caption snapshot, identified across speakers and calls.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Transcript {
    pub session: String,
    pub speaker: String,
    pub id: u64,
    pub text: String,
    pub is_partial: bool,
}
