//! Bounded-memory encoding of opaque total-state checkpoints.

use std::io::{self, BufWriter, Write};

use base64::{Engine, engine::general_purpose::STANDARD, write::EncoderWriter};
use flate2::{Compression, bufread::GzDecoder, write::GzEncoder};
use serde::{Serialize, de::DeserializeOwned};

const INLINE_BYTES: usize = 256 * 1024;
const PREFIX: &str = "nanocodex-durable-state-gzip-v1:";

type CompressedWriter =
    BufWriter<GzEncoder<EncoderWriter<'static, base64::engine::GeneralPurpose, Vec<u8>>>>;

#[derive(Default)]
struct CheckpointWriter {
    inline: Vec<u8>,
    compressed: Option<CompressedWriter>,
}

impl Write for CheckpointWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.compressed.is_none() && self.inline.len().saturating_add(bytes.len()) > INLINE_BYTES
        {
            let encoded = EncoderWriter::new(PREFIX.as_bytes().to_vec(), &STANDARD);
            let mut compressed =
                BufWriter::with_capacity(64 * 1024, GzEncoder::new(encoded, Compression::fast()));
            compressed.write_all(&self.inline)?;
            self.inline = Vec::new();
            self.compressed = Some(compressed);
        }
        if let Some(compressed) = &mut self.compressed {
            compressed.write_all(bytes)?;
        } else {
            self.inline.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.compressed {
            Some(compressed) => compressed.flush(),
            None => Ok(()),
        }
    }
}

pub(crate) fn encode(value: &impl Serialize) -> serde_json::Result<String> {
    let mut writer = CheckpointWriter::default();
    serde_json::to_writer(&mut writer, value)?;
    let bytes = match writer.compressed {
        Some(compressed) => compressed
            .into_inner()
            .map_err(|error| error.into_error())
            .and_then(GzEncoder::finish)
            .and_then(|mut encoded| encoded.finish())
            .map_err(serde_json::Error::io)?,
        None => writer.inline,
    };
    String::from_utf8(bytes)
        .map_err(|error| serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, error)))
}

pub(crate) fn decode<T: DeserializeOwned>(payload: &str) -> serde_json::Result<T> {
    let Some(encoded) = payload.strip_prefix(PREFIX) else {
        return serde_json::from_str(payload);
    };
    let compressed = STANDARD.decode(encoded).map_err(|error| {
        serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, error))
    })?;
    // Decode directly into the reduced state. Never allocate a second full
    // uncompressed JSON document while recovering a large conversation.
    let mut decoder = GzDecoder::new(compressed.as_slice());
    let value = serde_json::from_reader(&mut decoder)?;
    if !decoder.get_ref().is_empty() {
        return Err(serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing bytes in compressed durable state",
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_and_large_checkpoints_round_trip_exactly() {
        let small = serde_json::json!({"input": "quotes: \" \\ unicode: 🧪"});
        assert_eq!(encode(&small).unwrap(), small.to_string());
        assert_eq!(
            decode::<serde_json::Value>(&small.to_string()).unwrap(),
            small
        );
        let large = serde_json::json!({"history": "quoted \" 🧪\n".repeat(100_000)});
        let encoded = encode(&large).unwrap();
        assert!(encoded.starts_with(PREFIX));
        assert!(encoded.len() < large.to_string().len() / 10);
        assert_eq!(
            encoded,
            encode(&large).unwrap(),
            "retry bytes must be deterministic"
        );
        assert_eq!(decode::<serde_json::Value>(&encoded).unwrap(), large);
    }

    #[test]
    fn compressed_checkpoints_reject_corruption_truncation_and_trailing_data() {
        let encoded = encode(&"history".repeat(INLINE_BYTES)).unwrap();
        let bytes = STANDARD
            .decode(encoded.strip_prefix(PREFIX).unwrap())
            .unwrap();
        for length in [0, 1, bytes.len() / 2, bytes.len() - 1] {
            let truncated = format!("{PREFIX}{}", STANDARD.encode(&bytes[..length]));
            assert!(decode::<String>(&truncated).is_err());
        }
        let mut corrupted = bytes.clone();
        let checksum = corrupted.len() - 8;
        corrupted[checksum] ^= 1;
        assert!(decode::<String>(&format!("{PREFIX}{}", STANDARD.encode(corrupted))).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode::<String>(&format!("{PREFIX}{}", STANDARD.encode(trailing))).is_err());
        assert!(decode::<String>(&format!("{PREFIX}!invalid-base64!")).is_err());
    }
}
