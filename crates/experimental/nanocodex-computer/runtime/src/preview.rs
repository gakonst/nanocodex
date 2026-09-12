//! Owned local preview surface. A capability URL exposes only the latest frame;
//! it cannot inject input or select another capture target.
use crate::{Error, Result, native::Image};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const MAX_IMAGE: usize = 12 * 1024 * 1024;
struct Shared {
    frame: Value,
    sequence: u64,
    closed: bool,
}
pub struct Preview {
    pub app: String,
    owner: String,
    url: String,
    until: Instant,
    next: Instant,
    interval: Duration,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Preview {
    pub fn start(owner: &str, app: &str, duration: Duration, interval: Duration) -> Result<Self> {
        if duration.is_zero()
            || duration > Duration::from_secs(60)
            || interval < Duration::from_millis(250)
            || interval > Duration::from_secs(10)
        {
            return Err(Error::invalid(
                "Preview duration must be 1..60000ms and interval 250..10000ms",
            ));
        }
        if owner.trim().is_empty()
            || owner.len() > 4096
            || app.trim().is_empty()
            || app.len() > 4096
        {
            return Err(Error::invalid(
                "Preview owner/app must be bounded nonempty identities",
            ));
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| Error::action("Preview token unavailable"))?;
        let path = format!(
            "/{}",
            token.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let url = format!("http://{}{path}", listener.local_addr()?);
        let shared = Arc::new(Mutex::new(Shared {
            frame: Value::Null,
            sequence: 0,
            closed: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (state, cancel) = (shared.clone(), stop.clone());
        let until = Instant::now() + duration;
        let thread = thread::spawn(move || {
            while !cancel.load(Ordering::Acquire) && Instant::now() < until {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let _ = respond(socket, &path, &state, until, &cancel);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
            let mut state = state.lock().unwrap();
            state.closed = true;
            state.frame = Value::Null;
        });
        Ok(Self {
            app: app.into(),
            owner: owner.into(),
            url,
            until,
            next: Instant::now(),
            interval,
            shared,
            stop,
            thread: Some(thread),
        })
    }
    pub fn due(&self) -> bool {
        !self.closed() && Instant::now() >= self.next
    }
    pub fn closed(&self) -> bool {
        self.stop.load(Ordering::Acquire)
            || Instant::now() >= self.until
            || self.shared.lock().unwrap().closed
    }
    pub fn publish(&mut self, owner: &str, image: Image) -> Result<()> {
        self.check_owner(owner)?;
        if self.closed() {
            return Err(Error::action("Preview ended"));
        }
        if !["image/png", "image/jpeg"].contains(&image.mime_type.as_str())
            || image.data.len() > MAX_IMAGE
        {
            return Err(Error::invalid("Preview frame exceeds image limits"));
        }
        let bytes = STANDARD
            .decode(&image.data)
            .map_err(|_| Error::invalid("Preview frame is not base64"))?;
        let format = if image.mime_type == "image/png" {
            image::ImageFormat::Png
        } else {
            image::ImageFormat::Jpeg
        };
        let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
            .into_dimensions()
            .map_err(|_| Error::invalid("Invalid preview image"))?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 40_000_000 {
            return Err(Error::invalid("Preview image dimensions exceed limits"));
        }
        let mut state = self.shared.lock().unwrap();
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::action("Preview frame sequence exhausted"))?;
        state.frame = serde_json::to_value(image)?;
        self.next = Instant::now() + self.interval;
        Ok(())
    }
    pub fn status(&self, owner: &str) -> Result<Value> {
        self.check_owner(owner)?;
        let state = self.shared.lock().unwrap();
        Ok(
            json!({"url":self.url,"app":self.app,"sequence":state.sequence,"closed":state.closed||self.stop.load(Ordering::Acquire)||Instant::now()>=self.until}),
        )
    }
    pub fn close(&mut self, owner: &str) -> Result<()> {
        self.check_owner(owner)?;
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.shared.lock().unwrap().frame = Value::Null;
        Ok(())
    }
    fn check_owner(&self, owner: &str) -> Result<()> {
        if owner == self.owner {
            Ok(())
        } else {
            Err(Error::new(-32003, "Preview belongs to another session"))
        }
    }
}
impl Drop for Preview {
    fn drop(&mut self) {
        let owner = self.owner.clone();
        let _ = self.close(&owner);
    }
}
fn respond(
    mut socket: TcpStream,
    path: &str,
    state: &Arc<Mutex<Shared>>,
    until: Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    let deadline = (Instant::now() + Duration::from_millis(250)).min(until);
    // BSD accept inherits the listener's nonblocking flag. SO_RCVTIMEO alone
    // does not clear it: a valid fragmented request would otherwise close on
    // the first WouldBlock, before its absolute header deadline.
    socket.set_nonblocking(false)?;
    let expected_host = socket.local_addr()?.to_string();
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    let mut complete = false;
    while request.len() < 8192 {
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Ok(());
        }
        socket.set_read_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ))?;
        let remaining = (8192 - request.len()).min(chunk.len());
        let count = match socket.read(&mut chunk[..remaining]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if count == 0 {
            return Ok(());
        }
        let scan_start = request.len().saturating_sub(3);
        request.extend_from_slice(&chunk[..count]);
        if let Some(offset) = request[scan_start..]
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
        {
            // Ignore bytes following this request's header, as the old
            // byte-at-a-time reader did; this connection is never reused.
            request.truncate(scan_start + offset + 4);
            complete = true;
            break;
        }
    }
    let request = String::from_utf8_lossy(&request);
    let mut lines = request.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    let mut hosts = Vec::new();
    let mut forbidden = method != "GET";
    let mut malformed =
        !complete || parts.next().is_some() || !["HTTP/1.0", "HTTP/1.1"].contains(&version);
    for header in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = header.split_once(':') else {
            malformed = true;
            continue;
        };
        if name.is_empty()
            || name
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && b != b'-')
        {
            malformed = true;
        }
        match name.to_ascii_lowercase().as_str() {
            "origin" | "cookie" => forbidden = true,
            "host" => hosts.push(value.trim()),
            "transfer-encoding" => malformed = true,
            "content-length" if value.trim() != "0" => malformed = true,
            _ => (),
        }
    }
    if hosts.len() != 1 || hosts[0] != expected_host {
        forbidden = true;
    }
    let (status, mime, body) = if malformed {
        ("400 Bad Request", "text/plain", "Malformed request".into())
    } else if forbidden {
        ("403 Forbidden", "text/plain", "Forbidden".into())
    } else if target == path {
        ("200 OK", "text/html; charset=utf-8", html(path))
    } else if target == format!("{path}/frame") {
        let s = state.lock().unwrap();
        (
            "200 OK",
            "application/json",
            json!({"sequence":s.sequence,"closed":s.closed,"frame":s.frame}).to_string(),
        )
    } else {
        ("404 Not Found", "text/plain", "Not found".into())
    };
    let mut response=format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; img-src data:; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'\r\n\r\n",body.len()).into_bytes();
    response.extend_from_slice(body.as_bytes());
    socket.set_nonblocking(true)?;
    let deadline = (Instant::now() + Duration::from_millis(500)).min(until);
    let mut written = 0;
    while written < response.len() {
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Ok(());
        }
        match socket.write(&response[written..]) {
            Ok(0) => return Ok(()),
            Ok(count) => written += count,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn html(path: &str) -> String {
    format!(
        r#"<!doctype html><meta charset="utf-8"><title>Skyre owned preview</title><style>body{{margin:0;background:#111;color:#ddd;font:14px system-ui}}p{{margin:12px}}img{{max-width:100vw;max-height:85vh}}button{{margin:12px}}</style><p id="status">Waiting for a captured frame</p><button id="stop">Stop viewing</button><img id="frame" alt="Owned application preview"><script>let active=true,last=-1;document.querySelector('#stop').onclick=()=>{{active=false;document.querySelector('#frame').removeAttribute('src');document.querySelector('#status').textContent='Viewing stopped'}};async function tick(){{if(!active)return;try{{const r=await fetch('{path}/frame',{{cache:'no-store',credentials:'omit'}});const v=await r.json();if(v.closed){{active=false;document.querySelector('#status').textContent='Capture ended';return}}if(v.frame&&v.sequence!==last){{last=v.sequence;document.querySelector('#frame').src='data:'+v.frame.mime_type+';base64,'+v.frame.data;document.querySelector('#status').textContent='Frame '+last}}}}catch{{document.querySelector('#status').textContent='Preview disconnected';active=false}}if(active)setTimeout(tick,300)}}tick();</script>"#
    )
}
