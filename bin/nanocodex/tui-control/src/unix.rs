use super::*;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::JoinHandle,
};

pub struct Server {
    pub bridge: Bridge,
    pub commands: mpsc::Receiver<Command>,
    tasks: Vec<JoinHandle<()>>,
    registration: PathBuf,
    _runtime: tempfile::TempDir,
    stop: watch::Sender<bool>,
}

fn private_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "TUI directory must be owned by this user with mode 0700",
        ));
    }
    Ok(())
}

fn prepare_registry() -> io::Result<PathBuf> {
    let directory = registry_dir()?;
    let home = directory.ancestors().nth(3).unwrap();
    fs::create_dir_all(home)?;
    let parent = home.join("nanocodex");
    fs::create_dir_all(&parent)?;
    let meta = fs::symlink_metadata(&parent)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        return Err(io::Error::other("untrusted nanocodex state directory"));
    }
    for path in [home.join("nanocodex/tui"), directory.clone()] {
        private_dir(&path)?;
    }
    Ok(directory)
}

fn write_registration(path: &Path, registration: &Registration) -> io::Result<()> {
    let parent = path.parent().unwrap();
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, registration)?;
    file.flush()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

impl Server {
    pub fn enabled() -> bool {
        std::env::var("NANOCODEX_TUI_CONTROL").as_deref() != Ok("off")
    }

    pub fn start(backend: &str) -> io::Result<Self> {
        let registry = prepare_registry()?;
        let runtime = tempfile::Builder::new()
            .prefix("nc-tui-")
            .tempdir_in("/tmp")?;
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700))?;
        let socket_path = runtime.path().join("control.sock");
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        let registration = Registration {
            protocol_version: VERSION,
            instance_id: uuid::Uuid::new_v4().to_string(),
            pid: std::process::id(),
            started_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            backend: backend.into(),
            socket_path,
            auth_token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            active_generation: "0".into(),
            active_session_id: None,
            conversation: None,
        };
        let path = registry.join(format!("{}.json", registration.instance_id));
        let (tx, commands) = mpsc::channel(32);
        let bridge = Bridge::new(registration.clone(), tx)?;
        write_registration(&path, &registration)?;
        let (stop, _) = watch::channel(false);
        let mut stopped = stop.subscribe();
        let owner = bridge.clone();
        let connections = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    Some(_) = clients.join_next(), if !clients.is_empty() => {},
                    result = listener.accept() => match result {
                        Ok((socket,_)) if clients.len() < 32 => {
                            let bridge = owner.clone();
                            clients.spawn(async move { let _ = serve(socket, bridge).await; });
                        }
                        Ok(_) => {},
                        Err(_) => break,
                    }
                }
            }
        });
        let mut changed = bridge.registration_changed.subscribe();
        let registration_path = path.clone();
        let updates = tokio::spawn(async move {
            while changed.changed().await.is_ok() {
                let value = changed.borrow_and_update().clone();
                if write_registration(&registration_path, &value).is_err() {
                    // Never leave a stale discovery file advertising an available owner.
                    let _ = fs::remove_file(&registration_path);
                }
            }
        });
        Ok(Self {
            bridge,
            commands,
            tasks: vec![connections, updates],
            registration: path,
            _runtime: runtime,
            stop,
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        for task in &self.tasks {
            task.abort();
        }
        let _ = fs::remove_file(&self.registration);
    }
}

async fn line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    read_frame(reader, &mut Vec::new()).await
}

async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
) -> io::Result<Option<Vec<u8>>> {
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::other("incomplete frame"))
            };
        }
        let end = chunk.iter().position(|b| *b == b'\n').map(|p| p + 1);
        let len = end.unwrap_or(chunk.len());
        if bytes.len() + len > MAX_FRAME {
            return Err(io::Error::other("frame too large"));
        }
        bytes.extend_from_slice(&chunk[..len]);
        reader.consume(len);
        if end.is_some() {
            return Ok(Some(std::mem::take(bytes)));
        }
    }
}

async fn send<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_FRAME {
        bytes =
            serde_json::to_vec(&json!({"id":value["id"],"result":rejected("response_too_large")}))?;
    }
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(10), writer.write_all(&bytes))
        .await
        .map_err(|_| io::Error::other("subscriber too slow"))??;
    writer.flush().await
}

