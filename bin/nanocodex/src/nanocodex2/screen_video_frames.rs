//! Preserve encoder packet boundaries without waiting for the next H.264 AUD.
//! FFmpeg's tee muxer must flush framecrc metadata before its matching H.264
//! output. Pipe read boundaries are never interpreted as frame boundaries.
use std::io;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};

pub(crate) const FRAMED_H264_MAGIC: &[u8; 8] = b"NCH264F1";
pub(crate) const MAX_H264_FRAME: usize = 8 * 1024 * 1024;
const MAX_METADATA_LINE: usize = 16 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Read one complete newline-terminated metadata record, bounding allocation
/// before copying from the reader. EOF between records is clean; an unfinished
/// line is truncation, even if its prefix happens to resemble a valid record.
async fn metadata_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<bool> {
    line.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(false)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated encoder metadata",
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.unwrap_or(available.len());
        if count > MAX_METADATA_LINE.saturating_sub(line.len()) {
            return Err(invalid("encoder metadata line exceeds 16 KiB"));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(true);
        }
    }
}
fn frame_size(line: &[u8]) -> io::Result<Option<usize>> {
    let line = std::str::from_utf8(line).map_err(|_| invalid("encoder metadata is not UTF-8"))?;
    if line.starts_with('#') {
        return Ok(None);
    }
    let mut fields = line.split(',');
    if fields.next().map(str::trim) != Some("0") {
        return Err(invalid("invalid encoder metadata stream"));
    }
    // The framecrc schema is stream, DTS, PTS, duration, size, CRC, [side data].
    // Packet length is authoritative; neither codec delimiters nor read sizes are.
    let size = fields
        .nth(3)
        .ok_or_else(|| invalid("incomplete encoder frame metadata"))?;
    if fields.next().is_none() {
        return Err(invalid("incomplete encoder frame metadata"));
    }
    let size: usize = size
        .trim()
        .parse()
        .map_err(|_| invalid("invalid encoder frame size"))?;
    if !(1..=MAX_H264_FRAME).contains(&size) {
        return Err(invalid("encoder frame size exceeds bounds"));
    }
    Ok(Some(size))
}

