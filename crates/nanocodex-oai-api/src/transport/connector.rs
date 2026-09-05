//! Registry-publishable WebSocket connection policy.
//!
//! The proxy and Happy Eyeballs behavior is adapted from the MIT-licensed
//! `openai-oss-forks/{tokio-tungstenite,tungstenite-rs}` revisions previously
//! pinned by this workspace. Keeping it here preserves that behavior while the
//! public crate depends only on crates.io packages.

use std::{collections::VecDeque, env, future::Future, io, net::SocketAddr, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream::FuturesUnordered};
use http::Uri;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::{Instant, sleep_until},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
    tungstenite::{
        Error, Result,
        handshake::client::{Request, Response},
    },
};

const HAPPY_EYEBALLS_DELAY: Duration = Duration::from_millis(250);
const MAX_CONNECT_RESPONSE_SIZE: usize = 8_192;

pub(crate) async fn connect_async(
    request: Request,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response)> {
    let host = request
        .uri()
        .host()
        .ok_or_else(|| invalid_input("WebSocket URL has no host"))?
        .to_owned();
    let secure = match request.uri().scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        _ => return Err(invalid_input("unsupported WebSocket URL scheme")),
    };
    let port = request
        .uri()
        .port_u16()
        .unwrap_or(if secure { 443 } else { 80 });

    let socket = if let Some(proxy) = ProxyConfig::from_env(request.uri(), secure)? {
        let socket = connect_happy_eyeballs(proxy.authority()).await?;
        connect_via_proxy(socket, &proxy, &host, port).await?
    } else {
        connect_happy_eyeballs(format!("{host}:{port}")).await?
    };

    client_async_tls_with_config(request, socket, None, None).await
}

#[derive(Clone, Copy)]
enum ProxyScheme {
    Http,
    Socks5,
    Socks5h,
}

struct ProxyAuth {
    username: String,
    password: String,
}

struct ProxyConfig {
    scheme: ProxyScheme,
    host: String,
    port: u16,
    auth: Option<ProxyAuth>,
}

impl ProxyConfig {
    fn from_env(uri: &Uri, secure: bool) -> Result<Option<Self>> {
        let host = uri
            .host()
            .ok_or_else(|| invalid_input("WebSocket URL has no host"))?;
        let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
        if should_bypass_proxy(host, port) {
            return Ok(None);
        }

        let value = if secure {
            get_env_first(&["HTTPS_PROXY", "https_proxy"])
                .or_else(|| get_env_first(&["HTTP_PROXY", "http_proxy"]))
        } else {
            get_env_first(&["HTTP_PROXY", "http_proxy"])
        }
        .or_else(|| get_env_first(&["ALL_PROXY", "all_proxy"]));

        value.map(|value| Self::parse(&value)).transpose()
    }

    fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        let uri = value
            .parse::<Uri>()
            .map_err(|_| invalid_input(format!("invalid proxy URL {value:?}")))?;
        let scheme = match uri.scheme_str() {
            Some("http") => ProxyScheme::Http,
            Some("socks5") => ProxyScheme::Socks5,
            Some("socks5h") => ProxyScheme::Socks5h,
            _ => return Err(invalid_input("unsupported proxy URL scheme")),
        };
        let authority = uri
            .authority()
            .ok_or_else(|| invalid_input("proxy URL has no authority"))?
            .as_str();
        let (userinfo, host_port) = authority
            .rsplit_once('@')
            .map_or((None, authority), |(userinfo, host_port)| {
                (Some(userinfo), host_port)
            });
        let host_uri = format!("http://{host_port}")
            .parse::<Uri>()
            .map_err(|_| invalid_input("proxy URL has an invalid host"))?;
        let host = host_uri
            .host()
            .ok_or_else(|| invalid_input("proxy URL has no host"))?
            .to_owned();
        let port = host_uri.port_u16().unwrap_or(match scheme {
            ProxyScheme::Http => 80,
            ProxyScheme::Socks5 | ProxyScheme::Socks5h => 1080,
        });
        let auth = userinfo.map(parse_userinfo).transpose()?;
        Ok(Self {
            scheme,
            host,
            port,
            auth,
        })
    }

    fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

fn get_env_first(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| env::var(key).ok())
        .filter(|value| !value.is_empty())
}

fn should_bypass_proxy(host: &str, port: u16) -> bool {
    let Some(no_proxy) = get_env_first(&["NO_PROXY", "no_proxy"]) else {
        return false;
    };
    let no_proxy = no_proxy.trim();
    if no_proxy == "*" {
        return true;
    }
    if no_proxy.is_empty() {
        return false;
    }

    let host = normalize_host(host);
    no_proxy.split(',').any(|token| {
        let token = token.trim();
        if token.is_empty() {
            return false;
        }
        let (token_host, token_port) = split_host_port(token);
        if token_port.is_some_and(|token_port| token_port != port) {
            return false;
        }
        let token_host = normalize_host(token_host);
        let token_host = token_host.strip_prefix('.').unwrap_or(token_host);
        host == token_host || host.ends_with(&format!(".{token_host}"))
    })
}