async fn serve(socket: UnixStream, bridge: Bridge) -> io::Result<()> {
    if socket.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other("wrong peer owner"));
    }
    let (read, mut write) = socket.into_split();
    let mut read = BufReader::new(read);
    let hello = tokio::time::timeout(Duration::from_secs(5), line(&mut read))
        .await
        .map_err(|_| io::Error::other("authentication timeout"))??
        .ok_or_else(|| io::Error::other("missing hello"))?;
    let hello: Value = serde_json::from_slice(&hello)?;
    let registration = bridge.registration();
    if hello["protocol_version"] != VERSION
        || hello["instance_id"] != registration.instance_id
        || hello["auth_token"] != registration.auth_token
    {
        return Err(io::Error::other("authentication failed"));
    }
    send(
        &mut write,
        &json!({"type":"hello","protocol_version":VERSION,"snapshot":bridge.snapshot()}),
    )
    .await?;
    let mut changed = bridge.changed.subscribe();
    let mut cursor = None;
    let mut incoming = Vec::new();
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    let mut replies = tokio::task::JoinSet::new();
    let mut reading = true;
    loop {
        if !reading && replies.is_empty() {
            return Ok(());
        }
        if let Some(after) = cursor {
            match bridge.replay(after) {
                Ok(events) => {
                    for event in events {
                        cursor = event["seq"].as_str().and_then(|s| s.parse().ok());
                        send(&mut write, &event).await?;
                    }
                }
                Err(gap) => {
                    send(&mut write, &json!({"type":"replay_gap","data":gap})).await?;
                    cursor = None;
                }
            }
        }
        tokio::select! {
            Some(reply) = replies.join_next(), if !replies.is_empty() => {
                if let Ok((id, value)) = reply {
                    send(&mut write, &json!({"id":id,"result":value})).await?;
                }
            },
            _ = changed.changed(), if cursor.is_some() => {},
            bytes = read_frame(&mut read, &mut incoming), if reading => {
                let Some(bytes) = bytes? else { reading = false; continue; };
                let request: Request = serde_json::from_slice(&bytes)?;
                if request.method == "events.subscribe" {
                    let parsed = request.params["after_seq"].as_str().and_then(|s| s.parse::<u64>().ok());
                    if let Some(after) = parsed {
                        cursor = Some(after);
                        send(&mut write,&json!({"id":request.id,"result":{"subscribed":true}})).await?;
                    } else { send(&mut write,&json!({"id":request.id,"result":rejected("invalid_cursor")})).await?; }
                } else {
                    // Reserve two slots for cancellation even during slow history reads.
                    if request.method != "cancel" && (slots.available_permits() <= 2 || bridge.inflight.available_permits() <= 2) {
                        send(&mut write, &json!({"id":request.id,"result":rejected("connection_busy")})).await?;
                        continue;
                    }
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        send(&mut write, &json!({"id":request.id,"result":rejected("connection_busy")})).await?;
                        continue;
                    };
                    let Ok(global_permit) = bridge.inflight.clone().try_acquire_owned() else {
                        send(&mut write, &json!({"id":request.id,"result":rejected("server_busy")})).await?;
                        continue;
                    };
                    let id = request.id.clone();
                    let owner = bridge.clone();
                    // Disconnect/timeout drops only the waiter. Admitted work keeps its slot
                    // until it actually resolves and records its receipt.
                    let task = tokio::spawn(async move {
                        let _permit = permit;
                        let _global_permit = global_permit;
                        owner.dispatch(request).await
                    });
                    replies.spawn(async move {
                        let value = match tokio::time::timeout(Duration::from_secs(30), task).await {
                            Ok(Ok(value)) => value,
                            _ => json!({"status":"pending"}),
                        };
                        (id, value)
                    });
                }
            }
        }
    }
}

fn read_registration(path: &Path) -> io::Result<Registration> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > 16384
    {
        return Err(io::Error::other("untrusted registration"));
    }
    Ok(serde_json::from_reader(file)?)
}

pub fn list() -> io::Result<Vec<Registration>> {
    let directory = prepare_registry()?;
    let mut values = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        if let Ok(mut value) = read_registration(&entry.path()) {
            value.auth_token.clear();
            values.push(value);
        }
    }
    values.sort_by_key(|v| v.started_at_unix_ms);
    Ok(values)
}

