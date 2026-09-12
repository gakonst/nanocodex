//! Independent Chrome native-messaging/CDP bridge. No vendor signing identities.
use crate::{Error, Result};
use std::path::Path;
#[cfg(unix)]
#[path = "browser_extension_assets.rs"]
mod assets;
#[cfg(unix)]
mod unix {
    use super::*;
    use super::{Chunks, split_message};
    use serde_json::{Value, json};
    use std::{
        fs,
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        os::unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        sync::mpsc,
        time::{Duration, Instant},
    };
    use tungstenite::{
        Message,
        handshake::server::{ErrorResponse, Request, Response},
    };
    pub(super) fn private_directory(path: &Path) -> Result<()> {
        if !path.exists() {
            fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(Error::action(
                "Extension socket directory must be owned and private (0700)",
            ));
        }
        Ok(())
    }
    fn peer(stream: &UnixStream) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let (mut uid, mut gid) = (0, 0);
            if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0
                || uid != unsafe { libc::geteuid() }
            {
                return Err(Error::action("Extension socket peer UID mismatch"));
            }
        }
        #[cfg(target_os = "linux")]
        {
            let mut cred = unsafe { std::mem::zeroed::<libc::ucred>() };
            let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    &mut cred as *mut _ as *mut _,
                    &mut len,
                )
            } != 0
                || cred.uid != unsafe { libc::geteuid() }
            {
                return Err(Error::action("Extension socket peer UID mismatch"));
            }
        }
        Ok(())
    }
    use std::os::fd::AsRawFd;
    fn socket_check(path: &Path) -> Result<()> {
        private_directory(
            path.parent()
                .ok_or_else(|| Error::invalid("Socket requires parent directory"))?,
        )?;
        let meta = fs::symlink_metadata(path)?;
        if !meta.file_type().is_socket()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(Error::action("Extension socket must be owned and private"));
        }
        Ok(())
    }
    fn token() -> Result<String> {
        let mut bytes = [0u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(bytes.iter().map(|n| format!("{n:02x}")).collect())
    }
    // This independently owned bridge session survives transport reconnection and
    // process restart. A desired turn is durable before it reaches the extension.
    // Reconnecting resumes it; each explicit beginTurn requests a fresh intent
    // and must not be blindly retried after an uncertain acknowledgement.
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct OwnerContext {
        session_id: String,
        turn_id: String,
        #[serde(default)]
        host_authority: String,
        #[serde(default)]
        host_route: Option<Value>,
        #[serde(default)]
        host_event: Option<Value>,
    }
    impl OwnerContext {
        fn load(path: &Path) -> Result<Self> {
            match fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
            {
                Ok(mut file) => {
                    let meta = file.metadata()?;
                    if !meta.is_file()
                        || meta.uid() != unsafe { libc::geteuid() }
                        || meta.permissions().mode() & 0o077 != 0
                        || meta.len() > 4096
                    {
                        return Err(Error::action(
                            "Extension owner state must be an owned private file",
                        ));
                    }
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes)?;
                    let mut owner: Self = serde_json::from_slice(&bytes)?;
                    if [&owner.session_id, &owner.turn_id]
                        .iter()
                        .any(|id| id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()))
                    {
                        return Err(Error::action("Invalid extension owner state"));
                    }
                    if owner.host_authority.is_empty() {
                        owner.host_authority = token()?;
                        owner.persist(path)?;
                    }
                    if owner.host_authority.len() != 64
                        || !owner.host_authority.bytes().all(|c| c.is_ascii_hexdigit())
                    {
                        return Err(Error::action("Invalid extension host authority"));
                    }
                    Ok(owner)
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let owner = Self {
                        session_id: token()?,
                        turn_id: token()?,
                        host_authority: token()?,
                        host_route: None,
                        host_event: None,
                    };
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(path)?;
                    serde_json::to_writer(&mut file, &owner)?;
                    file.sync_all()?;
                    fs::File::open(path.parent().unwrap())?.sync_all()?;
                    Ok(owner)
                }
                Err(e) => Err(e.into()),
            }
        }
        fn persist(&self, path: &Path) -> Result<()> {
            let temporary = path.with_extension(format!("{}.tmp", token()?));
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temporary)?;
                serde_json::to_writer(&mut file, self)?;
                file.sync_all()?;
                fs::rename(&temporary, path)?;
                fs::File::open(path.parent().unwrap())?.sync_all()?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(temporary);
            }
            result
        }
        fn next_turn(&mut self, path: &Path) -> Result<()> {
            if self.host_route.is_some() {
                return Err(Error::new(
                    -32003,
                    "Managed extension turns require trusted host events",
                ));
            }
            let mut next = self.clone();
            next.turn_id = token()?;
            next.persist(path)?;
            *self = next;
            Ok(())
        }
        fn host_turn(&mut self, path: &Path, request: &mut Value) -> Result<()> {
            use sha2::{Digest, Sha256};
            let provided = request["params"]["authorityToken"].as_str().unwrap_or("");
            let expected = Sha256::digest(self.host_authority.as_bytes());
            let actual = Sha256::digest(provided.as_bytes());
            if expected
                .iter()
                .zip(actual)
                .fold(0u8, |v, (a, b)| v | (a ^ b))
                != 0
            {
                return Err(Error::new(
                    -32003,
                    "Extension host lifecycle authority is required",
                ));
            }
            let mut event = request["params"].clone();
            event.as_object_mut().unwrap().remove("authorityToken");
            let _: crate::host_turns::Event = serde_json::from_value(event.clone())?;
            let route: crate::host_turns::Route = serde_json::from_value(event["route"].clone())?;
            route.validate()?;
            for key in ["turnId", "eventId"] {
                if !event[key]
                    .as_str()
                    .is_some_and(|v| !v.is_empty() && v.len() <= 256)
                {
                    return Err(Error::invalid("Invalid extension host event identity"));
                }
            }
            let sequence = event["sequence"]
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or_else(|| Error::invalid("Invalid extension host sequence"))?;
            if !["started", "ended"].contains(&event["phase"].as_str().unwrap_or("")) {
                return Err(Error::invalid("Invalid extension host phase"));
            }
            if self
                .host_route
                .as_ref()
                .is_some_and(|previous| previous != &event["route"])
            {
                return Err(Error::new(
                    -32003,
                    "Extension host event belongs to another route",
                ));
            }
            if self.host_event.as_ref().is_some_and(|previous| {
                previous != &event
                    && previous["sequence"]
                        .as_u64()
                        .is_some_and(|old| sequence <= old)
            }) {
                return Err(Error::action("Extension host event sequence is stale"));
            }
            let desired = format!(
                "{:x}",
                Sha256::digest(
                    format!("{}\n{}", route.key(), event["turnId"].as_str().unwrap()).as_bytes()
                )
            );
            if event["phase"] == "ended" && self.turn_id != desired {
                return Err(Error::action(
                    "Extension host turn end does not match current authority",
                ));
            }
            let mut next = self.clone();
            next.turn_id = desired;
            next.host_route = Some(event["route"].clone());
            next.host_event = Some(event.clone());
            next.persist(path)?;
            *self = next;
            request["params"] = event;
            request["_skyreHostAuthorized"] = json!(true);
            Ok(())
        }
        fn bind(&self, request: &mut Value) {
            request["_skyreContext"] =
                json!({"sessionId":self.session_id,"turnId":self.turn_id,"mode":"current"});
        }
    }
    pub fn native_host(socket: &Path) -> Result<()> {
        socket_check(socket)?;
        let mut stream = UnixStream::connect(socket)?;
        peer(&stream)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        crate::protocol::write_frame(&mut stream, &json!({"skyreExtensionProtocol":1}))?;
        if crate::protocol::read_frame(&mut stream)? != Some(json!({"ready":true})) {
            return Err(Error::action("Extension bridge handshake failed"));
        }
        stream.set_read_timeout(None)?;
        let socket_writer = std::sync::Arc::new(std::sync::Mutex::new(stream.try_clone()?));
        let stdout = std::sync::Arc::new(std::sync::Mutex::new(std::io::stdout()));
        let assets = std::sync::Arc::new(std::sync::Mutex::new(super::assets::Assets::new(
            socket.parent().unwrap(),
        )));
        let input_socket = socket_writer.clone();
        let input_stdout = stdout.clone();
        let input_assets = assets.clone();
        let input = std::thread::spawn(move || -> Result<()> {
            let mut input = std::io::stdin().lock();
            let mut chunks = Chunks::default();
            let result = (|| {
                while let Some(value) = crate::protocol::read_frame(&mut input)? {
                    if let Some(value) = chunks.accept(value)? {
                        if let Some(response) = input_assets.lock().unwrap().dispatch(&value) {
                            let mut output = input_stdout.lock().unwrap();
                            for part in split_message(&response)? {
                                crate::protocol::write_frame(&mut *output, &part)?;
                            }
                            output.flush()?;
                        } else {
                            crate::protocol::write_frame(
                                &mut *input_socket.lock().unwrap(),
                                &value,
                            )?;
                        }
                    }
                }
                Ok(())
            })();
            input_assets.lock().unwrap().shutdown();
            let _ = input_socket
                .lock()
                .unwrap()
                .shutdown(std::net::Shutdown::Write);
            result
        });
        let result = (|| {
            while let Some(value) = crate::protocol::read_frame(&mut stream)? {
                if let Some(response) = assets.lock().unwrap().dispatch(&value) {
                    crate::protocol::write_frame(&mut *socket_writer.lock().unwrap(), &response)?;
                } else {
                    let mut output = stdout.lock().unwrap();
                    for part in split_message(&value)? {
                        crate::protocol::write_frame(&mut *output, &part)?;
                    }
                    output.flush()?;
                }
            }
            Ok(())
        })();
        assets.lock().unwrap().shutdown();
        // Chrome owns stdin lifetime. Do not block forever joining its read on remote EOF.
        if input.is_finished() {
            input
                .join()
                .map_err(|_| Error::action("Native messaging input worker failed"))??;
        }
        result
    }
    struct SocketGuard {
        path: std::path::PathBuf,
        inode: u64,
    }
    impl Drop for SocketGuard {
        fn drop(&mut self) {
            if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
    // tungstenite requires the unboxed HTTP ErrorResponse in its upgrade callback.
    #[allow(clippy::result_large_err)]
    pub fn serve(listen: &str, socket: &Path) -> Result<()> {
        let address: SocketAddr = listen.parse().map_err(|_| {
            Error::invalid("Bridge listen address must be an explicit loopback IP:port")
        })?;
        if !address.ip().is_loopback() {
            return Err(Error::invalid("Extension bridge must listen on loopback"));
        }
        let parent = socket
            .parent()
            .ok_or_else(|| Error::invalid("Socket requires parent directory"))?;
        private_directory(parent)?;
        if fs::symlink_metadata(socket).is_ok() {
            socket_check(socket)?;
            let inode = fs::symlink_metadata(socket)?.ino();
            match UnixStream::connect(socket) {
                Ok(_) => return Err(Error::action("Extension bridge socket is already active")),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    if fs::symlink_metadata(socket)?.ino() != inode {
                        return Err(Error::action("Extension socket changed during restart"));
                    }
                    fs::remove_file(socket)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        let native_listener = UnixListener::bind(socket)?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        let _guard = SocketGuard {
            path: socket.into(),
            inode: fs::symlink_metadata(socket)?.ino(),
        };
        native_listener.set_nonblocking(true)?;
        let websocket_listener = TcpListener::bind(address)?;
        let token = token()?;
        let mut owner_name = socket.as_os_str().to_os_string();
        owner_name.push(".owner.json");
        let owner_path = std::path::PathBuf::from(owner_name);
        let mut owner = OwnerContext::load(&owner_path)?;
        println!(
            "{}",
            json!({"endpoint":format!("ws://{}/{token}?skyre-provider=extension",websocket_listener.local_addr()?),"socket":socket,"protocol":"skyre-extension-1"})
        );
        loop {
            let (tcp, _) = websocket_listener.accept()?;
            tcp.set_read_timeout(Some(Duration::from_secs(5)))?;
            tcp.set_write_timeout(Some(Duration::from_secs(5)))?;
            let expected = format!("/{token}");
            let config = tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(crate::protocol::MAX_FRAME))
                .max_frame_size(Some(crate::protocol::MAX_FRAME));
            let accepted = tungstenite::accept_hdr_with_config(
                tcp,
                move |request: &Request,
                      response: Response|
                      -> std::result::Result<Response, ErrorResponse> {
                    if request.uri().path() != expected || request.headers().contains_key("origin")
                    {
                        return Err(tungstenite::http::Response::builder()
                            .status(403)
                            .body(Some("Forbidden".into()))
                            .unwrap());
                    }
                    Ok(response)
                },
                Some(config),
            );
            let Ok(mut websocket) = accepted else {
                continue;
            };
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut native = None;
            while Instant::now() < deadline {
                match native_listener.accept() {
                    Ok((mut stream, _)) => {
                        // Darwin propagates listener O_NONBLOCK to accepted Unix sockets.
                        stream.set_nonblocking(false)?;
                        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                        if peer(&stream).is_ok()
                            && crate::protocol::read_frame(&mut stream).ok().flatten()
                                == Some(json!({"skyreExtensionProtocol":1}))
                        {
                            if crate::protocol::write_frame(&mut stream, &json!({"ready":true}))
                                .is_err()
                            {
                                continue;
                            }
                            stream.set_read_timeout(None)?;
                            native = Some(stream);
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            let Some(mut native) = native else {
                let _ = websocket.close(None);
                continue;
            };
            let mut reader = native.try_clone()?;
            let (tx, rx) = mpsc::sync_channel(256);
            let queued = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let writer_queued = queued.clone();
            let worker = std::thread::spawn(move || {
                loop {
                    match crate::protocol::read_frame(&mut reader) {
                        Ok(Some(value)) => {
                            let size = value.to_string().len();
                            if writer_queued
                                .fetch_update(
                                    std::sync::atomic::Ordering::AcqRel,
                                    std::sync::atomic::Ordering::Acquire,
                                    |n| {
                                        n.checked_add(size)
                                            .filter(|n| *n <= crate::protocol::MAX_FRAME)
                                    },
                                )
                                .is_err()
                            {
                                let _ = tx.send(Err(Error::action(
                                    "Extension event queue exceeds 8 MiB",
                                )));
                                break;
                            }
                            if tx.send(Ok((value, size))).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {
                            let _ = tx.send(Err(Error::action("Extension disconnected")));
                            break;
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            break;
                        }
                    }
                }
            });
            websocket
                .get_mut()
                .set_read_timeout(Some(Duration::from_millis(25)))?;
            'connected: loop {
                loop {
                    match rx.try_recv() {
                        Ok(Ok((value, size))) => {
                            queued.fetch_sub(size, std::sync::atomic::Ordering::AcqRel);
                            if websocket.send(Message::text(value.to_string())).is_err() {
                                break 'connected;
                            }
                        }
                        Ok(Err(error)) => {
                            eprintln!("Extension peer ended: {}", error.message);
                            break 'connected;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => break 'connected,
                        Err(mpsc::TryRecvError::Empty) => break,
                    }
                }
                match websocket.read() {
                    Ok(Message::Text(text)) => {
                        let mut value: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => break,
                        };
                        if !value["method"].is_string() || !value["id"].is_u64() {
                            break;
                        }
                        value
                            .as_object_mut()
                            .unwrap()
                            .remove("_skyreHostAuthorized");
                        if value["method"] == "Skyre.hostLifecycle"
                            && let Err(error) = owner.host_turn(&owner_path, &mut value)
                        {
                            let response = json!({"id":value["id"],"error":{"code":error.code,"message":error.message}});
                            if websocket.send(Message::text(response.to_string())).is_err() {
                                break;
                            }
                            continue;
                        }
                        if value["method"] == "Skyre.beginTurn" {
                            // The capability holder requests a new turn; it cannot
                            // choose or impersonate another persisted session ID.
                            if let Err(error) = owner.next_turn(&owner_path) {
                                let response = json!({"id":value["id"],"error":{"code":error.code,"message":error.message}});
                                if websocket.send(Message::text(response.to_string())).is_err() {
                                    break;
                                }
                                continue;
                            }
                        }
                        owner.bind(&mut value);
                        if crate::protocol::write_frame(&mut native, &value).is_err() {
                            break;
                        }
                    }
                    Ok(Message::Ping(bytes)) => {
                        if websocket.send(Message::Pong(bytes)).is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => {
                        eprintln!("Extension WebSocket ended: {error}");
                        break;
                    }
                }
            }
            let _ = websocket.close(None);
            let _ = native.shutdown(std::net::Shutdown::Both);
            drop(rx);
            let _ = worker.join();
        }
    }
    pub fn prepare_manifest(
        destination: &Path,
        executable: &Path,
        socket: &Path,
        extension_id: &str,
    ) -> Result<serde_json::Value> {
        if extension_id.len() != 32 || !extension_id.bytes().all(|c| (b'a'..=b'p').contains(&c)) {
            return Err(Error::invalid(
                "Chrome extension ID must contain 32 letters a-p",
            ));
        }
        if !executable.is_absolute() || !executable.is_file() || !socket.is_absolute() {
            return Err(Error::invalid(
                "Executable and socket paths must be absolute",
            ));
        }
        private_directory(destination)?;
        fn quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
        let wrapper = destination.join("skyre-native-host");
        let manifest = destination.join("org.skyre.bridge.json");
        let script = format!(
            "#!/bin/sh\nexec {} extension-host --socket {}\n",
            quote(&executable.to_string_lossy()),
            quote(&socket.to_string_lossy())
        );
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700);
        let mut file = options.open(&wrapper)?;
        file.write_all(script.as_bytes())?;
        file.sync_all()?;
        let definition = json!({"name":"org.skyre.bridge","description":"Independent Skyre computer-use bridge","path":wrapper,"type":"stdio","allowed_origins":[format!("chrome-extension://{extension_id}/")]});
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        match options.open(&manifest).and_then(|mut f| {
            f.write_all(
                serde_json::to_string_pretty(&definition)
                    .unwrap()
                    .as_bytes(),
            )?;
            f.sync_all()
        }) {
            Ok(()) => {}
            Err(e) => {
                let _ = fs::remove_file(wrapper);
                return Err(e.into());
            }
        }
        Ok(json!({"manifest":manifest,"wrapper":wrapper,"definition":definition}))
    }
}
#[cfg(unix)]
pub use unix::{native_host, prepare_manifest, serve};
#[cfg(not(unix))]
pub fn serve(_: &str, _: &Path) -> Result<()> {
    Err(Error::unsupported(
        "Native extension transport requires Unix sockets on this build",
    ))
}
#[cfg(not(unix))]
pub fn native_host(_: &Path) -> Result<()> {
    Err(Error::unsupported(
        "Native extension transport requires Unix sockets on this build",
    ))
}
#[cfg(not(unix))]
pub fn prepare_manifest(_: &Path, _: &Path, _: &Path, _: &str) -> Result<serde_json::Value> {
    Err(Error::unsupported(
        "Native extension transport requires Unix sockets on this build",
    ))
}

use base64::Engine;
use serde_json::{Value, json};
const NATIVE_CHUNK: usize = 256 * 1024;
fn split_message(value: &Value) -> Result<Vec<Value>> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > crate::protocol::MAX_FRAME {
        return Err(Error::action("Native messaging message exceeds 8 MiB"));
    }
    if bytes.len() <= 512 * 1024 {
        return Ok(vec![value.clone()]);
    }
    static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let total = bytes.len().div_ceil(NATIVE_CHUNK);
    Ok(bytes.chunks(NATIVE_CHUNK).enumerate().map(|(part,data)|json!({"__skyre_chunk":{"id":id,"part":part,"total":total,"data":base64::engine::general_purpose::STANDARD.encode(data)}})).collect())
}
#[derive(Default)]
struct Chunks {
    pending: Option<(u64, usize, usize, Vec<u8>, std::time::Instant)>,
}
impl Chunks {
    fn accept(&mut self, value: Value) -> Result<Option<Value>> {
        let Some(chunk) = value.get("__skyre_chunk") else {
            if self.pending.is_some() {
                return Err(Error::action("Interleaved native chunk message"));
            }
            return Ok(Some(value));
        };
        let id = chunk["id"]
            .as_u64()
            .ok_or_else(|| Error::invalid("Chunk ID missing"))?;
        let part = chunk["part"]
            .as_u64()
            .ok_or_else(|| Error::invalid("Chunk part missing"))? as usize;
        let total = chunk["total"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 64)
            .ok_or_else(|| Error::invalid("Invalid chunk count"))? as usize;
        if self.pending.is_none() {
            if part != 0 {
                return Err(Error::invalid("Chunk stream does not begin at zero"));
            }
            self.pending = Some((id, total, 0, vec![], std::time::Instant::now()));
        }
        let current = self.pending.as_mut().unwrap();
        if current.0 != id
            || current.1 != total
            || current.2 != part
            || current.4.elapsed() > std::time::Duration::from_secs(10)
        {
            return Err(Error::invalid("Stale or out-of-order native chunks"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(
                chunk["data"]
                    .as_str()
                    .ok_or_else(|| Error::invalid("Chunk data missing"))?,
            )
            .map_err(|_| Error::invalid("Invalid chunk base64"))?;
        if bytes.len() > NATIVE_CHUNK || current.3.len() + bytes.len() > crate::protocol::MAX_FRAME
        {
            return Err(Error::invalid("Native chunk byte budget exceeded"));
        }
        current.3.extend(bytes);
        current.2 += 1;
        if current.2 == total {
            let current = self.pending.take().unwrap();
            return Ok(Some(serde_json::from_slice(&current.3)?));
        }
        Ok(None)
    }
}
#[cfg(test)]
mod chunk_tests {
    use super::*;
    #[test]
    fn native_messages_cross_chrome_limit_without_splitting_unicode() {
        let value = json!({"payload":"α🧪".repeat(300000)});
        let parts = split_message(&value).unwrap();
        assert!(parts.len() > 2);
        assert!(parts.iter().all(|v| v.to_string().len() < 1024 * 1024));
        let mut decoder = Chunks::default();
        let mut result = None;
        for part in parts {
            result = decoder.accept(part).unwrap();
        }
        assert_eq!(result, Some(value));
    }
    #[test]
    fn native_chunks_reject_missing_and_replayed_parts() {
        let parts = split_message(&json!({"payload":"x".repeat(700000)})).unwrap();
        assert!(Chunks::default().accept(parts[1].clone()).is_err());
        let mut d = Chunks::default();
        assert!(d.accept(parts[0].clone()).unwrap().is_none());
        assert!(d.accept(parts[0].clone()).is_err());
    }
}