fn split_host_port(token: &str) -> (&str, Option<u16>) {
    if token.starts_with('[') {
        if let Some(close) = token.find(']') {
            let host = &token[..=close];
            let port = token[close + 1..]
                .strip_prefix(':')
                .and_then(|port| port.parse().ok());
            return (host, port);
        }
        return (token, None);
    }
    if token.matches(':').count() == 1 {
        let (host, port) = token.rsplit_once(':').unwrap_or((token, ""));
        return (host, port.parse().ok());
    }
    (token, None)
}

fn normalize_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
}

fn parse_userinfo(userinfo: &str) -> Result<ProxyAuth> {
    let (username, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
    Ok(ProxyAuth {
        username: percent_decode(username)?,
        password: percent_decode(password)?,
    })
}

fn percent_decode(value: &str) -> Result<String> {
    let mut output = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes
                .next()
                .ok_or_else(|| invalid_input("invalid proxy credential encoding"))?;
            let low = bytes
                .next()
                .ok_or_else(|| invalid_input("invalid proxy credential encoding"))?;
            output.push((from_hex(high)? << 4) | from_hex(low)?);
        } else {
            output.push(byte);
        }
    }
    String::from_utf8(output).map_err(|_| invalid_input("proxy credentials are not UTF-8"))
}

fn from_hex(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(invalid_input("invalid proxy credential encoding")),
    }
}

async fn connect_via_proxy<S>(
    mut stream: S,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match proxy.scheme {
        ProxyScheme::Http => http_connect(&mut stream, host, port, proxy.auth.as_ref()).await?,
        ProxyScheme::Socks5 | ProxyScheme::Socks5h => {
            socks5_connect(&mut stream, host, port, proxy.auth.as_ref()).await?;
        }
    }
    Ok(stream)
}

async fn http_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let authority = format!("{host}:{port}");
    let mut request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n"
    );
    if let Some(auth) = auth {
        let token = STANDARD.encode(format!("{}:{}", auth.username, auth.password));
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&token);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(Error::Io)?;
    stream.flush().await.map_err(Error::Io)?;

    let response = read_connect_response(stream).await?;
    let response = std::str::from_utf8(&response)
        .map_err(|_| invalid_input("HTTP CONNECT response is not UTF-8"))?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| invalid_input("HTTP CONNECT response has no valid status"))?;
    if !(200..300).contains(&status) {
        return Err(invalid_input(format!(
            "HTTP CONNECT failed with status {status}"
        )));
    }
    Ok(())
}

async fn read_connect_response<S>(stream: &mut S) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut response = Vec::new();
    let mut chunk = [0; 512];
    loop {
        if response.len() >= MAX_CONNECT_RESPONSE_SIZE {
            return Err(invalid_input("HTTP CONNECT response is too large"));
        }
        let read = stream.read(&mut chunk).await.map_err(Error::Io)?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    Ok(response)
}

async fn socks5_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let methods: &[u8] = if auth.is_some() {
        &[0x05, 0x02, 0x00, 0x02]
    } else {
        &[0x05, 0x01, 0x00]
    };
    stream.write_all(methods).await.map_err(Error::Io)?;
    stream.flush().await.map_err(Error::Io)?;

    let mut choice = [0; 2];
    stream.read_exact(&mut choice).await.map_err(Error::Io)?;
    if choice[0] != 0x05 {
        return Err(invalid_input("SOCKS5 proxy returned an invalid version"));
    }
    match choice[1] {
        0x00 => {}
        0x02 => socks5_authenticate(stream, auth).await?,
        0xff => return Err(invalid_input("SOCKS5 proxy rejected authentication")),
        _ => return Err(invalid_input("SOCKS5 proxy selected an unsupported method")),
    }

    let host = host.as_bytes();
    if host.len() > usize::from(u8::MAX) {
        return Err(invalid_input("SOCKS5 destination host is too long"));
    }
    let host_length = u8::try_from(host.len())
        .map_err(|_| invalid_input("SOCKS5 destination host is too long"))?;
    let mut request = Vec::with_capacity(host.len() + 7);
    request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_length]);
    request.extend_from_slice(host);
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await.map_err(Error::Io)?;
    stream.flush().await.map_err(Error::Io)?;

    let mut header = [0; 4];
    stream.read_exact(&mut header).await.map_err(Error::Io)?;
    if header[0] != 0x05 || header[1] != 0x00 {
        return Err(invalid_input(format!(
            "SOCKS5 connection failed with code {}",
            header[1]
        )));
    }
    let address_len = match header[3] {
        0x01 => 4,
        0x03 => {
            let mut length = [0];
            stream.read_exact(&mut length).await.map_err(Error::Io)?;
            usize::from(length[0])
        }
        0x04 => 16,
        _ => {
            return Err(invalid_input(
                "SOCKS5 proxy returned an invalid address type",
            ));
        }
    };
    let mut remainder = vec![0; address_len + 2];
    stream.read_exact(&mut remainder).await.map_err(Error::Io)?;
    Ok(())
}

