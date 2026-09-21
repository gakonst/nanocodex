//! Opt-in viewer microphone. The owner must validate the current input lease before
//! enabling or renewing it. No device is opened before authorized audio arrives.
use opusic_c::{Channels, Decoder, SampleRate};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use webrtc::{peer_connection::RTCPeerConnection, rtp_transceiver::rtp_codec::RTPCodecType};

#[path = "audio_sink.rs"]
mod sink;
pub use sink::native_factory;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub type SinkFactory =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Box<dyn AudioSink>>> + Send>> + Send + Sync>;
#[async_trait::async_trait]
pub trait AudioSink: Send {
    async fn write(&mut self, pcm: &[u8]) -> Result<()>;
}
#[derive(Clone, Copy, Default)]
struct Permission {
    deadline: Option<Instant>,
    epoch: u64,
    since: Option<Instant>,
}
impl Permission {
    fn active(self) -> bool {
        self.deadline.is_some_and(|d| d > Instant::now())
    }
}
struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct Microphone {
    permission: watch::Sender<Permission>,
    available: bool,
    failed: Arc<AtomicBool>,
    tasks: Arc<Mutex<Vec<Task>>>,
}
impl Microphone {
    pub fn install(connection: &Arc<RTCPeerConnection>, factory: Option<SinkFactory>) -> Self {
        let (permission, updates) = watch::channel(Permission::default());
        let tasks = Arc::new(Mutex::new(Vec::new()));
        let owned = tasks.clone();
        let available = factory.is_some();
        let claimed = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let receiver_failed = failed.clone();
        connection.on_track(Box::new(move |track, _, _| {
            let factory = factory.clone();
            let updates = updates.clone();
            let owned = owned.clone();
            let claimed = claimed.clone();
            let failed = receiver_failed.clone();
            Box::pin(async move {
                let codec = track.codec();
                if track.kind() != RTPCodecType::Audio
                    || !codec
                        .capability
                        .mime_type
                        .eq_ignore_ascii_case("audio/opus")
                    || codec.capability.clock_rate != 48000
                    || !matches!(codec.capability.channels, 1 | 2)
                {
                    return;
                }
                let Some(factory) = factory else {
                    return;
                };
                if failed.load(Ordering::Acquire) || claimed.swap(true, Ordering::AcqRel) {
                    return;
                }
                let (packets, incoming) = mpsc::channel(3);
                let reader = Task(tokio::spawn(async move {
                    let mut sequence = None;
                    loop {
                        let (packet, _) = match track.read_rtp().await {
                            Ok(packet) => packet,
                            Err(error) => {
                                tracing::warn!(%error, "remote microphone RTP reader stopped");
                                break;
                            }
                        };
                        // Never replay reordered/duplicate microphone samples. A full queue
                        // drops audio instead of accumulating delayed speech.
                        let seq = packet.header.sequence_number;
                        if sequence.is_some_and(|last: u16| {
                            seq.wrapping_sub(last) == 0 || seq.wrapping_sub(last) >= 32768
                        }) {
                            continue;
                        }
                        sequence = Some(seq);
                        if packet.payload.len() <= 4000 {
                            let _ = packets.try_send((Instant::now(), packet.payload.to_vec()));
                        }
                    }
                }));
                owned
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(Task(tokio::spawn(async move {
                        let _reader = reader;
                        struct Release(Arc<AtomicBool>);
                        impl Drop for Release {
                            fn drop(&mut self) {
                                self.0.store(false, Ordering::Release);
                            }
                        }
                        let _release = Release(claimed);
                        if let Err(error) = receive_report(incoming, updates, factory, failed).await
                        {
                            tracing::warn!(%error, "remote microphone stopped");
                        }
                    })));
            })
        }));
        Self {
            permission,
            available,
            failed,
            tasks,
        }
    }
    pub fn available(&self) -> bool {
        self.available
    }
    /// False after expiry, revoke, transport EOF or any decoder/sink failure.
    /// A failed receiver requires a fresh peer; opt-in cannot resurrect it.
    pub fn enabled(&self) -> bool {
        !self.failed.load(Ordering::Acquire) && self.permission.borrow().active()
    }
    pub fn set_enabled(&self, enabled: bool, lease_remaining: Duration) -> bool {
        let enabled = enabled
            && self.available
            && !self.failed.load(Ordering::Acquire)
            && !lease_remaining.is_zero();
        self.permission.send_modify(|p| {
            p.epoch = p.epoch.wrapping_add(1);
            p.since = enabled.then(Instant::now);
            p.deadline = p
                .since
                .map(|now| now + lease_remaining.min(Duration::from_secs(10)));
        });
        enabled
    }
    pub fn renew(&self, lease_remaining: Duration) {
        self.permission.send_modify(|p| {
            if !self.failed.load(Ordering::Acquire) && p.active() {
                p.deadline = Some(Instant::now() + lease_remaining.min(Duration::from_secs(10)));
            }
        });
    }
    pub fn revoke(&self) {
        self.set_enabled(false, Duration::ZERO);
    }
}
impl Drop for Microphone {
    fn drop(&mut self) {
        self.revoke();
        self.tasks.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

async fn receive_report(
    packets: mpsc::Receiver<(Instant, Vec<u8>)>,
    permission: watch::Receiver<Permission>,
    factory: SinkFactory,
    failed: Arc<AtomicBool>,
) -> Result<()> {
    let result = receive(packets, permission, factory).await;
    failed.store(true, Ordering::Release);
    result
}

// Renewal extends authorization without dropping partially completed sink work.
async fn while_authorized<F: Future>(
    permission: &mut watch::Receiver<Permission>,
    epoch: u64,
    work: F,
) -> Option<F::Output> {
    tokio::pin!(work);
    loop {
        let current = *permission.borrow_and_update();
        if current.epoch != epoch || !current.active() {
            return None;
        }
        tokio::select! {
            biased;
            changed = permission.changed() => {
                if changed.is_err() { return None; }
            }
            _ = tokio::time::sleep_until(current.deadline.unwrap().into()) => {}
            result = &mut work => { return Some(result); }
        }
    }
}

async fn receive(
    mut packets: mpsc::Receiver<(Instant, Vec<u8>)>,
    mut permission: watch::Receiver<Permission>,
    factory: SinkFactory,
) -> Result<()> {
    let mut sink: Option<Box<dyn AudioSink>> = None;
    let mut decoder = None;
    let mut epoch = 0;
    let mut pcm = [0u16; 5760]; // Opus permits at most 120 ms at 48 kHz; decode mono.
    loop {
        let current = *permission.borrow_and_update();
        if current.epoch != epoch || !current.active() {
            sink = None;
            decoder = None;
            epoch = current.epoch;
        }
        let expiry = current
            .deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(60));
        tokio::select! {
            biased;
            changed = permission.changed() => { if changed.is_err() { return Ok(()); } }
            _ = tokio::time::sleep_until(expiry.into()), if current.active() => { sink = None; decoder = None; }
            packet = packets.recv() => {
                let Some((arrived, packet)) = packet else { return Ok(()); };
                if !current.active() || current.since.is_none_or(|since| arrived < since) || arrived.elapsed() > Duration::from_millis(100) { continue; }
                if sink.is_none() {
                    let Some(result) = while_authorized(
                        &mut permission,
                        current.epoch,
                        tokio::time::timeout(Duration::from_secs(5), factory()),
                    ).await else { continue; };
                    sink = Some(result??);
                    decoder = Some(Decoder::new(Channels::Mono, SampleRate::Hz48000).map_err(|e| std::io::Error::other(e.message()))?);
                }
                // A revoke/close while device creation was pending must never write.
                let latest = *permission.borrow();
                if permission.has_changed().is_err() || latest.epoch != current.epoch || !latest.active() { sink = None; decoder = None; continue; }
                let count = decoder.as_mut().unwrap().decode_to_slice(&packet, &mut pcm, false).map_err(|e| std::io::Error::other(e.message()))?;
                let bytes: Vec<u8> = pcm[..count].iter().flat_map(|s| s.to_le_bytes()).collect();
                match while_authorized(
                    &mut permission,
                    current.epoch,
                    tokio::time::timeout(Duration::from_millis(100), sink.as_mut().unwrap().write(&bytes)),
                ).await {
                    Some(result) => { result??; }
                    None => { sink = None; decoder = None; }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "audio_duplex_tests.rs"]
mod tests;
