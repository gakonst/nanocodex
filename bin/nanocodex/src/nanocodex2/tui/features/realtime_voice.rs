//! Local OpenAI Realtime voice: /voice [on|off|stop|mute|list|<voice>],
//! the configurable --voice-mute-key and live captions.
//!
//! Port of the legacy worker's control_voice/forward_voice_events. The voice
//! lifecycle runs on its own thread inside nanocodex_voice; this feature owns
//! the session, serializes control through one async lock and reports status
//! and transcripts through [FeatureHost]. Available only when the local agent
//! configuration provides a Realtime client (capabilities.voice_realtime).

use std::sync::{
    Arc, Mutex as StdMutex, PoisonError,
    atomic::{AtomicU64, Ordering},
};

use crossterm::event::{KeyCode, KeyEvent};
use nanocodex::{Nanocodex, OpenAi};
use nanocodex_voice::{
    CHATGPT_REALTIME_VOICES, PLATFORM_REALTIME_VOICES, RealtimeVoice as Voice, VoiceAgentControl,
    VoiceEvent, VoiceEvents, VoiceSession, VoiceSessionBuilder, VoiceSpeaker,
};
use tokio::{sync::Mutex, task::JoinHandle};

use super::{Feature, FeatureCommand, FeatureContext, FeatureHost, FeatureUpdate, KeyOutcome};
use crate::nanocodex2::{
    tui::{local::agent::LocalParts, pane::PaneId, transcript::LocalEvent, voice_keys::MuteKey},
    voice_state::{Phase, Status, Transcript},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Control {
    Toggle,
    Start(Option<Voice>),
    Stop,
    Mute,
    List,
}

fn parse(arguments: &str) -> Result<Control, String> {
    let argument = arguments.trim();
    Ok(match argument {
        "" => Control::Toggle,
        "on" => Control::Start(None),
        "off" | "stop" => Control::Stop,
        "list" => Control::List,
        "mute" => Control::Mute,
        _ if argument.split_whitespace().count() == 1 => {
            Control::Start(Some(argument.parse().map_err(|_| {
                "Unknown voice. Use /voice list to see Codex voices.".to_owned()
            })?))
        }
        _ => return Err("Usage: /voice [on|off|stop|mute|list|<voice>]".to_owned()),
    })
}

type Slot = Arc<Mutex<Option<VoiceSession>>>;

#[derive(Default)]
pub(crate) struct RealtimeVoice {
    realtime: Option<OpenAi>,
    agent: Option<Nanocodex>,
    mute_key: Option<MuteKey>,
    slot: Slot,
    control: VoiceAgentControl,
    /// Events of retired sessions are ignored.
    generation: Arc<AtomicU64>,
    forward: Arc<StdMutex<Option<JoinHandle<()>>>>,
}

impl Feature for RealtimeVoice {
    fn name(&self) -> &'static str {
        "realtime_voice"
    }

    fn attach(&mut self, parts: &mut LocalParts, cx: &FeatureContext<'_>) {
        self.shutdown();
        self.realtime = parts.realtime.clone();
        self.agent = cx.agent.cloned();
        self.mute_key = cx
            .launch
            .and_then(|launch| MuteKey::parse(&launch.args.voice_mute_key));
    }

    fn command(&mut self, pane: PaneId, command: &FeatureCommand, cx: &FeatureContext<'_>) -> bool {
        let FeatureCommand::RealtimeVoice(arguments) = command else {
            return false;
        };
        match parse(arguments) {
            Ok(control) => self.control(control, pane, cx.host.clone()),
            Err(error) => cx.host.error(Some(pane), error),
        }
        true
    }

    fn key(&mut self, key: &KeyEvent, cx: &FeatureContext<'_>) -> KeyOutcome {
        if self.mute_key.is_some_and(|mute| mute.matches(key)) && self.active() {
            self.control(Control::Mute, PaneId::Main, cx.host.clone());
            return KeyOutcome::Consumed;
        }
        if key.code == KeyCode::Esc && self.control.has_active_turn() {
            // Esc interrupts the coding turn voice started.
            let control = self.control.clone();
            let host = cx.host.clone();
            tokio::spawn(async move {
                if let Err(error) = control.cancel().await {
                    host.error(
                        Some(PaneId::Main),
                        format!("Could not cancel the voice turn: {error}"),
                    );
                }
            });
        }
        KeyOutcome::Ignored
    }

    fn shutdown(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(forward) = self
            .forward
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            forward.abort();
        }
        let slot = Arc::clone(&self.slot);
        tokio::spawn(async move {
            if let Some(mut session) = slot.lock().await.take() {
                drop(session.shutdown().await);
            }
        });
    }
}