/// Forward exact encoded packets in the framing shared with the Go Hand:
/// `NCH264F1`, followed by repeated big-endian u32 lengths and H.264 packet bytes.
/// A packet is fully read and validated for length before any of its framing is
/// published. Callers own encoder shutdown and deadlines on their pipe readers.
pub(crate) async fn forward_encoded_frames<M, V, W>(
    metadata: M,
    mut video: V,
    mut output: W,
) -> io::Result<()>
where
    M: AsyncRead + Unpin,
    V: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut metadata = BufReader::with_capacity(1024, metadata);
    let mut line = Vec::with_capacity(1024);
    let mut frame = Vec::new();
    output.write_all(FRAMED_H264_MAGIC).await?;
    output.flush().await?;
    while metadata_line(&mut metadata, &mut line).await? {
        let Some(size) = frame_size(&line)? else {
            continue;
        };
        frame.resize(size, 0);
        video.read_exact(&mut frame).await?;
        output.write_all(&(size as u32).to_be_bytes()).await?;
        output.write_all(&frame).await?;
        output.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::Stdio, time::Duration};

    #[tokio::test]
    async fn fragmented_pipes_preserve_exact_encoder_packets() {
        let (mut metadata_writer, metadata) = tokio::io::duplex(1);
        let (mut video_writer, video) = tokio::io::duplex(1);
        let metadata_task = tokio::spawn(async move {
            metadata_writer
                .write_all(b"#tb 0: 1/60\n0, 0, 0, 1, 5, 0xabc\n0, 1, 1, 1, 3, 0x123, extra\r\n")
                .await
                .unwrap();
        });
        let video_task = tokio::spawn(async move {
            video_writer.write_all(b"firsttwo").await.unwrap();
        });
        let mut output = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            forward_encoded_frames(metadata, video, &mut output),
        )
        .await
        .unwrap()
        .unwrap();
        metadata_task.await.unwrap();
        video_task.await.unwrap();
        let mut expected = FRAMED_H264_MAGIC.to_vec();
        expected.extend_from_slice(&5u32.to_be_bytes());
        expected.extend_from_slice(b"first");
        expected.extend_from_slice(&3u32.to_be_bytes());
        expected.extend_from_slice(b"two");
        assert_eq!(output, expected);
    }

    #[tokio::test]
    async fn malformed_metadata_and_truncated_packets_never_publish_a_frame() {
        let mut cases = vec![
            b"garbage\n".to_vec(),
            b"\n".to_vec(),
            b"0,0,0,1,4\n".to_vec(),
            b"1,0,0,1,4,0x0\n".to_vec(),
            b"0,0,0,1,0,0x0\n".to_vec(),
            b"0,0,0,1,-1,0x0\n".to_vec(),
            b"0,0,0,1,8388609,0x0\n".to_vec(),
            b"0,0,0,1,999999999999999999999999999999,0x0\n".to_vec(),
            b"0,0,0,1,8,0x0\n".to_vec(), // Only four encoded bytes below.
            b"0,0,0,1,4,0x0".to_vec(),   // Metadata itself was truncated.
            vec![0xff, b'\n'],
        ];
        cases.push(
            [
                b"#".as_slice(),
                vec![b'x'; MAX_METADATA_LINE].as_slice(),
                b"\n",
            ]
            .concat(),
        );
        for metadata in cases {
            let mut output = Vec::new();
            let result =
                forward_encoded_frames(metadata.as_slice(), b"tiny".as_slice(), &mut output).await;
            assert!(result.is_err(), "accepted metadata {metadata:?}");
            assert_eq!(output, FRAMED_H264_MAGIC, "published an incomplete packet");
        }
    }

    #[tokio::test]
    async fn oversized_header_errors_before_reading_any_video() {
        // Header comments are bounded too; no terminating newline is needed to
        // trigger rejection, so a malicious peer cannot grow this buffer forever.
        let mut header = vec![b'x'; MAX_METADATA_LINE + 1];
        header[0] = b'#';
        let mut output = Vec::new();
        let error = forward_encoded_frames(header.as_slice(), tokio::io::empty(), &mut output)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("16 KiB"));
        assert_eq!(output, FRAMED_H264_MAGIC);
    }

    #[test]
    fn accepts_bounded_sizes_and_extra_framecrc_fields() {
        assert_eq!(
            frame_size(b"0, -1, -1, 1, 8388608, 0x0, S=1").unwrap(),
            Some(MAX_H264_FRAME)
        );
        assert_eq!(frame_size(b"0, 0, 0, 1, 1, 0x0").unwrap(), Some(1));
        assert_eq!(frame_size(b"#software: Lavf").unwrap(), None);
    }

    /// One real input frame, with stdin deliberately left open: an AUD
    /// lookahead parser cannot satisfy this test because no second frame exists.
    #[tokio::test]
    async fn ffmpeg_forwards_one_frame_while_input_remains_open() {
        let configured = std::env::var_os("NANOCODEX_TEST_FFMPEG");
        let ffmpeg = configured.clone().unwrap_or_else(|| "ffmpeg".into());
        let command = tokio::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "quiet",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-f",
                "rawvideo",
                "-pixel_format",
                "bgra",
                "-video_size",
                "64x64",
                "-framerate",
                "60",
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-pix_fmt",
                "yuv420p",
                "-x264-params",
                "aud=1:repeat-headers=1",
                "-map",
                "0:v:0",
                "-f",
                "tee",
                "[f=framecrc:flush_packets=1]pipe:2|[f=h264:flush_packets=1]pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut child = match command {
            Err(error) if configured.is_none() && error.kind() == io::ErrorKind::NotFound => {
                eprintln!("skipping encoder test: FFmpeg is not installed");
                return;
            }
            result => result.expect("start FFmpeg (configured paths must exist)"),
        };
        let mut input = child.stdin.take().unwrap();
        let video = child.stdout.take().unwrap();
        let metadata = child.stderr.take().unwrap();
        let (sink, mut output) = tokio::io::duplex(1024 * 1024);
        let forwarding = tokio::spawn(forward_encoded_frames(metadata, video, sink));
        input.write_all(&[0; 64 * 64 * 4]).await.unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            let mut magic = [0; 8];
            output.read_exact(&mut magic).await.unwrap();
            assert_eq!(&magic, FRAMED_H264_MAGIC);
            let size = output.read_u32().await.unwrap() as usize;
            assert!((10..=MAX_H264_FRAME).contains(&size));
            let mut frame = vec![0; size];
            output.read_exact(&mut frame).await.unwrap();
            frame
        })
        .await
        .expect("encoder waited for a second input frame");
        assert!(frame.starts_with(&[0, 0, 0, 1]) || frame.starts_with(&[0, 0, 1]));
        assert!(
            child.try_wait().unwrap().is_none(),
            "FFmpeg exited before stdin was closed"
        );
        drop(input);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        tokio::time::timeout(Duration::from_secs(5), forwarding)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
