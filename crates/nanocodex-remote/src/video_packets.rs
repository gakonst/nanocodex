//! Per-viewer RTP packetization using capture time, including skipped frames.
use bytes::Bytes;
use std::time::Instant;
use webrtc::rtp::{
    codecs::h264::H264Payloader,
    packet::Packet,
    packetizer::{Packetizer, new_packetizer},
    sequence::new_random_sequencer,
};

pub(crate) struct VideoPackets {
    packetizer: Box<dyn Packetizer + Send + Sync>,
    origin: Option<Instant>,
    ticks: u32,
}
impl VideoPackets {
    pub(crate) fn new() -> Self {
        Self {
            // Match webrtc's sample track MTU. SSRC/PT are replaced by the
            // negotiated static RTP track; the sequencer and timestamp are random.
            packetizer: Box::new(new_packetizer(
                1200,
                0,
                0,
                Box::<H264Payloader>::default(),
                Box::new(new_random_sequencer()),
                90_000,
            )),
            origin: None,
            ticks: 0,
        }
    }
    pub(crate) fn packetize(
        &mut self,
        data: &Bytes,
        captured_at: Instant,
    ) -> webrtc::error::Result<Vec<Packet>> {
        let origin = *self.origin.get_or_insert(captured_at);
        let ticks = (captured_at.saturating_duration_since(origin).as_nanos() * 90_000
            / 1_000_000_000) as u32;
        // Advance before emitting the resumed frame. Sample.duration advances
        // after emission and would move a skipped-frame gap one frame too late.
        self.packetizer.skip_samples(ticks.wrapping_sub(self.ticks));
        self.ticks = ticks;
        Ok(self.packetizer.packetize(data, 0)?)
    }
}