async fn socks5_authenticate<S>(stream: &mut S, auth: Option<&ProxyAuth>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let auth = auth.ok_or_else(|| invalid_input("SOCKS5 proxy requested credentials"))?;
    let username = auth.username.as_bytes();
    let password = auth.password.as_bytes();
    if username.len() > usize::from(u8::MAX) || password.len() > usize::from(u8::MAX) {
        return Err(invalid_input("SOCKS5 proxy credentials are too long"));
    }
    let username_length = u8::try_from(username.len())
        .map_err(|_| invalid_input("SOCKS5 proxy username is too long"))?;
    let password_length = u8::try_from(password.len())
        .map_err(|_| invalid_input("SOCKS5 proxy password is too long"))?;
    let mut request = Vec::with_capacity(username.len() + password.len() + 3);
    request.extend_from_slice(&[0x01, username_length]);
    request.extend_from_slice(username);
    request.push(password_length);
    request.extend_from_slice(password);
    stream.write_all(&request).await.map_err(Error::Io)?;
    stream.flush().await.map_err(Error::Io)?;

    let mut response = [0; 2];
    stream.read_exact(&mut response).await.map_err(Error::Io)?;
    if response != [0x01, 0x00] {
        return Err(invalid_input("SOCKS5 authentication failed"));
    }
    Ok(())
}

async fn connect_happy_eyeballs(address: impl tokio::net::ToSocketAddrs) -> Result<TcpStream> {
    let addresses = tokio::net::lookup_host(address)
        .await
        .map_err(Error::Io)?
        .collect();
    happy_eyeballs_connect(addresses, TcpStream::connect)
        .await
        .map_err(Error::Io)
}

async fn happy_eyeballs_connect<T, F, Fut>(
    addresses: Vec<SocketAddr>,
    mut connect: F,
) -> io::Result<T>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let mut addresses = interleave_addresses(addresses);
    let Some(first) = addresses.pop_front() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "host did not resolve to any address",
        ));
    };
    let mut attempts = FuturesUnordered::new();
    attempts.push(connect(first));
    let mut next_attempt = Instant::now() + HAPPY_EYEBALLS_DELAY;
    let mut last_error = None;

    loop {
        if addresses.is_empty() {
            match attempts.next().await {
                Some(Ok(stream)) => return Ok(stream),
                Some(Err(error)) => {
                    last_error = Some(error);
                    if attempts.is_empty() {
                        break;
                    }
                }
                None => break,
            }
            continue;
        }

        tokio::select! {
            result = attempts.next() => {
                if let Some(Ok(stream)) = result {
                    return Ok(stream);
                }
                if let Some(Err(error)) = result {
                    last_error = Some(error);
                }
            }
            () = sleep_until(next_attempt) => {}
        }
        if let Some(address) = addresses.pop_front() {
            attempts.push(connect(address));
            next_attempt = Instant::now() + HAPPY_EYEBALLS_DELAY;
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "all connection attempts failed",
        )
    }))
}

fn interleave_addresses(addresses: Vec<SocketAddr>) -> VecDeque<SocketAddr> {
    let mut addresses = addresses.into_iter();
    let Some(first) = addresses.next() else {
        return VecDeque::new();
    };
    let first_is_ipv4 = first.is_ipv4();
    let mut preferred = VecDeque::from([first]);
    let mut alternate = VecDeque::new();
    for address in addresses {
        if address.is_ipv4() == first_is_ipv4 {
            preferred.push_back(address);
        } else {
            alternate.push_back(address);
        }
    }

    let mut interleaved = VecDeque::new();
    while !preferred.is_empty() || !alternate.is_empty() {
        if let Some(address) = preferred.pop_front() {
            interleaved.push_back(address);
        }
        if let Some(address) = alternate.pop_front() {
            interleaved.push_back(address);
        }
    }
    interleaved
}

fn invalid_input(message: impl Into<String>) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaves_address_families() {
        let v6_a = "[::1]:1".parse().unwrap();
        let v6_b = "[::1]:2".parse().unwrap();
        let v4_a = "127.0.0.1:1".parse().unwrap();
        let v4_b = "127.0.0.1:2".parse().unwrap();
        assert_eq!(
            interleave_addresses(vec![v6_a, v6_b, v4_a, v4_b])
                .into_iter()
                .collect::<Vec<_>>(),
            vec![v6_a, v4_a, v6_b, v4_b]
        );
    }

    #[test]
    fn parses_proxy_authentication() {
        let proxy = ProxyConfig::parse("http://user:p%40ss@proxy.local:3128").unwrap();
        assert_eq!(proxy.host, "proxy.local");
        assert_eq!(proxy.port, 3128);
        let auth = proxy.auth.unwrap();
        assert_eq!(auth.username, "user");
        assert_eq!(auth.password, "p@ss");
    }
}
