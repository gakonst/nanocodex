//! Negotiated screen playout hint, matching the Go Hand's 0–100ms range.
//! Keep this per stream: a shared track can have different extension IDs for
//! each viewer, including viewers that do not negotiate the extension at all.
use async_trait::async_trait;
use std::sync::Arc;
use webrtc::{
    api::media_engine::MediaEngine,
    interceptor::{
        Attributes, Error, Interceptor, InterceptorBuilder, RTCPReader, RTCPWriter, RTPReader,
        RTPWriter, registry::Registry, stream_info::StreamInfo,
    },
    rtp::{
        header::{EXTENSION_PROFILE_ONE_BYTE, EXTENSION_PROFILE_TWO_BYTE, Extension},
        packet::Packet,
    },
    rtp_transceiver::rtp_codec::{RTCRtpHeaderExtensionCapability, RTPCodecType},
};

const URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay";
type InterceptorResult<T> = std::result::Result<T, Error>;

pub(crate) fn register(
    engine: &mut MediaEngine,
    mut registry: Registry,
) -> crate::Result<Registry> {
    engine.register_header_extension(
        RTCRtpHeaderExtensionCapability { uri: URI.into() },
        RTPCodecType::Video,
        None,
    )?;
    // Extend the caller's default NACK, report, and transport feedback chain.
    registry.add(Box::new(PlayoutDelay));
    Ok(registry)
}

struct PlayoutDelay;
impl InterceptorBuilder for PlayoutDelay {
    fn build(&self, _id: &str) -> InterceptorResult<Arc<dyn Interceptor + Send + Sync>> {
        Ok(Arc::new(Self))
    }
}

#[async_trait]
impl Interceptor for PlayoutDelay {
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
        match info
            .rtp_header_extensions
            .iter()
            .find(|ext| ext.uri == URI && (1..=255).contains(&ext.id))
        {
            Some(extension) => Arc::new(PlayoutWriter {
                writer,
                // Two 12-bit fields, in 10ms units: minimum 0, maximum 10.
                // Repeat on every packet for joins, packet loss, and ICE restarts.
                extension: Extension {
                    id: extension.id as u8,
                    payload: vec![0, 0, 10].into(),
                },
            }),
            None => writer,
        }
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
    async fn close(&self) -> InterceptorResult<()> {
        Ok(())
    }
}

struct PlayoutWriter {
    writer: Arc<dyn RTPWriter + Send + Sync>,
    extension: Extension,
}
#[async_trait]
impl RTPWriter for PlayoutWriter {
    async fn write(&self, packet: &Packet, attributes: &Attributes) -> InterceptorResult<usize> {
        // Never mutate the shared track packet or another viewer's extensions.
        let mut outgoing = packet.clone();
        // The RTP helper chooses its header format from payload length alone.
        // IDs above 14 require the two-byte format even for this 3-byte hint.
        // Promote existing one-byte extensions and recalculate their padding.
        if self.extension.id > 14
            && (!outgoing.header.extension
                || outgoing.header.extension_profile == EXTENSION_PROFILE_ONE_BYTE)
        {
            outgoing.header.extension = true;
            outgoing.header.extension_profile = EXTENSION_PROFILE_TWO_BYTE;
            let size: usize = outgoing
                .header
                .extensions
                .iter()
                .map(|ext| ext.payload.len() + 2)
                .sum();
            outgoing.header.extensions_padding = (4 - size % 4) % 4;
        }
        if outgoing
            .header
            .set_extension(self.extension.id, self.extension.payload.clone())
            .is_err()
        {
            // An optional playback preference must not interrupt media using an
            // unsupported header format. Preserve the original packet instead.
            return self.writer.write(packet, attributes).await;
        }
        self.writer.write(&outgoing, attributes).await
    }
}

#[cfg(test)]
#[path = "playout_tests.rs"]
mod tests;