impl RealtimeVoice {
    fn active(&self) -> bool {
        self.slot.try_lock().map_or(true, |session| {
            session.as_ref().is_some_and(VoiceSession::is_running)
        })
    }

    fn control(&self, control: Control, pane: PaneId, host: FeatureHost) {
        if control == Control::List {
            let names = |voices: &[Voice]| {
                voices
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            host.notice(
                Some(pane),
                format!(
                    "Codex/ChatGPT voices (default cove): {}. Platform voices (default marin): {}",
                    names(CHATGPT_REALTIME_VOICES),
                    names(PLATFORM_REALTIME_VOICES)
                ),
            );
            return;
        }
        let job = Job {
            slot: Arc::clone(&self.slot),
            realtime: self.realtime.clone(),
            agent: self.agent.clone(),
            control: self.control.clone(),
            generation: Arc::clone(&self.generation),
            forward: Arc::clone(&self.forward),
            host,
            pane,
        };
        tokio::spawn(job.run(control));
    }
}

struct Job {
    slot: Slot,
    realtime: Option<OpenAi>,
    agent: Option<Nanocodex>,
    control: VoiceAgentControl,
    generation: Arc<AtomicU64>,
    forward: Arc<StdMutex<Option<JoinHandle<()>>>>,
    host: FeatureHost,
    pane: PaneId,
}

impl Job {
    async fn run(self, control: Control) {
        let mut slot = self.slot.lock().await;
        let running = slot.as_ref().is_some_and(VoiceSession::is_running);
        let voice = match control {
            Control::Mute => {
                match slot.as_ref() {
                    Some(session) if running => {
                        if let Err(error) = session.toggle_muted().await {
                            self.host.error(Some(self.pane), error.to_string());
                        }
                    }
                    _ => self
                        .host
                        .error(Some(self.pane), "Start /voice before muting."),
                }
                return;
            }
            Control::Stop | Control::Toggle if running => {
                self.stop(&mut slot).await;
                return;
            }
            Control::Stop => {
                self.host.notice(Some(self.pane), "Voice is not active");
                return;
            }
            Control::Start(_) if running => {
                self.host.error(
                    Some(self.pane),
                    "voice is already active; use /voice off before changing it",
                );
                return;
            }
            Control::Start(voice) => voice,
            Control::Toggle => None,
            Control::List => return,
        };
        // A finished session (failure) is released before a new start.
        if let Some(mut stale) = slot.take() {
            drop(stale.shutdown().await);
        }
        let (Some(realtime), Some(agent)) = (self.realtime.clone(), self.agent.clone()) else {
            self.host.error(
                Some(self.pane),
                "voice is unavailable with the selected harness or paid provider",
            );
            return;
        };
        if let Err(error) = crate::update::ensure_installed_voice_runtime().await {
            self.host.error(
                Some(self.pane),
                format!("failed to repair installed voice runtime: {error:#}"),
            );
            return;
        }
        let chatgpt = realtime.auth_mode() == nanocodex::oai::auth::OpenAiAuthMode::ChatGpt;
        let session_id: Arc<str> = Arc::from(agent.session_id());
        let mut builder = VoiceSessionBuilder::new(realtime, agent)
            .client_managed_handoffs(chatgpt)
            .include_startup_context(!chatgpt)
            .session_id(Arc::clone(&session_id))
            .agent_control(self.control.clone());
        if let Some(voice) = voice {
            builder = builder.voice(voice);
        }
        match builder.spawn() {
            Ok((session, events)) => {
                let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
                let forward = tokio::spawn(forward(
                    events,
                    self.host.clone(),
                    Arc::clone(&self.generation),
                    generation,
                    session_id.to_string(),
                ));
                if let Some(old) = self
                    .forward
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .replace(forward)
                {
                    old.abort();
                }
                *slot = Some(session);
            }
            Err(error) => self.host.error(
                Some(self.pane),
                format!("failed to start voice thread: {error}"),
            ),
        }
    }

