//! Pure Pulse monitor binding and fragment contracts; PCM/lifetimes use the shared recorder.
use super::windows_audio_model::{Format, MAX_PACKET_BYTES, Packet, SILENT};
use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub sink: u32,
    pub sink_name: String,
    pub source: u32,
    pub source_name: String,
    pub rate: u32,
}
impl Monitor {
    pub fn validate(
        &self,
        source: u32,
        name: &str,
        monitor_of_sink: u32,
        sink_name: &str,
    ) -> Result<()> {
        if self.sink == u32::MAX
            || self.source == u32::MAX
            || self.sink != monitor_of_sink
            || self.source != source
            || self.source_name != name
            || self.sink_name != sink_name
            || !(8_000..=192_000).contains(&self.rate)
            || [&self.sink_name, &self.source_name]
                .iter()
                .any(|s| s.is_empty() || s.len() > 4096 || s.contains('\0'))
        {
            return Err(Error::action(
                "PulseAudio source is not the selected render sink's monitor",
            ));
        }
        Ok(())
    }
    pub fn validate_stream(&self, index: u32, name: &str) -> Result<()> {
        if index != self.source || name != self.source_name {
            Err(Error::action(
                "PulseAudio recording source identity changed",
            ))
        } else {
            Ok(())
        }
    }
}

/// A null nonempty Pulse fragment is a hole (silence), not end-of-stream.
pub fn fragment(
    data: Option<&[u8]>,
    bytes: usize,
    format: Format,
    position: u64,
) -> Result<Option<Packet>> {
    format.validate()?;
    if bytes == 0 {
        return Ok(None);
    }
    if bytes > MAX_PACKET_BYTES
        || !bytes.is_multiple_of(format.block_align as usize)
        || data.is_some_and(|d| d.len() != bytes)
    {
        return Err(Error::action("Invalid or oversized PulseAudio fragment"));
    }
    let frames = u32::try_from(bytes / format.block_align as usize)
        .map_err(|_| Error::action("PulseAudio frame count overflow"))?;
    format.packet_bytes(frames)?;
    position
        .checked_add(frames as u64)
        .ok_or_else(|| Error::action("PulseAudio frame position overflow"))?;
    let mut owned = Vec::new();
    if let Some(data) = data {
        owned
            .try_reserve_exact(bytes)
            .map_err(|_| Error::action("PulseAudio fragment allocation failed"))?;
        owned.extend_from_slice(data);
    }
    Ok(Some(Packet {
        data: owned,
        frames,
        flags: if data.is_none() { SILENT } else { 0 },
        position,
    }))
}

/// Unlike WASAPI, Pulse drops the whole nonempty fragment on every release path.
pub struct FragmentLease<F: FnOnce() -> Result<()>>(Option<F>);
impl<F: FnOnce() -> Result<()>> FragmentLease<F> {
    pub fn new(release: F) -> Self {
        Self(Some(release))
    }
    pub fn finish<T>(mut self, result: Result<T>) -> Result<T> {
        let cleanup = self.0.take().unwrap()();
        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(mut error), Err(cleanup)) => {
                error
                    .message
                    .push_str(&format!("; fragment drop failed: {}", cleanup.message));
                Err(error)
            }
        }
    }
}
impl<F: FnOnce() -> Result<()>> Drop for FragmentLease<F> {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release();
        }
    }
}
