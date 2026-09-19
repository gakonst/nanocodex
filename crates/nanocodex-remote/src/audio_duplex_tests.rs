use super::*;
use opusic_c::{Application, Encoder};
use std::sync::atomic::AtomicUsize;

struct RecordingSink {
    writes: mpsc::UnboundedSender<Vec<u8>>,
    closed: Arc<AtomicUsize>,
}
impl Drop for RecordingSink {
    fn drop(&mut self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl AudioSink for RecordingSink {
    async fn write(&mut self, pcm: &[u8]) -> Result<()> {
        self.writes.send(pcm.into())?;
        Ok(())
    }
}
fn tone() -> Vec<u8> {
    let mut encoder = Encoder::new(Channels::Mono, SampleRate::Hz48000, Application::Voip).unwrap();
    let pcm: Vec<f32> = (0..960)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.25)
        .collect();
    let mut encoded = [0; 4000];
    let size = encoder.encode_float_to_slice(&pcm, &mut encoded).unwrap();
    encoded[..size].into()
}
struct Fixture {
    mic: Microphone,
    packets: mpsc::Sender<(Instant, Vec<u8>)>,
    writes: mpsc::UnboundedReceiver<Vec<u8>>,
    opened: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    receiver: tokio::task::JoinHandle<Result<()>>,
}
impl Fixture {
    fn new(delay: Duration) -> Self {
        let (permission, updates) = watch::channel(Permission::default());
        let (packets, incoming) = mpsc::channel(3);
        let (writes, output) = mpsc::unbounded_channel();
        let opened = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let started = opened.clone();
        let stopped = closed.clone();
        let factory: SinkFactory = Arc::new(move || {
            let sink = RecordingSink {
                writes: writes.clone(),
                closed: stopped.clone(),
            };
            started.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(Box::new(sink) as Box<dyn AudioSink>)
            })
        });
        let mic = Microphone {
            permission,
            available: true,
            tasks: Arc::new(Mutex::new(Vec::new())),
        };
        let receiver = tokio::spawn(receive(incoming, updates, factory));
        Self {
            mic,
            packets,
            writes: output,
            opened,
            closed,
            receiver,
        }
    }
    async fn packet(&self) {
        self.packets.send((Instant::now(), tone())).await.unwrap();
    }
    async fn written(&mut self) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(2), self.writes.recv())
            .await
            .unwrap()
            .unwrap()
    }
}
async fn count(value: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while value.load(Ordering::SeqCst) != expected {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn opt_in_decodes_opus_and_mute_releases_without_replaying_old_packets() {
    let mut f = Fixture::new(Duration::ZERO);
    f.packet().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        f.opened.load(Ordering::SeqCst),
        0,
        "default-muted never creates a sink"
    );
    let old = Instant::now();
    assert!(f.mic.set_enabled(true, Duration::from_secs(2)));
    f.packet().await;
    let pcm = f.written().await;
    assert_eq!(pcm.len(), 960 * 2);
    assert!(
        pcm.chunks_exact(2)
            .any(|p| i16::from_le_bytes([p[0], p[1]]).unsigned_abs() > 100)
    );
    f.mic.revoke();
    count(&f.closed, 1).await;
    assert!(f.mic.set_enabled(true, Duration::from_secs(2)));
    f.packets.send((old, tone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        f.opened.load(Ordering::SeqCst),
        1,
        "queued speech from before opt-in is discarded"
    );
    f.packet().await;
    f.written().await;
    drop(f.mic);
    f.receiver.await.unwrap().unwrap();
    assert_eq!(f.closed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn lease_expiry_releases_sink_and_renew_cannot_resurrect_it() {
    let mut f = Fixture::new(Duration::ZERO);
    f.mic.set_enabled(true, Duration::from_millis(80));
    f.packet().await;
    f.written().await;
    count(&f.closed, 1).await;
    f.mic.renew(Duration::from_secs(2));
    f.packet().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(f.opened.load(Ordering::SeqCst), 1);
    drop(f.mic);
    f.receiver.await.unwrap().unwrap();
}

#[tokio::test]
async fn revoke_cancels_pending_device_creation_before_any_pcm_write() {
    let f = Fixture::new(Duration::from_secs(10));
    f.mic.set_enabled(true, Duration::from_secs(2));
    f.packet().await;
    count(&f.opened, 1).await;
    f.mic.revoke();
    count(&f.closed, 1).await;
    assert!(f.writes.is_empty());
    drop(f.mic);
    f.receiver.await.unwrap().unwrap();
}

#[tokio::test]
async fn stale_audio_and_absent_or_zero_duration_permission_never_open_device() {
    let f = Fixture::new(Duration::ZERO);
    assert!(!f.mic.set_enabled(true, Duration::ZERO));
    f.packet().await;
    f.mic.set_enabled(true, Duration::from_secs(1));
    f.packets
        .send((Instant::now() - Duration::from_secs(1), tone()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(f.opened.load(Ordering::SeqCst), 0);
    drop(f.mic);
    f.receiver.await.unwrap().unwrap();
}

#[tokio::test]
async fn malformed_opus_stops_receiver_and_releases_sink() {
    let f = Fixture::new(Duration::ZERO);
    f.mic.set_enabled(true, Duration::from_secs(1));
    f.packets
        .send((Instant::now(), vec![3, 255]))
        .await
        .unwrap();
    assert!(f.receiver.await.unwrap().is_err());
    assert!(f.writes.is_empty());
    assert_eq!(f.closed.load(Ordering::SeqCst), 1);
}