    async fn stop(&self, slot: &mut Option<VoiceSession>) {
        let Some(mut session) = slot.take() else {
            return;
        };
        self.host.send(FeatureUpdate::VoiceStatus(Some(Status {
            text: "Voice stopping…".into(),
            phase: Phase::Stopping,
            ..Status::default()
        })));
        let result = session.shutdown().await;
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(forward) = self
            .forward
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            forward.abort();
        }
        self.host.send(FeatureUpdate::VoiceStatus(None));
        match result {
            Ok(()) => self.host.notice(Some(self.pane), "Voice stopped"),
            Err(error) => self.host.error(
                Some(self.pane),
                format!("failed to stop voice cleanly: {error}"),
            ),
        }
    }
}

/// One caption accumulating for a speaker until its transcript completes.
#[derive(Default)]
struct Caption {
    id: Option<u64>,
    text: String,
}

async fn forward(
    mut events: VoiceEvents,
    host: FeatureHost,
    current: Arc<AtomicU64>,
    generation: u64,
    session: String,
) {
    let mut status = Status {
        text: "Voice connecting…".into(),
        ..Status::default()
    };
    let mut next_id = 0_u64;
    let mut user = Caption::default();
    let mut assistant = Caption::default();
    while let Some(event) = events.recv().await {
        if current.load(Ordering::Acquire) != generation {
            return;
        }
        let mut transcript = |speaker: VoiceSpeaker, text: String, partial: bool| {
            let caption = match speaker {
                VoiceSpeaker::User => &mut user,
                VoiceSpeaker::Assistant => &mut assistant,
            };
            let id = *caption.id.get_or_insert_with(|| {
                next_id += 1;
                next_id
            });
            if partial {
                caption.text.push_str(&text);
            } else {
                caption.text = text;
            }
            let record = Transcript {
                session: session.clone(),
                speaker: match speaker {
                    VoiceSpeaker::User => "user",
                    VoiceSpeaker::Assistant => "assistant",
                }
                .to_owned(),
                id,
                text: caption.text.clone(),
                is_partial: partial,
            };
            if !partial {
                *caption = Caption::default();
            }
            host.send(FeatureUpdate::Record {
                pane: Some(PaneId::Main),
                event: LocalEvent::VoiceTranscript(record),
            });
        };
        match event {
            VoiceEvent::Connecting => {
                status.phase = Phase::Connecting;
                "Voice connecting…".clone_into(&mut status.text);
            }
            VoiceEvent::Started { voice } => {
                status.phase = Phase::Active;
                status.text = format!("Voice active ({voice}) — /voice off to stop");
                host.notice(Some(PaneId::Main), status.text.clone());
            }
            VoiceEvent::AudioLevels {
                microphone,
                speaker,
                muted,
            } => {
                status.microphone = microphone;
                status.speaker = speaker;
                status.speaking = speaker > 0;
                status.muted = muted;
            }
            VoiceEvent::TranscriptDelta { speaker, delta } => {
                transcript(speaker, delta, true);
                continue;
            }
            VoiceEvent::Transcript { speaker, text } => {
                transcript(speaker, text, false);
                continue;
            }
            VoiceEvent::UndeliveredAnswer { text } => {
                host.notice(Some(PaneId::Main), text);
                continue;
            }
            VoiceEvent::Failed { error } => {
                host.error(Some(PaneId::Main), format!("Voice failed: {error}"));
                host.send(FeatureUpdate::VoiceStatus(None));
                return;
            }
            VoiceEvent::Stopped => {
                host.send(FeatureUpdate::VoiceStatus(None));
                return;
            }
        }
        host.send(FeatureUpdate::VoiceStatus(Some(status.clone())));
    }
}
