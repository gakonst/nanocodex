//! Restart-safe Waymote framing. Restart markers are accepted only at record boundaries.
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};
const MAX_FRAME: usize = 8 * 1024 * 1024;
pub(crate) async fn read<R: AsyncRead + Unpin>(
    mut reader: R,
    mut emit: impl FnMut(Vec<u8>),
) -> io::Result<()> {
    let mut magic = [0; 8];
    reader.read_exact(&mut magic).await?;
    let mut chunked = mode(&magic)?;
    let mut frame = Vec::new();
    loop {
        let mut header = [0; 4];
        // Distinguish a clean boundary EOF from a truncated header or frame.
        if reader.read(&mut header[..1]).await? == 0 {
            return if frame.is_empty() {
                Ok(())
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        reader.read_exact(&mut header[1..]).await?;
        if &header == b"NCH2" {
            magic[..4].copy_from_slice(&header);
            reader.read_exact(&mut magic[4..]).await?;
            chunked = mode(&magic)?;
            frame.clear();
            continue;
        }
        let value = u32::from_be_bytes(header);
        let final_chunk = !chunked || value & 0x80000000 != 0;
        let size = if chunked { value & 0x7fffffff } else { value } as usize;
        if size == 0
            || size > MAX_FRAME
            || (chunked && size > 4092)
            || frame.len() + size > MAX_FRAME
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Waymote frame length",
            ));
        }
        let start = frame.len();
        frame.resize(start + size, 0);
        reader.read_exact(&mut frame[start..]).await?;
        if final_chunk {
            if !frame.starts_with(&[0, 0, 1]) && !frame.starts_with(&[0, 0, 0, 1]) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid Waymote H.264 payload",
                ));
            }
            emit(std::mem::take(&mut frame));
        }
    }
}
fn mode(magic: &[u8; 8]) -> io::Result<bool> {
    match magic {
        b"NCH264C1" => Ok(true),
        b"NCH264F1" => Ok(false),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Waymote frame protocol",
        )),
    }
}
pub(crate) fn keyframe(frame: &[u8]) -> bool {
    frame
        .windows(4)
        .any(|v| v[..3] == [0, 0, 1] && v[3] & 31 == 5)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn chunk(out: &mut Vec<u8>, bytes: &[u8], last: bool) {
        out.extend_from_slice(
            &((bytes.len() as u32) | if last { 0x80000000 } else { 0 }).to_be_bytes(),
        );
        out.extend_from_slice(bytes);
    }
    #[tokio::test]
    async fn restart_discards_only_partial_frame_and_never_scans_payload() {
        let mut wire = b"NCH264C1".to_vec();
        chunk(&mut wire, b"discard", false);
        wire.extend_from_slice(b"NCH264C1");
        let frame = b"\0\0\x01\x65NCH264C1payload";
        chunk(&mut wire, &frame[..9], false);
        chunk(&mut wire, &frame[9..], true);
        wire.extend_from_slice(b"NCH264F1");
        wire.extend_from_slice(&4u32.to_be_bytes());
        wire.extend_from_slice(b"\0\0\x01\x41");
        let mut frames = Vec::new();
        read(wire.as_slice(), |f| frames.push(f)).await.unwrap();
        assert_eq!(frames, [frame.to_vec(), b"\0\0\x01\x41".to_vec()]);
        assert!(keyframe(&frames[0]));
        assert!(!keyframe(&frames[1]));
    }
    #[tokio::test]
    async fn rejects_truncation_and_oversized_chunks() {
        for suffix in [
            vec![0],
            4093u32.to_be_bytes().to_vec(),
            vec![0x80, 0, 0, 4, 0, 0, 1],
            vec![0, 0, 0, 1, 1],
        ] {
            let mut wire = b"NCH264C1".to_vec();
            wire.extend(suffix);
            assert!(
                read(wire.as_slice(), |_| panic!("partial frame emitted"))
                    .await
                    .is_err()
            );
        }
    }
}
