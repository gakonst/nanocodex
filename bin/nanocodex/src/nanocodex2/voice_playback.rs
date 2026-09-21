//! Bounded synthesis into the same native output engine as ChatGPT voice.
use super::{elevenlabs::Client, error};
use nanocodex_managed::ManagedError;
use nanocodex_voice_native::RealtimeWebrtcSessionHandle;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

type Media = Arc<dyn Fn() -> Result<RealtimeWebrtcSessionHandle, ManagedError> + Send + Sync>;
static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);
const PCM_RATE: u32 = 24_000;
const BLOCK_BYTES: usize = 960; // 20ms mono PCM16, bounded across the native pipe.

pub(super) struct Playback {
    queue: mpsc::Sender<(CancellationToken, String)>,
    generation: std::sync::Mutex<CancellationToken>,
    task: tokio::task::JoinHandle<()>,
}
impl Playback {
    pub(super) fn new(
        client: Client,
        voice: String,
        status: watch::Sender<super::Status>,
        media: Media,
    ) -> Self {
        let (queue, mut pending) = mpsc::channel::<(CancellationToken, String)>(4);
        let task = tokio::spawn(async move {
            while let Some((cancel, text)) = pending.recv().await {
                if cancel.is_cancelled() {
                    continue;
                }
                let result = tokio::select! {
                    biased;
                    () = cancel.cancelled() => continue,
                    result = async {
                        let handle = media()?;
                        play(&client, &voice, &text, handle).await
                    } => result,
                };
                if let Err(error) = result {
                    status.send_modify(|s| s.text = format!("ElevenLabs playback failed: {error}"));
                }
            }
        });
        Self {
            queue,
            generation: std::sync::Mutex::new(CancellationToken::new()),
            task,
        }
    }
    pub(super) fn cancel(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        generation.cancel();
        *generation = CancellationToken::new();
    }
    pub(super) fn enqueue(&self, text: String) -> Result<(), ManagedError> {
        let generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.queue
            .try_send((generation, text))
            .map_err(|_| error("ElevenLabs speech queue full; reply remains available as text"))
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.cancel();
        self.task.abort();
    }
}

// Created before begin is awaited: cancellation during native admission must
// invalidate that admission too. The helper ignores cancellation of old IDs.
struct NativeStream {
    media: RealtimeWebrtcSessionHandle,
    id: u64,
}
impl Drop for NativeStream {
    fn drop(&mut self) {
        let media = self.media.clone();
        let id = self.id;
        tokio::spawn(async move {
            let _ = media.cancel_pcm(id).await;
        });
    }
}

async fn play(
    client: &Client,
    voice: &str,
    text: &str,
    media: RealtimeWebrtcSessionHandle,
) -> Result<(), ManagedError> {
    let response = client.speech_stream(voice, text).await?;
    let stream = NativeStream {
        media,
        id: NEXT_STREAM.fetch_add(1, Ordering::Relaxed),
    };
    stream
        .media
        .begin_pcm_stream(stream.id, PCM_RATE)
        .await
        .map_err(|e| error(e.to_string()))?;
    forward_audio(response, |samples| {
        let media = &stream.media;
        let id = stream.id;
        async move {
            media
                .write_pcm(id, samples)
                .await
                .map_err(|e| error(e.to_string()))
        }
    })
    .await?;
    stream
        .media
        .drain_pcm(stream.id)
        .await
        .map_err(|e| error(e.to_string()))?;
    Ok(())
}

async fn forward_audio<F, Fut>(
    mut response: reqwest::Response,
    mut write: F,
) -> Result<(), ManagedError>
where
    F: FnMut(Vec<i16>) -> Fut,
    Fut: std::future::Future<Output = Result<(), ManagedError>>,
{
    let mut pcm = PcmFrames::default();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error("ElevenLabs audio stream failed"))?
    {
        pcm.account(chunk.len())?;
        for byte in chunk {
            pcm.pending.push(byte);
            if pcm.pending.len() == BLOCK_BYTES {
                write(pcm.take()).await?;
            }
        }
    }
    pcm.validate_end()?;
    if !pcm.pending.is_empty() {
        write(pcm.take()).await?;
    }
    Ok(())
}

#[derive(Default)]
struct PcmFrames {
    pending: Vec<u8>,
    total: usize,
}
impl PcmFrames {
    fn account(&mut self, count: usize) -> Result<(), ManagedError> {
        self.total = self.total.saturating_add(count);
        if self.total > 16 * 1024 * 1024 {
            return Err(error("ElevenLabs audio exceeded 16 MiB"));
        }
        Ok(())
    }
    fn take(&mut self) -> Vec<i16> {
        let samples = self
            .pending
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        self.pending.clear();
        samples
    }
    fn validate_end(&self) -> Result<(), ManagedError> {
        if self.total == 0 {
            return Err(error("ElevenLabs returned empty audio"));
        }
        if !self.total.is_multiple_of(2) {
            return Err(error("ElevenLabs returned incomplete PCM audio"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_preserves_split_samples_and_rejects_bad_streams() {
        let mut pcm = PcmFrames::default();
        assert!(pcm.validate_end().is_err());
        for bytes in [&[0xff][..], &[0x7f, 0, 0x80][..]] {
            pcm.account(bytes.len()).unwrap();
            pcm.pending.extend_from_slice(bytes);
        }
        pcm.validate_end().unwrap();
        assert_eq!(pcm.take(), vec![i16::MAX, i16::MIN]);
        pcm.account(1).unwrap();
        assert!(pcm.validate_end().is_err());
        assert!(pcm.account(16 * 1024 * 1024).is_err());
    }
    #[tokio::test]
    async fn streams_before_eof_and_cancellation_closes_http() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3c0\r\n").await.unwrap();
            socket.write_all(&[1u8; BLOCK_BYTES]).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
            socket.flush().await.unwrap();
            let _ = closed_tx.send(matches!(socket.read(&mut byte).await, Ok(0) | Err(_)));
        });
        let (written, mut observed) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            let response = reqwest::get(url).await.unwrap();
            forward_audio(response, move |samples| {
                let written = written.clone();
                async move {
                    written.send(samples).await.unwrap();
                    Ok(())
                }
            })
            .await
        });
        let samples = tokio::time::timeout(std::time::Duration::from_secs(3), observed.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(samples, vec![257; BLOCK_BYTES / 2]);
        assert!(!task.is_finished(), "response deliberately has no EOF");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), closed_rx)
                .await
                .unwrap()
                .unwrap()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn interruption_invalidates_every_queued_item_and_allows_fresh_speech() {
        let (queue, mut receiver) = mpsc::channel(4);
        let playback = Playback {
            queue,
            generation: std::sync::Mutex::new(CancellationToken::new()),
            task: tokio::spawn(std::future::pending()),
        };
        for _ in 0..4 {
            playback.enqueue("old".into()).unwrap();
        }
        assert!(playback.enqueue("overflow".into()).is_err());
        playback.cancel();
        for _ in 0..4 {
            assert!(receiver.recv().await.unwrap().0.is_cancelled());
        }
        playback.enqueue("new".into()).unwrap();
        let (token, text) = receiver.recv().await.unwrap();
        assert!(!token.is_cancelled());
        assert_eq!(text, "new");
        drop(playback);
        assert!(token.is_cancelled());
    }
}
