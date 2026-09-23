//! Continuous 60 Hz H.264 capture, independent of agent screenshots and input.
//! Encoders expose packet boundaries; legacy Annex B remains supported. Only signaling crosses the
//! account broker; media and leased input use authenticated WebRTC peers.
use crate::capture::{CaptureData, PacketStream};
use futures_util::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    sync::{broadcast, mpsc},
};
use webrtc::{
    api::{
        APIBuilder, interceptor_registry::register_default_interceptors, media_engine::MediaEngine,
    },
    data_channel::{RTCDataChannel, data_channel_init::RTCDataChannelInit},
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer},
    interceptor::registry::Registry,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
    rtp_transceiver::rtp_codec::RTCRtpCodecCapability,
    track::track_local::{
        track_local_static_rtp::TrackLocalStaticRTP,
        track_local_static_sample::TrackLocalStaticSample,
    },
};

use crate::Result;
use crate::audio_duplex::{Microphone, SinkFactory};
pub use crate::capture::{Capture, CaptureSource as VideoSource, Task};
use webrtc::rtp_transceiver::{
    RTCRtpTransceiverInit, rtp_codec::RTPCodecType,
    rtp_transceiver_direction::RTCRtpTransceiverDirection,
};

/// Bounded incremental Annex-B parser. Never decode/re-encode agent JPEGs.
#[derive(Default)]
struct AccessUnits {
    bytes: Vec<u8>,
    scanned: usize,
    framed: Option<bool>,
    chunked: bool,
    partial: Vec<u8>,
}
impl AccessUnits {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() > 8 * 1024 * 1024 + 64 * 1024 {
            return Err("H.264 access unit exceeds limit".into());
        }
        if self.framed.is_none() {
            let magic = b"NCH264F1";
            let chunked = b"NCH264C1";
            if self.bytes.len() < magic.len()
                && (magic.starts_with(&self.bytes) || chunked.starts_with(&self.bytes))
            {
                return Ok(Vec::new());
            }
            self.chunked = self.bytes.starts_with(chunked);
            self.framed = Some(self.bytes.starts_with(magic) || self.chunked);
            if self.framed == Some(true) {
                self.bytes.drain(..magic.len());
            }
        }
        if self.framed == Some(true) {
            let mut units = Vec::new();
            while self.bytes.len() >= 4 {
                // A framed source may restart at a record boundary. Native
                // capture has a fresh pipe per child; legacy VM sources can reuse it.
                if self.bytes.starts_with(b"NCH2") {
                    if self.bytes.len() < 8 {
                        break;
                    }
                    match &self.bytes[..8] {
                        b"NCH264F1" => self.chunked = false,
                        b"NCH264C1" => self.chunked = true,
                        _ => return Err("invalid H.264 restart header".into()),
                    }
                    self.partial.clear();
                    self.bytes.drain(..8);
                    continue;
                }
                let header = u32::from_be_bytes(self.bytes[..4].try_into().unwrap());
                let final_chunk = !self.chunked || header & (1 << 31) != 0;
                let size = if self.chunked {
                    header & !(1 << 31)
                } else {
                    header
                } as usize;
                if size == 0
                    || size > 8 * 1024 * 1024
                    || (self.chunked && size > 4092)
                    || self.partial.len() + size > 8 * 1024 * 1024
                {
                    return Err("invalid H.264 frame size".into());
                }
                if self.bytes.len() < size + 4 {
                    break;
                }
                self.partial.extend_from_slice(&self.bytes[4..size + 4]);
                self.bytes.drain(..size + 4);
                if !final_chunk {
                    continue;
                }
                let frame = std::mem::take(&mut self.partial);
                if !frame.starts_with(&[0, 0, 1]) && !frame.starts_with(&[0, 0, 0, 1]) {
                    return Err("invalid framed Annex B packet".into());
                }
                units.push(frame);
            }
            return Ok(units);
        }
        if self.bytes.len() > 8 * 1024 * 1024 {
            return Err("H.264 access unit exceeds limit".into());
        }
        if self.bytes.len() >= 5
            && !self.bytes.starts_with(&[0, 0, 1])
            && !self.bytes.starts_with(&[0, 0, 0, 1])
        {
            return Err("invalid Annex B stream".into());
        }
        let mut result = Vec::new();
        let mut index = self.scanned.max(4);
        while index + 3 < self.bytes.len() {
            if self.bytes[index..index + 4] == [0, 0, 1, 9] {
                let end = if self.bytes[index - 1] == 0 {
                    index - 1
                } else {
                    index
                };
                result.push(self.bytes.drain(..end).collect());
                index = 4;
            } else {
                index += 1;
            }
        }
        self.scanned = index;
        Ok(result)
    }
}

/// Legacy process/VM sources keep their wire parser. Native packet sources pass
/// ownership straight through without serialization or payload copies. Consumers
/// at process boundaries can concatenate the resulting Annex B packets.
pub fn packet_stream(data: CaptureData) -> PacketStream {
    match data {
        CaptureData::Packets(packets) => packets,
        CaptureData::Bytes(reader) => stream::try_unfold(
            (reader, AccessUnits::default(), VecDeque::<Vec<u8>>::new()),
            |(mut reader, mut parser, mut pending)| async move {
                loop {
                    if let Some(packet) = pending.pop_front() {
                        return Ok(Some((packet.into(), (reader, parser, pending))));
                    }
                    let mut buffer = [0; 64 * 1024];
                    let count = reader.read(&mut buffer).await?;
                    if count == 0 {
                        return Ok(None);
                    }
                    pending.extend(parser.push(&buffer[..count])?);
                }
            },
        )
        .boxed(),
    }
}

