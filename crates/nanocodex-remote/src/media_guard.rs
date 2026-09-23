//! Bound transport writes, including NACK retransmissions, inside one peer.
//! Each peer owns its frame sender, so this never holds up another viewer.
use async_trait::async_trait;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use webrtc::interceptor::{
    Attributes, Error, Interceptor, InterceptorBuilder, RTCPReader, RTCPWriter, RTPReader,
    RTPWriter, stream_info::StreamInfo,
};
use webrtc::rtp::packet::Packet;

type Fault = Arc<dyn Fn(&'static str) + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;
const WRITE_DEADLINE: Duration = Duration::from_secs(1);

pub(crate) struct Guard(pub(crate) Fault);
impl InterceptorBuilder for Guard {
    fn build(&self, _id: &str) -> Result<Arc<dyn Interceptor + Send + Sync>> {
        Ok(Arc::new(Self(self.0.clone())))
    }
}
#[async_trait]
impl Interceptor for Guard {
    async fn bind_local_stream(
        &self,
        info: &StreamInfo,
        writer: Arc<dyn RTPWriter + Send + Sync>,
    ) -> Arc<dyn RTPWriter + Send + Sync> {
        if !info
            .mime_type
            .split_once('/')
            .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case("video"))
        {
            return writer;
        }
        Arc::new(Writer {
            writer,
            fault: self.0.clone(),
            retired: AtomicBool::new(false),
        })
    }

    async fn bind_rtcp_reader(
        &self,
        reader: Arc<dyn RTCPReader + Send + Sync>,
    ) -> Arc<dyn RTCPReader + Send + Sync> {
        reader
    }
    async fn bind_rtcp_writer(
        &self,
        writer: Arc<dyn RTCPWriter + Send + Sync>,
    ) -> Arc<dyn RTCPWriter + Send + Sync> {
        writer
    }
    async fn unbind_local_stream(&self, _info: &StreamInfo) {}
    async fn bind_remote_stream(
        &self,
        _info: &StreamInfo,
        reader: Arc<dyn RTPReader + Send + Sync>,
    ) -> Arc<dyn RTPReader + Send + Sync> {
        reader
    }
    async fn unbind_remote_stream(&self, _info: &StreamInfo) {}
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
struct Writer {
    writer: Arc<dyn RTPWriter + Send + Sync>,
    fault: Fault,
    retired: AtomicBool,
}
#[async_trait]
impl RTPWriter for Writer {
    async fn write(&self, packet: &Packet, attributes: &Attributes) -> Result<usize> {
        if self.retired.load(Ordering::Acquire) {
            return Ok(0);
        }
        let outcome =
            match tokio::time::timeout(WRITE_DEADLINE, self.writer.write(packet, attributes)).await
            {
                Ok(Ok(bytes)) => return Ok(bytes),
                Ok(Err(_)) => "transport_error",
                Err(_) => "transport_timeout",
            };
        if !self.retired.swap(true, Ordering::AcqRel) {
            (self.fault)(outcome);
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    struct Sink(u8, AtomicUsize);
    #[async_trait]
    impl RTPWriter for Sink {
        async fn write(&self, packet: &Packet, _attributes: &Attributes) -> Result<usize> {
            self.1.fetch_add(1, Ordering::Relaxed);
            match self.0 {
                1 => std::future::pending().await,
                2 => Err(Error::Other("fixture transport failure".into())),
                3 => Ok(0),
                _ => {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    Ok(packet.payload.len())
                }
            }
        }
    }
    #[tokio::test(start_paused = true)]
    async fn stalled_transport_retires_once_without_rejecting_a_normal_turn_pause() {
        for mode in [0, 1, 2, 3] {
            let failures = Arc::new(AtomicUsize::new(0));
            let notify = failures.clone();
            let sink = Arc::new(Sink(mode, AtomicUsize::new(0)));
            let writer = Guard(Arc::new(move |_| {
                notify.fetch_add(1, Ordering::Relaxed);
            }))
            .bind_local_stream(
                &StreamInfo {
                    mime_type: "video/H264".into(),
                    ..Default::default()
                },
                sink.clone(),
            )
            .await;
            let packet = Packet {
                payload: vec![0x65, 42].into(),
                ..Default::default()
            };
            for _ in 0..2 {
                assert_eq!(
                    writer.write(&packet, &Attributes::new()).await.unwrap(),
                    if mode == 0 { 2 } else { 0 }
                );
            }
            let retired = matches!(mode, 1 | 2);
            assert_eq!(failures.load(Ordering::Relaxed), usize::from(retired));
            assert_eq!(sink.1.load(Ordering::Relaxed), if retired { 1 } else { 2 });
        }
    }
}
