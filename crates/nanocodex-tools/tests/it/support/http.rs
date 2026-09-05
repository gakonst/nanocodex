use eyre::{Result, eyre};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub(crate) struct CapturedRequest {
    pub(crate) path: String,
    pub(crate) headers: String,
    pub(crate) body: Vec<u8>,
}

pub(crate) async fn read_request(stream: &mut TcpStream) -> Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(eyre!("HTTP request ended before its headers"));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?.to_owned();
    let request_line = headers
        .lines()
        .next()
        .ok_or_else(|| eyre!("HTTP request omitted request line"))?;
    let path = request_line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or_else(|| eyre!("HTTP request line omitted path"))?
        .to_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| eyre!("HTTP request omitted content-length"))?;
    while bytes.len() - header_end < content_length {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(eyre!("HTTP request body ended early"));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(CapturedRequest {
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

pub(crate) async fn write_unauthorized(stream: &mut TcpStream) -> Result<()> {
    stream
        .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        .await?;
    Ok(())
}

pub(crate) async fn write_json(stream: &mut TcpStream, value: &serde_json::Value) -> Result<()> {
    let response = serde_json::to_vec(value)?;
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.write_all(&response).await?;
    Ok(())
}