pub struct Event {
    pub value: Value,
    pub outgoing: bool,
    pub created: Instant,
    active: Option<Arc<AtomicBool>>,
}
struct Connection(Arc<RTCPeerConnection>);
impl std::ops::Deref for Connection {
    type Target = RTCPeerConnection;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        let connection = self.0.clone();
        tokio::spawn(async move {
            let _ = connection.close().await;
        });
    }
}
struct Peer {
    microphone: Arc<Microphone>,
    _connection: Connection,
    control: Arc<RTCDataChannel>,
    _rtcp: Vec<Task>,
    _media: Task,
    signals: mpsc::Sender<Value>,
    _signaling: Task,
    answered: Arc<AtomicBool>,
    started: Instant,
    refresh_ice: bool,
    incoming: Option<mpsc::Receiver<Event>>,
    active: Arc<AtomicBool>,
    relay: Option<Task>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.microphone.revoke();
        self.active.store(false, Ordering::Release);
    }
}
#[derive(Default)]
struct Motion {
    latest: std::sync::Mutex<HashMap<String, Event>>,
    changed: tokio::sync::Notify,
}
#[derive(Clone)]
struct EncodedFrame {
    data: bytes::Bytes,
    captured_at: Instant,
}

pub struct Video {
    frames: broadcast::Sender<EncodedFrame>,
    audio: Option<crate::audio::Audio>,
    microphone_factory: Option<SinkFactory>,
    peers: HashMap<String, Peer>,
    preparing: crate::preparation::Preparations<Result<(Peer, Value)>>,
    events: mpsc::Sender<Event>,
    incoming: mpsc::Receiver<Event>,
    motion: Arc<Motion>,
    failed: Arc<AtomicBool>,
    _capture: Task,
}
impl Video {
    pub async fn start(source: &VideoSource, audio_source: Option<&VideoSource>) -> Result<Self> {
        Self::start_with_microphone(source, audio_source, None).await
    }
    pub async fn start_with_microphone(
        source: &VideoSource,
        audio_source: Option<&VideoSource>,
        microphone_factory: Option<SinkFactory>,
    ) -> Result<Self> {
        let capture = tokio::time::timeout(Duration::from_secs(8), source()).await??;
        // Complete encoded frames are shared without copying payloads. Each
        // viewer owns its own packetizer and sender, so network backpressure
        // cannot hold capture or another viewer's RTP stream.
        let (frames, _) = broadcast::channel::<EncodedFrame>(2);
        let (events, incoming) = mpsc::channel(128);
        let failed = Arc::new(AtomicBool::new(false));
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let captured = frames.clone();
        let failure = failed.clone();
        let task = Task(tokio::spawn(async move {
            let _owner = capture.owner;
            let mut ready = Some(ready);
            let mut packets = packet_stream(capture.data);
            let mut phase = "capture_read";
            let mut packet_bytes = 0usize;
            let result: Result<()> = async {
                loop {
                    phase = "capture_read";
                    let data = tokio::time::timeout(Duration::from_secs(5), packets.try_next())
                        .await??
                        .ok_or("encoder stopped")?;
                    packet_bytes = data.len();
                    phase = "packet_validation";
                    crate::frames::validate_packet(&data)?;
                    let _ = captured.send(EncodedFrame {
                        data,
                        captured_at: Instant::now(),
                    });
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(());
                    }
                }
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(target: "nanocodex2", stage = "screen.video.failed", phase, packet_bytes, timed_out = error.downcast_ref::<tokio::time::error::Elapsed>().is_some(), webrtc_error = ?error.downcast_ref::<webrtc::Error>().map(std::mem::discriminant));
                failure.store(true, Ordering::Release);
            }
        }));
        tokio::time::timeout(Duration::from_secs(10), waiting).await??;
        let audio = if let Some(source) = audio_source {
            match crate::audio::Audio::start(source).await {
                Ok(audio) => Some(audio),
                Err(error) => {
                    tracing::warn!(%error, "desktop audio unavailable");
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            frames,
            audio,
            microphone_factory,
            peers: HashMap::new(),
            preparing: crate::preparation::Preparations::new(),
            events,
            incoming,
            motion: Arc::new(Motion::default()),
            failed,
            _capture: task,
        })
    }
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            tokio::select! {
                biased;
                event = self.incoming.recv() => {
                    let event = event?;
                    if event.active.as_ref().is_none_or(|active| active.load(Ordering::Acquire)) { return Some(event); }
                },
                (id, _, result) = self.preparing.next() => {
                    let (value, outgoing) = match result {
                        Ok(Ok((mut peer, offer))) => {
                            let mut incoming = peer.incoming.take().expect("uninstalled peer");
                            let events = self.events.clone();
                            let active = peer.active.clone();
                            peer.relay = Some(Task(tokio::spawn(async move {
                                while let Some(mut event) = incoming.recv().await {
                                    event.active = Some(active.clone());
                                    if events.send(event).await.is_err() { break; }
                                }
                            })));
                            self.peers.insert(id, peer);
                            (offer, true)
                        },
                        _ => (json!({"type":"viewer_left","viewer_id":id}), false),
                    };
                    return Some(Event { value, outgoing, created: Instant::now(), active: None });
                },
                _ = self.motion.changed.notified() => {
                    let mut latest = self.motion.latest.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(id) = latest.keys().next().cloned() {
                        let event = latest.remove(&id);
                        if !latest.is_empty() { self.motion.changed.notify_one(); }
                        if event.as_ref().is_some_and(|event| event.active.as_ref().is_none_or(|active| active.load(Ordering::Acquire))) { return event; }
                    }
                }
            }
        }
    }
    pub fn microphone_available(&self) -> bool {
        self.microphone_factory.is_some()
    }
    /// Caller must validate the current control lease and explicit opt-in.
    pub fn set_microphone(&self, viewer: &str, enabled: bool, lease_remaining: Duration) -> bool {
        self.peers
            .get(viewer)
            .is_some_and(|p| p.microphone.set_enabled(enabled, lease_remaining))
    }
    /// Poll alongside the lease timer; transition to false must notify the viewer.
    pub fn microphone_enabled(&self, viewer: &str) -> bool {
        self.peers
            .get(viewer)
            .is_some_and(|p| p.microphone.enabled())
    }
    pub fn renew_microphone(&self, viewer: &str, lease_remaining: Duration) {
        if let Some(peer) = self.peers.get(viewer) {
            peer.microphone.renew(lease_remaining);
        }
    }
    pub fn revoke_microphone(&self, viewer: &str) {
        if let Some(peer) = self.peers.get(viewer) {
            peer.microphone.revoke();
        }
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub fn expired(&self) -> Vec<String> {
        self.peers
            .iter()
            .filter(|(_, p)| {
                (!p.answered.load(Ordering::Acquire)
                    && p.started.elapsed() > Duration::from_secs(25))
                    || (p.refresh_ice && p.started.elapsed() > Duration::from_secs(20 * 60))
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
    pub fn remove(&mut self, id: &str) {
        self.preparing.remove(id);
        self.peers.remove(id);
        self.motion
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
    pub async fn control(&mut self, id: &str, value: &Value) -> Result<()> {
        if let Some(peer) = self.peers.get(id) {
            tokio::time::timeout(
                Duration::from_secs(1),
                peer.control.send_text(value.to_string()),
            )
            .await??;
        }
        Ok(())
    }
    /// Schedule setup without holding the input loop while ICE/SDP awaits.
    pub fn add(
        &mut self,
        id: &str,
        servers: Vec<RTCIceServer>,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        if self.peers.contains_key(id)
            || self.preparing.contains(id)
            || self.peers.len() + self.preparing.len() >= 4
        {
            return Err("viewer capacity or duplicate".into());
        }
        let builder = PeerBuilder {
            frames: self.frames.clone(),
            audio: self.audio.as_ref().map(|a| a.track.clone()),
            microphone_factory: self.microphone_factory.clone(),
            motion: self.motion.clone(),
            failed: self.failed.clone(),
        };
        let viewer = id.to_owned();
        self.preparing
            .insert(id, deadline, async move {
                builder.build(&viewer, servers).await
            })
            .map_err(|_| "viewer capacity or duplicate".into())
    }
    /// A bounded per-peer actor serializes SDP and ICE independently of leased input.
    pub fn signal(&mut self, id: &str, signal: &Value) -> Result<()> {
        let peer = self.peers.get(id).ok_or("viewer not ready")?;
        peer.signals
            .try_send(signal.clone())
            .map_err(|_| "signaling queue full".into())
    }
}
struct PeerBuilder {
    frames: broadcast::Sender<EncodedFrame>,
    audio: Option<Arc<TrackLocalStaticSample>>,
    microphone_factory: Option<SinkFactory>,
    motion: Arc<Motion>,
    failed: Arc<AtomicBool>,
}
impl PeerBuilder {
    async fn build(self, id: &str, servers: Vec<RTCIceServer>) -> Result<(Peer, Value)> {
        let track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: "video/H264".into(),
                clock_rate: 90_000,
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e034".into(),
                ..Default::default()
            },
            "desktop".into(),
            "nanocodex".into(),
        ));
        let (peer_events, peer_incoming) = mpsc::channel(128);
        let active = Arc::new(AtomicBool::new(true));
        let mut engine = MediaEngine::default();
        engine.register_default_codecs()?;
        let mut registry = Registry::new();
        let media_events = peer_events.clone();
        let media_failure = self.failed.clone();
        let media_viewer = id.to_owned();
        let media_retired = AtomicBool::new(false);
        let media_fault: Arc<dyn Fn(&'static str) + Send + Sync> = Arc::new(move |outcome| {
            if media_retired.swap(true, Ordering::AcqRel) {
                return;
            }
            tracing::warn!(target: "nanocodex2", stage = "screen.video.peer_failed", outcome);
            if media_events
                .try_send(Event {
                    value: json!({"type":"viewer_left","viewer_id":media_viewer}),
                    outgoing: false,
                    created: Instant::now(),
                    active: None,
                })
                .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
            {
                media_failure.store(true, Ordering::Release);
            }
        });
        // Register before NACK so retransmissions share the same bounded writer.
        registry.add(Box::new(crate::media_guard::Guard(media_fault.clone())));
        let registry = register_default_interceptors(registry, &mut engine)?;
        let registry = crate::playout::register(&mut engine, registry)?;
        let mut settings = webrtc::api::setting_engine::SettingEngine::default();
        settings.set_include_loopback_candidate(
            std::env::var("NANOCODEX_VIDEO_INCLUDE_LOOPBACK").as_deref() == Ok("1"),
        );
        if let Ok(interface) = std::env::var("NANOCODEX_VIDEO_INTERFACE") {
            if interface.is_empty() {
                return Err("empty video interface".into());
            }
            settings.set_interface_filter(Box::new(move |name| name == interface));
        }
        if std::env::var("NANOCODEX_VIDEO_IPV4_ONLY").as_deref() == Ok("1") {
            settings.set_network_types(vec![webrtc::ice::network_type::NetworkType::Udp4]);
        }
        if let Ok(range) = std::env::var("NANOCODEX_VIDEO_UDP_PORTS") {
            let (min, max) = range.split_once('-').ok_or("video ports require MIN-MAX")?;
            let (min, max): (u16, u16) = (min.parse()?, max.parse()?);
            if min == 0 || max < min {
                return Err("invalid video UDP range".into());
            }
            settings.set_udp_network(webrtc::ice::udp_network::UDPNetwork::Ephemeral(
                webrtc::ice::udp_network::EphemeralUDP::new(min, max)?,
            ));
        }
        if let Ok(address) = std::env::var("NANOCODEX_VIDEO_ADVERTISE_IP") {
            // A VM/container can bind its private interface while advertising
            // an administrator-configured, port-preserving NAT address.
            let address: std::net::IpAddr = address.parse()?;
            if address.is_unspecified() || address.is_multicast() {
                return Err("video advertised address must be unicast".into());
            }
            settings.set_nat_1to1_ips(
                vec![address.to_string()],
                webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType::Host,
            );
        }
        let api = APIBuilder::new()
            .with_setting_engine(settings)
            .with_media_engine(engine)
            .with_interceptor_registry(registry)
            .build();
        let refresh_ice = servers.iter().any(|s| {
            s.urls
                .iter()
                .any(|url| url.starts_with("turn:") || url.starts_with("turns:"))
        });
        let connection = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers: servers,
                ..Default::default()
            })
            .await?,
        );
        let owned = Connection(connection.clone());
        let microphone = Arc::new(Microphone::install(&connection, self.microphone_factory));
        let diagnostics = Arc::new(crate::diagnostics::Budget::default());
        let path_diagnostics = diagnostics.clone();
        connection
            .dtls_transport()
            .ice_transport()
            .on_selected_candidate_pair_change(Box::new(move |pair| {
                if path_diagnostics.take() {
                    tracing::info!(target: "nanocodex2", stage = "screen.network.path",
                    local_type = %pair.local.typ, local_protocol = %pair.local.protocol,
                    remote_type = %pair.remote.typ, remote_protocol = %pair.remote.protocol);
                }
                Box::pin(async {})
            }));
        let sender = connection.add_track(track.clone()).await?;
        let rtcp = Task(tokio::spawn(async move {
            while sender.read_rtcp().await.is_ok() {}
        }));
        let mut rtcp = vec![rtcp];
        if let Some(audio) = &self.audio {
            let transceiver = connection
                .add_transceiver_from_track(
                    audio.clone(),
                    Some(RTCRtpTransceiverInit {
                        direction: if microphone.available() {
                            RTCRtpTransceiverDirection::Sendrecv
                        } else {
                            RTCRtpTransceiverDirection::Sendonly
                        },
                        send_encodings: Vec::new(),
                    }),
                )
                .await?;
            let sender = transceiver.sender().await;
            rtcp.push(Task(tokio::spawn(async move {
                while sender.read_rtcp().await.is_ok() {}
            })));
        }
        if self.audio.is_none() && microphone.available() {
            connection
                .add_transceiver_from_kind(
                    RTPCodecType::Audio,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Recvonly,
                        send_encodings: Vec::new(),
                    }),
                )
                .await?;
        }
        let control = connection
            .create_data_channel("remote-control-v1", None)
            .await?;
        let motion = connection
            .create_data_channel(
                "remote-motion-v1",
                Some(RTCDataChannelInit {
                    ordered: Some(false),
                    max_retransmits: Some(0),
                    ..Default::default()
                }),
            )
            .await?;
        for (channel, motion) in [(control.clone(), false), (motion, true)] {
            let events = peer_events.clone();
            let failed = self.failed.clone();
            let id = id.to_owned();
            let latest = self.motion.clone();
            let active = active.clone();
            let closed = peer_events.clone();
            let failure = self.failed.clone();
            let viewer = id.clone();
            channel.on_close(Box::new(move || {
                if closed
                    .try_send(Event {
                        value: json!({"type":"viewer_left","viewer_id":viewer}),
                        outgoing: false,
                        created: Instant::now(),
                        active: None,
                    })
                    .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
                {
                    tracing::warn!(target: "nanocodex2", stage = "screen.video.failed", phase = "peer_event_queue");
                    failure.store(true, Ordering::Release);
                }
                Box::pin(async {})
            }));
            channel.on_message(Box::new(move |message| {
                let value =
                    crate::input::data_channel_event(&id, motion, message.is_string, &message.data);
                if motion && value["type"] == "input" {
                    latest
                        .latest
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(
                            id.clone(),
                            Event {
                                value,
                                outgoing: false,
                                created: Instant::now(),
                                active: Some(active.clone()),
                            },
                        );
                    latest.changed.notify_one();
                    return Box::pin(async {});
                }
                // Motion is disposable. Reliable queue overflow fails closed:
                // never retain a key-down after losing its matching key-up.
                if events
                    .try_send(Event {
                        value,
                        outgoing: false,
                        created: Instant::now(),
                        active: None,
                    })
                    .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
                    && !motion
                {
                    tracing::warn!(target: "nanocodex2", stage = "screen.video.failed", phase = "peer_event_queue");
                    failed.store(true, Ordering::Release);
                }
                Box::pin(async {})
            }));
        }
        let events = peer_events.clone();
        let failed = self.failed.clone();
        let viewer = id.to_owned();
        connection.on_ice_candidate(Box::new(move |candidate| {
            if let Some(candidate) = candidate.and_then(|c| c.to_json().ok()) {
                let signal = candidate_signal(candidate);
                if events
                    .try_send(Event {
                        value: json!({"type":"signal","viewer_id":viewer,"signal":signal}),
                        outgoing: true,
                        created: Instant::now(),
                        active: None,
                    })
                    .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
                {
                    tracing::warn!(target: "nanocodex2", stage = "screen.video.failed", phase = "peer_event_queue");
                    failed.store(true, Ordering::Release);
                }
            }
            Box::pin(async {})
        }));
        let events = peer_events.clone();
        let failed = self.failed.clone();
        let viewer = id.to_owned();
        let revoke_microphone = Arc::downgrade(&microphone);
        let peer_diagnostics = diagnostics.clone();
        let connected = Arc::new(AtomicBool::new(false));
        let connection_ready = connected.clone();
        connection.on_peer_connection_state_change(Box::new(move |state| {
            connection_ready.store(
                state == RTCPeerConnectionState::Connected,
                Ordering::Release,
            );
            peer_diagnostics.event(
                "peer_connection",
                match state {
                    RTCPeerConnectionState::New => "new",
                    RTCPeerConnectionState::Connecting => "connecting",
                    RTCPeerConnectionState::Connected => "connected",
                    RTCPeerConnectionState::Disconnected => "disconnected",
                    RTCPeerConnectionState::Failed => "failed",
                    RTCPeerConnectionState::Closed => "closed",
                    _ => "unspecified",
                },
                None,
            );
            if matches!(
                state,
                RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed
                    | RTCPeerConnectionState::Disconnected
            ) && let Some(microphone) = revoke_microphone.upgrade()
            {
                microphone.revoke();
            }
            if matches!(
                state,
                RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed
                    | RTCPeerConnectionState::Disconnected
            ) && events
                .try_send(Event {
                    value: json!({"type":"viewer_left","viewer_id":viewer}),
                    outgoing: false,
                    created: Instant::now(),
                    active: None,
                })
                .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
            {
                failed.store(true, Ordering::Release);
            }
            Box::pin(async {})
        }));
        let events = peer_events.clone();
        let failed = self.failed.clone();
        let viewer = id.to_owned();
        connection.on_data_channel(Box::new(move |channel| {
            if events
                .try_send(Event {
                    value: json!({"type":"viewer_left","viewer_id":viewer}),
                    outgoing: false,
                    created: Instant::now(),
                    active: None,
                })
                .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
            {
                failed.store(true, Ordering::Release);
            }
            Box::pin(async move {
                let _ = channel.close().await;
            })
        }));
        let offer = connection.create_offer(None).await?;
        connection.set_local_description(offer.clone()).await?;
        diagnostics.event("offer", "created", None);
        let (signals, mut incoming) = mpsc::channel::<Value>(128);
        let answered = Arc::new(AtomicBool::new(false));
        let answer = answered.clone();
        let viewer = id.to_owned();
        let events = peer_events.clone();
        let failed = self.failed.clone();
        let signaling = Task(tokio::spawn(async move {
            let mut candidates = Vec::new();
            while let Some(signal) = incoming.recv().await {
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    apply_signal(&connection, &answer, &mut candidates, &signal),
                )
                .await;
                if signal["type"] == "answer" && matches!(result, Ok(Ok(()))) {
                    diagnostics.event("answer", "applied", None);
                }
                if !matches!(result, Ok(Ok(()))) {
                    diagnostics.event(
                        "apply_signal",
                        if result.is_err() {
                            "timeout"
                        } else {
                            "rejected"
                        },
                        None,
                    );
                    if events
                        .try_send(Event {
                            value: json!({"type":"viewer_left","viewer_id":viewer}),
                            outgoing: false,
                            created: Instant::now(),
                            active: None,
                        })
                        .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
                    {
                        tracing::warn!(target: "nanocodex2", stage = "screen.video.failed", phase = "peer_event_queue");
                        failed.store(true, Ordering::Release);
                    }
                    break;
                }
            }
        }));
        let mut packets = crate::video_packets::VideoPackets::new();
        let media = Task(tokio::spawn(forward_frames(
            self.frames.subscribe(),
            connected,
            media_fault,
            move |frame| {
                let packets = packets.packetize(&frame.data, frame.captured_at);
                let track = track.clone();
                async move {
                    for packet in packets? {
                        track.write_rtp_with_extensions(&packet, &[]).await?;
                    }
                    Ok(())
                }
            },
        )));
        let peer = Peer {
            microphone,
            _connection: owned,
            control,
            _rtcp: rtcp,
            _media: media,
            signals,
            _signaling: signaling,
            answered,
            started: Instant::now(),
            refresh_ice,
            incoming: Some(peer_incoming),
            active,
            relay: None,
        };
        Ok((
            peer,
            json!({"type":"signal","viewer_id":id,"signal":{"type":"offer","sdp":offer.sdp}}),
        ))
    }
}
// A resumed/new decoder must receive parameter sets with its first IDR.
// Every native encoder repeats headers; an IDR alone is not sufficient.
fn recovery_keyframe(frame: &[u8]) -> bool {
    let mut found = 0u8;
    for nal in frame.windows(4).filter(|nal| nal[..3] == [0, 0, 1]) {
        match nal[3] & 31 {
            7 => found |= 1,
            8 => found |= 2,
            5 => return found == 3,
            _ => {}
        }
    }
    false
}