pub async fn connect(instance: &str) -> io::Result<()> {
    uuid::Uuid::parse_str(instance).map_err(|_| io::Error::other("invalid instance ID"))?;
    let registration = read_registration(&prepare_registry()?.join(format!("{instance}.json")))?;
    if registration.instance_id != instance {
        return Err(io::Error::other("instance mismatch"));
    }
    let socket = UnixStream::connect(&registration.socket_path).await?;
    if socket.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other("wrong socket owner"));
    }
    let (mut read, mut write) = socket.into_split();
    send(&mut write,&json!({"protocol_version":VERSION,"instance_id":instance,"auth_token":registration.auth_token})).await?;
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();
    tokio::try_join!(
        async {
            tokio::io::copy(&mut input, &mut write).await?;
            write.shutdown().await
        },
        async {
            tokio::io::copy(&mut read, &mut output).await?;
            output.flush().await
        }
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn bridge() -> Bridge {
        let (tx, _rx) = mpsc::channel(1);
        Bridge::new(
            Registration {
                protocol_version: VERSION,
                instance_id: "test".into(),
                pid: 1,
                started_at_unix_ms: 0,
                backend: "test".into(),
                socket_path: "/unused".into(),
                auth_token: "secret".into(),
                active_generation: "0".into(),
                active_session_id: None,
                conversation: None,
            },
            tx,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn socket_requires_authentication_before_exposing_state() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let owner = tokio::spawn(serve(server, bridge()));
        client
            .write_all(
                b"{\"protocol_version\":1,\"instance_id\":\"test\",\"auth_token\":\"wrong\"}\n",
            )
            .await
            .unwrap();
        let mut output = Vec::new();
        client.read_to_end(&mut output).await.unwrap();
        assert!(output.is_empty());
        assert!(owner.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn socket_replays_then_streams_without_an_attachment_gap() {
        let bridge = bridge();
        bridge.publish("before", json!(1));
        let (client, server) = UnixStream::pair().unwrap();
        let owner = tokio::spawn(serve(server, bridge.clone()));
        let (read, mut write) = client.into_split();
        let mut read = BufReader::new(read);
        send(
            &mut write,
            &json!({"protocol_version":1,"instance_id":"test","auth_token":"secret"}),
        )
        .await
        .unwrap();
        let hello: Value =
            serde_json::from_slice(&line(&mut read).await.unwrap().unwrap()).unwrap();
        assert_eq!(hello["snapshot"]["seq"], "1");
        send(
            &mut write,
            &json!({"id":"subscribe","method":"events.subscribe","params":{"after_seq":"0"}}),
        )
        .await
        .unwrap();
        let _ = line(&mut read).await.unwrap();
        bridge.publish("after", json!(2));
        for expected in ["1", "2"] {
            let event: Value = serde_json::from_slice(
                &tokio::time::timeout(Duration::from_secs(2), line(&mut read))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(event["seq"], expected);
        }
        owner.abort();
    }

    #[tokio::test]
    async fn pending_history_does_not_block_events_or_cancel_and_disconnect_keeps_receipts() {
        let (tx, mut commands) = mpsc::channel(32);
        let bridge = Bridge::new(bridge().registration(), tx).unwrap();
        bridge.state(Some("session"), json!({"connection":"ready"}));
        let (client, server) = UnixStream::pair().unwrap();
        let owner = tokio::spawn(serve(server, bridge.clone()));
        let (read, mut write) = client.into_split();
        let mut read = BufReader::new(read);
        send(
            &mut write,
            &json!({"protocol_version":1,"instance_id":"test","auth_token":"secret"}),
        )
        .await
        .unwrap();
        let hello: Value =
            serde_json::from_slice(&line(&mut read).await.unwrap().unwrap()).unwrap();
        send(&mut write, &json!({"id":"sub","method":"events.subscribe","params":{"after_seq":hello["snapshot"]["seq"]}})).await.unwrap();
        line(&mut read).await.unwrap();
        send(&mut write, &json!({"id":"slow","method":"history.list"}))
            .await
            .unwrap();
        let slow = commands.recv().await.unwrap();
        bridge.publish("progress", json!({"text":"still streaming"}));
        let event: Value = serde_json::from_slice(
            &tokio::time::timeout(Duration::from_secs(1), line(&mut read))
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(event["type"], "progress");
        send(&mut write, &json!({"id":"stop","method":"cancel","params":{
            "expected_instance_id":"test","expected_session_id":"session","expected_active_generation":"1","expected_turn_id":"turn"}})).await.unwrap();
        let cancel = tokio::time::timeout(Duration::from_secs(1), commands.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancel.request.method, "cancel");
        // A lost connection must not undo an already dispatched cancellation.
        drop(write);
        drop(read);
        cancel.finish(accepted(json!({"turn_id":"turn"})));
        slow.finish(json!({"records":[]}));
        let _ = owner.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let value = bridge
                    .dispatch(Request {
                        id: "status".into(),
                        method: "request.get".into(),
                        params: json!({"request_id":"stop"}),
                    })
                    .await;
                if value["status"] == "accepted" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn stdin_half_close_drains_pending_replies() {
        let (tx, mut commands) = mpsc::channel(32);
        let bridge = Bridge::new(bridge().registration(), tx).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let owner = tokio::spawn(serve(server, bridge));
        let (read, mut write) = client.into_split();
        let mut read = BufReader::new(read);
        send(
            &mut write,
            &json!({"protocol_version":1,"instance_id":"test","auth_token":"secret"}),
        )
        .await
        .unwrap();
        line(&mut read).await.unwrap();
        send(&mut write, &json!({"id":"history","method":"history.list"}))
            .await
            .unwrap();
        write.shutdown().await.unwrap();
        let command = commands.recv().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), line(&mut read))
                .await
                .is_err(),
            "half-close must wait for the pending reply"
        );
        command.finish(json!({"records":["saved"]}));
        let response: Value = serde_json::from_slice(
            &tokio::time::timeout(Duration::from_secs(1), line(&mut read))
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["id"], "history");
        assert_eq!(response["result"]["records"], json!(["saved"]));
        assert!(line(&mut read).await.unwrap().is_none());
        owner.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn frame_limit_applies_before_allocating_unbounded_input() {
        let bytes = vec![b'x'; MAX_FRAME + 1];
        assert!(line(&mut BufReader::new(bytes.as_slice())).await.is_err());
    }

    #[test]
    fn registration_rejects_symlinks_and_public_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance.json");
        write_registration(&path, &bridge().registration()).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(read_registration(&path).is_ok());
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_registration(&link).is_err());
        fs::set_permissions(path.clone(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_registration(&path).is_err());
    }
}
