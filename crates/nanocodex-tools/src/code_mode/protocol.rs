use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const MAX_PROTOCOL_LINE_BYTES: u64 = 8 * 1024 * 1024;

pub(super) async fn write_json_line(
    stdin: &mut tokio::process::ChildStdin,
    value: &impl Serialize,
) -> std::io::Result<()> {
    let mut encoded = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    encoded.push(b'\n');
    stdin.write_all(&encoded).await?;
    stdin.flush().await
}

pub(super) async fn read_protocol_line(
    stdout: &mut BufReader<tokio::process::ChildStdout>,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let read = stdout
        .take(MAX_PROTOCOL_LINE_BYTES + 1)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if u64::try_from(read).unwrap_or(u64::MAX) > MAX_PROTOCOL_LINE_BYTES || !line.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "local code-mode protocol line exceeded 8 MiB",
        ));
    }
    line.pop();
    Ok(Some(line))
}