async fn forward_frames<F, Fut>(
    mut frames: broadcast::Receiver<EncodedFrame>,
    connected: Arc<AtomicBool>,
    fault: Arc<dyn Fn(&'static str) + Send + Sync>,
    mut write: F,
) where
    F: FnMut(EncodedFrame) -> Fut,
    Fut: std::future::Future<Output = webrtc::error::Result<()>>,
{
    let mut need_keyframe = true;
    loop {
        let frame = match frames.recv().await {
            Ok(frame) => frame,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                need_keyframe = true;
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
        if !connected.load(Ordering::Acquire) {
            need_keyframe = true;
            continue;
        }
        if need_keyframe && !recovery_keyframe(&frame.data) {
            continue;
        }
        need_keyframe = false;
        let result = tokio::time::timeout(Duration::from_secs(1), write(frame)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                fault("frame_write_error");
                break;
            }
            Err(_) => {
                fault("frame_write_timeout");
                break;
            }
        }
    }
}

async fn apply_signal(
    connection: &RTCPeerConnection,
    answered: &AtomicBool,
    candidates: &mut Vec<RTCIceCandidateInit>,
    signal: &Value,
) -> Result<()> {
    match signal["type"].as_str() {
        Some("answer") if !answered.load(Ordering::Acquire) => {
            let sdp = signal["sdp"]
                .as_str()
                .filter(|s| s.len() <= 65536)
                .ok_or("invalid SDP")?;
            connection
                .set_remote_description(RTCSessionDescription::answer(sdp.into())?)
                .await?;
            answered.store(true, Ordering::Release);
            for candidate in candidates.drain(..) {
                connection.add_ice_candidate(candidate).await?;
            }
        }
        Some("candidate") => {
            let candidate: RTCIceCandidateInit = serde_json::from_value(signal.clone())?;
            if candidate.candidate.len() > 4096 {
                return Err("candidate too large".into());
            }
            if answered.load(Ordering::Acquire) {
                connection.add_ice_candidate(candidate).await?;
            } else if candidates.len() < 128 {
                candidates.push(candidate);
            } else {
                return Err("too many candidates".into());
            }
        }
        _ => return Err("invalid signaling".into()),
    }
    Ok(())
}

fn candidate_signal(candidate: RTCIceCandidateInit) -> Value {
    // The broker's strict schema excludes usernameFragment. This host offers
    // one bundled video m-line (mid 0) followed by its data channels.
    json!({"type":"candidate", "candidate":candidate.candidate, "sdpMid":"0", "sdpMLineIndex":0})
}

pub use crate::ice::ice_servers;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn packet_stream_normalizes_wire_formats_and_preserves_native_storage() {
        use crate::capture::EncodedPacket;
        let frame = EncodedPacket::from_static(b"\0\0\x01\x09\x10\0\0\x01\x65frame");
        let mut framed = b"NCH264F1".to_vec();
        framed.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        framed.extend_from_slice(&frame);
        let mut chunked = b"NCH264C1".to_vec();
        chunked.extend_from_slice(&3u32.to_be_bytes());
        chunked.extend_from_slice(&frame[..3]);
        chunked.extend_from_slice(&((1u32 << 31) | (frame.len() as u32 - 3)).to_be_bytes());
        chunked.extend_from_slice(&frame[3..]);
        let mut annex_b = frame.to_vec();
        annex_b.extend_from_slice(b"\0\0\x01\x09\x10");
        for wire in [framed, chunked, annex_b] {
            let mut packets =
                packet_stream(CaptureData::Bytes(Box::new(std::io::Cursor::new(wire))));
            assert_eq!(packets.try_next().await.unwrap().unwrap(), frame);
            assert!(packets.try_next().await.unwrap().is_none());
        }
        let expected = frame.clone();
        let mut packets = packet_stream(CaptureData::Packets(
            stream::once(async move { Ok(frame) }).boxed(),
        ));
        let actual = packets.try_next().await.unwrap().unwrap();
        assert_eq!(actual.as_ptr(), expected.as_ptr());
        assert_eq!(actual, expected);
        assert!(packets.try_next().await.unwrap().is_none());
    }

    fn captured(id: u8, keyframe: bool) -> EncodedFrame {
        let mut data = Vec::new();
        if keyframe {
            data.extend_from_slice(&[0, 0, 1, 0x67, 42, 0, 0, 0, 1, 0x68, 42]);
        }
        data.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }, id]);
        EncodedFrame {
            data: data.into(),
            captured_at: Instant::now(),
        }
    }
    #[test]
    fn recovery_requires_idr_and_both_parameter_sets_with_mixed_start_codes() {
        assert!(recovery_keyframe(&captured(1, true).data));
        assert!(!recovery_keyframe(&captured(2, false).data));
        assert!(!recovery_keyframe(&[0, 0, 1, 0x65, 42]));
        assert!(!recovery_keyframe(&[0, 0, 1, 0x67, 42, 0, 0, 1, 0x65, 42]));
        assert!(!recovery_keyframe(&[0, 0, 1, 0x68, 42, 0, 0, 1, 0x65, 42]));
    }
    #[tokio::test(start_paused = true)]
    async fn a_slow_viewer_cannot_hold_capture_or_another_viewer_and_recovers_at_keyframes() {
        let (frames, _) = broadcast::channel(2);
        let (fast_tx, mut fast_rx) = mpsc::channel(16);
        let (slow_tx, mut slow_rx) = mpsc::channel(16);
        let gate = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());
        let slow_gate = gate.clone();
        let slow_entered = entered.clone();
        let connected = Arc::new(AtomicBool::new(true));
        let slow = Task(tokio::spawn(forward_frames(
            frames.subscribe(),
            connected.clone(),
            Arc::new(|_| panic!("slow fixture retired")),
            move |sample| {
                let gate = slow_gate.clone();
                let entered = slow_entered.clone();
                let sent = slow_tx.clone();
                async move {
                    let id = *sample.data.last().unwrap();
                    if id == 1 {
                        entered.notify_one();
                        gate.notified().await;
                    }
                    sent.send(id).await.unwrap();
                    Ok(())
                }
            },
        )));
        let fast = Task(tokio::spawn(forward_frames(
            frames.subscribe(),
            connected,
            Arc::new(|_| panic!("healthy fixture retired")),
            move |sample| {
                let sent = fast_tx.clone();
                async move {
                    sent.send(*sample.data.last().unwrap()).await.unwrap();
                    Ok(())
                }
            },
        )));
        assert!(frames.send(captured(1, true)).is_ok());
        assert_eq!(fast_rx.recv().await, Some(1));
        entered.notified().await;
        for id in 2..=6 {
            assert!(frames.send(captured(id, false)).is_ok());
            assert_eq!(fast_rx.recv().await, Some(id));
        }
        gate.notify_one();
        assert_eq!(slow_rx.recv().await, Some(1));
        assert!(frames.send(captured(7, false)).is_ok());
        assert_eq!(fast_rx.recv().await, Some(7));
        assert!(frames.send(captured(8, true)).is_ok());
        assert_eq!(fast_rx.recv().await, Some(8));
        assert_eq!(
            slow_rx.recv().await,
            Some(8),
            "lagged viewer resumed from an undecodable delta frame"
        );
        drop((slow, fast));
    }
    #[tokio::test(start_paused = true)]
    async fn stalled_frame_retires_only_its_peer_and_initial_deltas_are_skipped() {
        let (frames, _) = broadcast::channel(2);
        let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failed = failures.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let written = calls.clone();
        let task = Task(tokio::spawn(forward_frames(
            frames.subscribe(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(move |_| {
                failed.fetch_add(1, Ordering::Relaxed);
            }),
            move |_| {
                written.fetch_add(1, Ordering::Relaxed);
                std::future::pending::<webrtc::error::Result<()>>()
            },
        )));
        assert!(frames.send(captured(1, false)).is_ok());
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(frames.send(captured(2, true)).is_ok());
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(failures.load(Ordering::Relaxed), 1);
        assert_eq!(frames.receiver_count(), 0);
        drop(task);
    }

    #[tokio::test]
    async fn dropping_peer_media_cancels_its_pending_writer() {
        let (frames, _) = broadcast::channel(2);
        let (entered, started) = tokio::sync::oneshot::channel();
        let (cancelled, observed) = tokio::sync::oneshot::channel::<()>();
        let mut owned = Some((entered, cancelled));
        let task = Task(tokio::spawn(forward_frames(
            frames.subscribe(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(|_| panic!("dropped peer must not report a failure")),
            move |_| {
                let (entered, cancelled) = owned.take().unwrap();
                async move {
                    let _cancelled_on_drop = cancelled;
                    let _ = entered.send(());
                    std::future::pending::<webrtc::error::Result<()>>().await
                }
            },
        )));
        assert!(frames.send(captured(1, true)).is_ok());
        started.await.unwrap();
        drop(task);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), observed)
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(frames.receiver_count(), 0);
    }

    #[tokio::test]
    async fn closing_a_removed_peer_does_not_fail_the_shared_video() {
        crate::tls::ensure_crypto_provider();
        let failed = Arc::new(AtomicBool::new(false));
        let (frames, _) = broadcast::channel(2);
        let builder = PeerBuilder {
            frames,
            audio: None,
            microphone_factory: None,
            motion: Arc::new(Motion::default()),
            failed: failed.clone(),
        };
        let (mut peer, _) = builder.build("fixture-viewer", Vec::new()).await.unwrap();
        // Removal drops this viewer's receiver before the asynchronously owned
        // peer finishes closing. Closed queues are expected during teardown;
        // they are not reliable-input overflow in another live viewer.
        peer.active.store(false, Ordering::Release);
        peer.incoming.take();
        tokio::time::timeout(Duration::from_secs(2), peer._connection.close())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !failed.load(Ordering::Acquire),
            "peer teardown poisoned shared video"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn encoder_diagnostics_cannot_corrupt_frame_boundaries() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let encoder = directory.path().join("encoder");
        std::fs::write(
            &encoder,
            r#"#!/usr/bin/env python3
import os, socket, sys
path = sys.argv[-1].split(']unix://', 1)[1].split('|', 1)[0]
os.write(2, b'objc: diagnostic outside FFmpeg logging\n')
with socket.socket(socket.AF_UNIX) as stream:
    stream.connect(path)
    stream.sendall(b'0, 0, 0, 1, 5, 0x0000\n')
    os.write(1, bytes([0, 0, 1, 0x65, 42]))
"#,
        )
        .unwrap();
        std::fs::set_permissions(&encoder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = std::process::Command::new(&encoder);
        command.args(["-f", "h264", "pipe:1"]);
        let capture = Capture::ffmpeg(command).unwrap();
        let mut packets = packet_stream(capture.data);
        let packet = tokio::time::timeout(Duration::from_secs(3), packets.try_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet.as_ref(), &[0, 0, 1, 0x65, 42]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn encoder_exit_before_metadata_does_not_leave_reader_waiting() {
        let mut command = std::process::Command::new("/usr/bin/false");
        command.args(["-f", "h264", "pipe:1"]);
        let capture = Capture::ffmpeg(command).unwrap();
        let mut packets = packet_stream(capture.data);
        let result = tokio::time::timeout(Duration::from_secs(3), packets.try_next())
            .await
            .unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("exited before metadata")
        );
    }

    #[test]
    fn annex_b_every_split_and_bounded() {
        let input = [
            0, 0, 0, 1, 9, 16, 0, 0, 1, 5, 42, 0, 0, 1, 9, 16, 0, 0, 0, 1, 1, 43, 0, 0, 0, 1, 9, 16,
        ];
        for split in 1..input.len() {
            let mut parser = AccessUnits::default();
            let mut units = parser.push(&input[..split]).unwrap();
            units.extend(parser.push(&input[split..]).unwrap());
            assert_eq!(
                units,
                vec![input[..11].to_vec(), input[11..22].to_vec()],
                "split {split}"
            );
        }
        assert!(AccessUnits::default().push(b"not h264").is_err());
        assert!(
            AccessUnits::default()
                .push(&vec![0; 8 * 1024 * 1024 + 1])
                .is_err()
        );
    }
    #[test]
    fn framed_packet_arrives_without_following_frame() {
        let frame = [0, 0, 0, 1, 9, 16, 0, 0, 1, 5, 42];
        let mut input = b"NCH264F1".to_vec();
        input.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        input.extend_from_slice(&frame);
        for split in 0..=input.len() {
            let mut parser = AccessUnits::default();
            let mut frames = parser.push(&input[..split]).unwrap();
            frames.extend(parser.push(&input[split..]).unwrap());
            assert_eq!(frames, vec![frame.to_vec()]);
        }
        let repeated = [input.clone(), input].concat();
        for split in 0..=repeated.len() {
            let mut parser = AccessUnits::default();
            let mut frames = parser.push(&repeated[..split]).unwrap();
            frames.extend(parser.push(&repeated[split..]).unwrap());
            assert_eq!(frames, vec![frame.to_vec(), frame.to_vec()]);
        }
        for size in [0u32, 8 * 1024 * 1024 + 1] {
            let mut input = b"NCH264F1".to_vec();
            input.extend_from_slice(&size.to_be_bytes());
            assert!(AccessUnits::default().push(&input).is_err());
        }
    }
    #[test]
    fn chunked_restart_discards_partial_frame_at_every_read_boundary() {
        let frame = [0, 0, 0, 1, 5, 42];
        let mut input = b"NCH264C1".to_vec();
        // Interrupted nonfinal chunk followed by a restarted encoder.
        input.extend_from_slice(&3u32.to_be_bytes());
        input.extend_from_slice(&[0, 0, 0]);
        input.extend_from_slice(b"NCH264C1");
        input.extend_from_slice(&3u32.to_be_bytes());
        input.extend_from_slice(&frame[..3]);
        input.extend_from_slice(&(0x80000000u32 | 3).to_be_bytes());
        input.extend_from_slice(&frame[3..]);
        // Switching from C1 to F1 is also legal at a record boundary.
        input.extend_from_slice(b"NCH264F1");
        input.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        input.extend_from_slice(&frame);
        for split in 0..=input.len() {
            let mut parser = AccessUnits::default();
            let mut frames = parser.push(&input[..split]).unwrap();
            frames.extend(parser.push(&input[split..]).unwrap());
            assert_eq!(
                frames,
                vec![frame.to_vec(), frame.to_vec()],
                "split {split}"
            );
        }
        for size in [0u32, 4093, 0x80000000] {
            assert!(
                AccessUnits::default()
                    .push(&[b"NCH264C1".as_slice(), &size.to_be_bytes()].concat())
                    .is_err()
            );
        }
    }
    #[test]
    fn candidate_matches_strict_broker_contract() {
        let signal = candidate_signal(RTCIceCandidateInit {
            candidate: "candidate:test".into(),
            username_fragment: Some("private-ice-generation".into()),
            ..Default::default()
        });
        assert_eq!(
            signal,
            json!({"type":"candidate","candidate":"candidate:test","sdpMid":"0","sdpMLineIndex":0})
        );
    }
    #[test]
    fn stun_without_turn_credentials() {
        let servers =
            ice_servers(&json!({"iceServers":[{"urls":"stun:example.com:3478"}]})).unwrap();
        assert_eq!(servers[0].urls, ["stun:example.com:3478"]);
        assert!(servers[0].username.is_empty());
    }
}
