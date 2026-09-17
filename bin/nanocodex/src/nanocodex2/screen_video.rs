//! Continuous 60 Hz H.264 capture, independent of agent screenshots and input.
//! Encoders emit Annex B with access-unit delimiters. Only signaling crosses the
//! account broker; media and leased input use authenticated WebRTC peers.
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::mpsc,
    task::JoinHandle,
};
use webrtc::{
    api::{
        APIBuilder, interceptor_registry::register_default_interceptors, media_engine::MediaEngine,
    },
    data_channel::{RTCDataChannel, data_channel_init::RTCDataChannelInit},
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer},
    interceptor::registry::Registry,
    media::Sample,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
    rtp_transceiver::rtp_codec::RTCRtpCodecCapability,
    track::track_local::track_local_static_sample::TrackLocalStaticSample,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) type VideoSource = Arc<dyn Fn() -> BoxFuture<'static, Result<Capture>> + Send + Sync>;
pub(crate) struct Capture {
    pub reader: Box<dyn AsyncRead + Unpin + Send>,
    pub owner: Task,
}
pub(crate) struct Task(pub JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Capture {
    pub(crate) fn child(mut child: tokio::process::Child) -> Result<Self> {
        let reader = child.stdout.take().ok_or("encoder stdout unavailable")?;
        Ok(Self {
            reader: Box::new(reader),
            owner: Task(tokio::spawn(async move {
                let _ = child.wait().await;
            })),
        })
    }
}

/// Bounded incremental Annex-B parser. Never decode/re-encode agent JPEGs.
#[derive(Default)]
struct AccessUnits {
    bytes: Vec<u8>,
    scanned: usize,
}
impl AccessUnits {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.bytes.extend_from_slice(bytes);
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

pub(crate) struct Event {
    pub value: Value,
    pub outgoing: bool,
    pub created: Instant,
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
    connection: Connection,
    control: Arc<RTCDataChannel>,
    _rtcp: Vec<Task>,
    candidates: Vec<RTCIceCandidateInit>,
    answered: bool,
    started: Instant,
    refresh_ice: bool,
}
#[derive(Default)]
struct Motion {
    latest: std::sync::Mutex<HashMap<String, Event>>,
    changed: tokio::sync::Notify,
}
pub(crate) struct Video {
    track: Arc<TrackLocalStaticSample>,
    audio: Option<super::screen_audio::Audio>,
    peers: HashMap<String, Peer>,
    events: mpsc::Sender<Event>,
    incoming: mpsc::Receiver<Event>,
    motion: Arc<Motion>,
    failed: Arc<AtomicBool>,
    _capture: Task,
}
impl Video {
    pub(crate) async fn start(
        source: &VideoSource,
        audio_source: Option<&VideoSource>,
    ) -> Result<Self> {
        let mut capture = tokio::time::timeout(Duration::from_secs(8), source()).await??;
        let track = Arc::new(TrackLocalStaticSample::new(
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
        let (events, incoming) = mpsc::channel(128);
        let failed = Arc::new(AtomicBool::new(false));
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let writer = track.clone();
        let failure = failed.clone();
        let task = Task(tokio::spawn(async move {
            let _owner = capture.owner;
            let mut ready = Some(ready);
            let mut parser = AccessUnits::default();
            let mut buffer = [0; 64 * 1024];
            let mut previous = Instant::now();
            let result: Result<()> = async {
                loop {
                    let count = tokio::time::timeout(
                        Duration::from_secs(5),
                        capture.reader.read(&mut buffer),
                    )
                    .await??;
                    if count == 0 {
                        return Err("encoder stopped".into());
                    }
                    for data in parser.push(&buffer[..count])? {
                        let now = Instant::now();
                        let duration = now.duration_since(previous).max(Duration::from_micros(1));
                        previous = now;
                        tokio::time::timeout(
                            Duration::from_millis(250),
                            writer.write_sample(&Sample {
                                data: data.into(),
                                duration,
                                ..Default::default()
                            }),
                        )
                        .await??;
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(());
                        }
                    }
                }
            }
            .await;
            if result.is_err() {
                failure.store(true, Ordering::Release);
            }
        }));
        tokio::time::timeout(Duration::from_secs(10), waiting).await??;
        let audio = if let Some(source) = audio_source {
            match super::screen_audio::Audio::start(source).await {
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
            track,
            audio,
            peers: HashMap::new(),
            events,
            incoming,
            motion: Arc::new(Motion::default()),
            failed,
            _capture: task,
        })
    }
    pub(crate) async fn next(&mut self) -> Option<Event> {
        loop {
            tokio::select! {
                biased;
                event = self.incoming.recv() => return event,
                _ = self.motion.changed.notified() => {
                    let mut latest = self.motion.latest.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(id) = latest.keys().next().cloned() {
                        let event = latest.remove(&id);
                        if !latest.is_empty() { self.motion.changed.notify_one(); }
                        return event;
                    }
                }
            }
        }
    }
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub(crate) fn expired(&self) -> Vec<String> {
        self.peers
            .iter()
            .filter(|(_, p)| {
                (!p.answered && p.started.elapsed() > Duration::from_secs(25))
                    || (p.refresh_ice && p.started.elapsed() > Duration::from_secs(20 * 60))
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
    pub(crate) fn remove(&mut self, id: &str) {
        self.peers.remove(id);
        self.motion
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
    pub(crate) async fn control(&self, id: &str, value: &Value) -> Result<()> {
        if let Some(peer) = self.peers.get(id) {
            tokio::time::timeout(
                Duration::from_secs(1),
                peer.control.send_text(value.to_string()),
            )
            .await??;
        }
        Ok(())
    }
    pub(crate) async fn add(&mut self, id: &str, servers: Vec<RTCIceServer>) -> Result<Value> {
        if self.peers.contains_key(id) || self.peers.len() >= 4 {
            return Err("viewer capacity or duplicate".into());
        }
        let mut engine = MediaEngine::default();
        engine.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut engine)?;
        let mut settings = webrtc::api::setting_engine::SettingEngine::default();
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
        let sender = connection.add_track(self.track.clone()).await?;
        let rtcp = Task(tokio::spawn(async move {
            while sender.read_rtcp().await.is_ok() {}
        }));
        let mut rtcp = vec![rtcp];
        if let Some(audio) = &self.audio {
            let sender = connection.add_track(audio.track.clone()).await?;
            rtcp.push(Task(tokio::spawn(async move {
                while sender.read_rtcp().await.is_ok() {}
            })));
        }
        let control = connection
            .create_data_channel("remote-control-v1", None)
            .await?;
        // Install the owned guard before any subsequent fallible operation.
        self.peers.insert(
            id.into(),
            Peer {
                connection: owned,
                control: control.clone(),
                _rtcp: rtcp,
                candidates: Vec::new(),
                answered: false,
                started: Instant::now(),
                refresh_ice,
            },
        );
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
        for (channel, motion) in [(control, false), (motion, true)] {
            let events = self.events.clone();
            let failed = self.failed.clone();
            let id = id.to_owned();
            let latest = self.motion.clone();
            let closed = self.events.clone();
            let failure = self.failed.clone();
            let viewer = id.clone();
            channel.on_close(Box::new(move || {
                if closed
                    .try_send(Event {
                        value: json!({"type":"viewer_left","viewer_id":viewer}),
                        outgoing: false,
                        created: Instant::now(),
                    })
                    .is_err()
                {
                    failure.store(true, Ordering::Release);
                }
                Box::pin(async {})
            }));
            channel.on_message(Box::new(move |message| {
                let value = if message.is_string && message.data.len() <= 8192 {
                    serde_json::from_slice::<Value>(&message.data).ok().filter(|v| v.is_object() && (!motion || v["kind"] == "move"))
                } else { None };
                let value = match value {
                    Some(data) => json!({"type":if data.get("kind").is_some(){"input"}else{"control"},"viewer_id":id,"data":data}),
                    None => json!({"type":"viewer_left","viewer_id":id}),
                };
                if motion && value["type"] == "input" {
                    latest.latest.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), Event { value, outgoing: false, created: Instant::now() });
                    latest.changed.notify_one();
                    return Box::pin(async {});
                }
                // Motion is disposable. Reliable queue overflow fails closed:
                // never retain a key-down after losing its matching key-up.
                if events.try_send(Event { value, outgoing: false, created: Instant::now() }).is_err() && !motion { failed.store(true, Ordering::Release); }
                Box::pin(async {})
            }));
        }
        let events = self.events.clone();
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
                    })
                    .is_err()
                {
                    failed.store(true, Ordering::Release);
                }
            }
            Box::pin(async {})
        }));
        let events = self.events.clone();
        let failed = self.failed.clone();
        let viewer = id.to_owned();
        connection.on_peer_connection_state_change(Box::new(move |state| {
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
                })
                .is_err()
            {
                failed.store(true, Ordering::Release);
            }
            Box::pin(async {})
        }));
        let events = self.events.clone();
        let failed = self.failed.clone();
        let viewer = id.to_owned();
        connection.on_data_channel(Box::new(move |channel| {
            if events
                .try_send(Event {
                    value: json!({"type":"viewer_left","viewer_id":viewer}),
                    outgoing: false,
                    created: Instant::now(),
                })
                .is_err()
            {
                failed.store(true, Ordering::Release);
            }
            Box::pin(async move {
                let _ = channel.close().await;
            })
        }));
        let offer = connection.create_offer(None).await?;
        connection.set_local_description(offer.clone()).await?;
        Ok(json!({"type":"signal","viewer_id":id,"signal":{"type":"offer","sdp":offer.sdp}}))
    }
    pub(crate) async fn signal(&mut self, id: &str, signal: &Value) -> Result<()> {
        let Some(peer) = self.peers.get_mut(id) else {
            return Ok(());
        };
        match signal["type"].as_str() {
            Some("answer") if !peer.answered => {
                let sdp = signal["sdp"]
                    .as_str()
                    .filter(|s| s.len() <= 65536)
                    .ok_or("invalid SDP")?;
                peer.connection
                    .set_remote_description(RTCSessionDescription::answer(sdp.into())?)
                    .await?;
                peer.answered = true;
                for candidate in peer.candidates.drain(..) {
                    peer.connection.add_ice_candidate(candidate).await?;
                }
            }
            Some("candidate") => {
                let candidate: RTCIceCandidateInit = serde_json::from_value(signal.clone())?;
                if candidate.candidate.len() > 4096 {
                    return Err("candidate too large".into());
                }
                if peer.answered {
                    peer.connection.add_ice_candidate(candidate).await?;
                } else if peer.candidates.len() < 128 {
                    peer.candidates.push(candidate);
                } else {
                    return Err("too many candidates".into());
                }
            }
            _ => return Err("invalid signaling".into()),
        }
        Ok(())
    }
}

fn candidate_signal(candidate: RTCIceCandidateInit) -> Value {
    // The broker's strict schema excludes usernameFragment. This host offers
    // one bundled video m-line (mid 0) followed by its data channels.
    json!({"type":"candidate", "candidate":candidate.candidate, "sdpMid":"0", "sdpMLineIndex":0})
}

pub(crate) fn ice_servers(value: &Value) -> Result<Vec<RTCIceServer>> {
    value["iceServers"]
        .as_array()
        .filter(|s| s.len() <= 16)
        .ok_or_else(|| io::Error::other("invalid ICE servers"))?
        .iter()
        .map(|s| {
            let urls = match &s["urls"] {
                Value::String(s) => vec![s.clone()],
                Value::Array(urls) => urls
                    .iter()
                    .map(|u| u.as_str().map(str::to_owned).ok_or("invalid ICE URL"))
                    .collect::<std::result::Result<Vec<_>, _>>()?,
                _ => return Err("invalid ICE URLs".into()),
            };
            Ok(RTCIceServer {
                urls,
                username: s["username"].as_str().unwrap_or("").into(),
                credential: s["credential"].as_str().unwrap_or("").into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
