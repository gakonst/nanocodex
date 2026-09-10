//! Private Linux desktop infrastructure and a bounded, trusted-host command channel.
//!
//! `Xvfb`, `openbox`, and `xterm` are the only external desktop processes. Capture
//! and input use the Rust X11 protocol client directly. Pointer coordinates are
//! normalized to `0..=1`; buttons are `0 = left`, `1 = right`, `2 = middle`, and
//! keys are USB HID keyboard-page usages, matching the remote Hand protocol.
//! The publisher owns generation/control authorization. Each Unix connection
//! carries one JSON line. A logical viewer disconnect must send `release` (or
//! `disconnect`); raw held input also expires after five seconds without input.

mod input;
mod pixels;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ExtendedColorType, codecs::jpeg::JpegEncoder, imageops::FilterType};
use input::{Action, Input, parse};
use nix::{
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::{
        prctl,
        signal::{Signal, kill, killpg},
        wait::{WaitPidFlag, waitpid},
    },
    unistd::{Pid, getuid},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{self, ConnectionExt as _, ImageFormat, ImageOrder, VisualClass},
        xtest::ConnectionExt as _,
    },
    rust_connection::{DefaultStream, PollMode, RustConnection, Stream},
    utils::RawFdContainer,
};

/// A desktop protocol, infrastructure, I/O, or encoding failure.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;
const REQUEST_LIMIT: usize = 8192;
const RESPONSE_LIMIT: usize = 510_000;
const FRAME_LIMIT: usize = 500_000;
const OP_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const WIDTH: u16 = 1280;
const HEIGHT: u16 = 800;

fn invalid(message: impl Into<String>) -> Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into()).into()
}
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "desktop operation timed out")
}

/// Start an isolated X11 desktop and serve `runtime/hand.sock` until shutdown.
///
/// Creates a private (0700) runtime directory and a 0600 command socket, and
/// writes `ready` only after capture and XTEST are usable. Existing sockets are
/// never unlinked. `shutdown`, SIGTERM, SIGINT, or cancellation release input,
/// terminate all owned process groups, reap children, and remove owned files.
pub async fn serve(workspace: PathBuf, runtime: PathBuf) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    struct Cancel(Arc<AtomicBool>);
    impl Drop for Cancel {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let cancel = Cancel(stop.clone());
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut worker =
        tokio::task::spawn_blocking(move || serve_blocking(&workspace, &runtime, stop));
    let result = tokio::select! {
        result = &mut worker => result?,
        _ = terminate.recv() => { drop(cancel); return worker.await?; },
        _ = interrupt.recv() => { drop(cancel); return worker.await?; },
    };
    drop(cancel);
    result
}

/// Send one bounded JSON request over the private desktop command socket.
///
/// Agent actions return `{status:"ok",jpeg,width,height}`; raw input, release,
/// and shutdown return `{status:"ok"}`. Errors return `{status:"error",error}`.
/// This is a synchronous local IPC call; async callers can use `spawn_blocking`.
pub fn request(runtime: &Path, input: Value) -> Result<Value> {
    parse(input.clone())?;
    let mut bytes = serde_json::to_vec(&input)?;
    if bytes.len() > REQUEST_LIMIT {
        return Err(invalid("desktop request exceeds 8192 bytes"));
    }
    bytes.push(b'\n');
    let mut stream = UnixStream::connect(runtime.join("hand.sock"))?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    stream.write_all(&bytes)?;
    let response = read_line(
        &mut stream,
        RESPONSE_LIMIT,
        Instant::now() + REQUEST_TIMEOUT,
        None,
    )?;
    Ok(serde_json::from_slice(&response)?)
}

// Use a deadline, not a per-read timeout, so a trickling peer cannot hold the
// serial input owner forever. Cancellation is checked at most 100 ms apart.
fn read_line(
    stream: &mut UnixStream,
    limit: usize,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        if stop.is_some_and(|s| s.load(Ordering::Acquire)) {
            return Err(invalid("desktop cancelled"));
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(timed_out)?;
        stream.set_read_timeout(Some(remaining.min(Duration::from_millis(100))))?;
        let count = match stream.read(&mut buffer) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            other => other?,
        };
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "desktop request disconnected",
            )
            .into());
        }
        if let Some(end) = buffer[..count].iter().position(|&b| b == b'\n') {
            if data.len() + end > limit {
                return Err(invalid("desktop message too large"));
            }
            if buffer[end + 1..count]
                .iter()
                .any(|b| !b.is_ascii_whitespace())
            {
                return Err(invalid("one request per connection required"));
            }
            data.extend_from_slice(&buffer[..end]);
            return Ok(data);
        }
        if data.len() + count > limit {
            return Err(invalid("desktop message too large"));
        }
        data.extend_from_slice(&buffer[..count]);
    }
}

struct Runtime {
    path: PathBuf,
    // Keep the inode and lock across restarts; unlinking lock files races waiters.
    _lock: File,
    socket: bool,
    auth: bool,
    ready: bool,
}
impl Runtime {
    fn claim(path: &Path) -> Result<Self> {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?,
            Err(e) => return Err(e.into()),
            Ok(_) => (),
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != getuid().as_raw() || metadata.mode() & 0o077 != 0
        {
            return Err(invalid(
                "desktop runtime must be an owned, private 0700 directory",
            ));
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path.join("lock"))?;
        lock.try_lock()
            .map_err(|_| invalid("desktop runtime is already in use"))?;
        if fs::symlink_metadata(path.join("hand.sock")).is_ok() {
            return Err(invalid(
                "desktop socket already exists; refusing to replace it",
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            _lock: lock,
            socket: false,
            auth: false,
            ready: false,
        })
    }
    fn write_private(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path.join(name))?;
        file.write_all(bytes)?;
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        for (owned, name) in [
            (self.ready, "ready"),
            (self.socket, "hand.sock"),
            (self.auth, "Xauthority"),
        ] {
            if owned {
                let _ = fs::remove_file(self.path.join(name));
            }
        }
    }
}

struct Children(Vec<Child>, bool);
impl Children {
    fn new() -> Result<Self> {
        let previous = prctl::get_child_subreaper()?;
        prctl::set_child_subreaper(true)?;
        Ok(Self(Vec::new(), previous))
    }
    fn spawn(&mut self, command: &mut Command) -> Result<usize> {
        let child = command
            .process_group(0)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        self.0.push(child);
        Ok(self.0.len() - 1)
    }
    fn alive(&mut self) -> Result<()> {
        for child in &mut self.0 {
            if let Some(status) = child.try_wait()? {
                return Err(
                    io::Error::other(format!("desktop infrastructure exited: {status}")).into(),
                );
            }
        }
        Ok(())
    }
}
impl Drop for Children {
    fn drop(&mut self) {
        // xterm's shell starts its own session. Track descendants as well as
        // direct process groups, and adopt/reap them when their parent exits.
        let mut descendants = BTreeSet::new();
        let mut pending: Vec<u32> = self.0.iter().map(Child::id).collect();
        while let Some(pid) = pending.pop() {
            if descendants.len() >= 4096 {
                break;
            }
            if let Ok(children) = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
                for child in children
                    .split_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok())
                {
                    if descendants.insert(child) {
                        pending.push(child);
                    }
                }
            }
        }
        for &pid in &descendants {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        for child in &self.0 {
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(750);
        while Instant::now() < deadline {
            if self
                .0
                .iter_mut()
                .all(|c| matches!(c.try_wait(), Ok(Some(_))))
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Kill groups even when the direct child has exited: xterm may have
        // started a shell or application that outlives the terminal process.
        for &pid in &descendants {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
        }
        for child in &mut self.0 {
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            let _ = child.wait();
        }
        for pid in descendants {
            let _ = waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::empty()));
        }
        let _ = prctl::set_child_subreaper(self.1);
    }
}

fn auth_record(cookie: &[u8]) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(&65535u16.to_be_bytes()); // FamilyWild, private cookie file.
    for field in [
        b"".as_slice(),
        b"".as_slice(),
        b"MIT-MAGIC-COOKIE-1".as_slice(),
        cookie,
    ] {
        record.extend_from_slice(&(field.len() as u16).to_be_bytes());
        record.extend_from_slice(field);
    }
    record
}

fn start_x(
    runtime: &mut Runtime,
    children: &mut Children,
    stop: &AtomicBool,
) -> Result<(RustConnection<TimedStream>, String)> {
    let mut cookie = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut cookie)?;
    runtime.write_private("Xauthority", &auth_record(&cookie))?;
    runtime.auth = true;
    let index = children.spawn(
        Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "1280x800x24",
                "-nolisten",
                "tcp",
                "-nolisten",
                "local",
                "-noreset",
                "-auth",
            ])
            .arg(runtime.path.join("Xauthority"))
            .stdout(Stdio::piped()),
    )?;
    let stdout = children.0[index]
        .stdout
        .as_mut()
        .ok_or_else(|| invalid("Xvfb display pipe unavailable"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut number = Vec::new();
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(invalid("desktop cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(timed_out().into());
        }
        let mut descriptors = [PollFd::new(stdout.as_fd(), PollFlags::POLLIN)];
        if poll(&mut descriptors, 100u16)? == 0 {
            continue;
        }
        let mut byte = [0];
        if stdout.read(&mut byte)? == 0 {
            return Err(invalid("Xvfb exited before publishing a display"));
        }
        if byte[0] == b'\n' {
            break;
        }
        if !byte[0].is_ascii_digit() || number.len() >= 5 {
            return Err(invalid("invalid Xvfb display number"));
        }
        number.push(byte[0]);
    }
    let number: u16 = std::str::from_utf8(&number)?.parse()?;
    // Xvfb's filesystem socket is owner-only; the abstract transport and TCP
    // are disabled. The random cookie additionally isolates same-host displays.
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{number}"));
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let stream = UnixStream::connect(socket)?;
    let stream = TimedStream::new(stream)?;
    let connection = RustConnection::connect_to_stream_with_auth_info(
        stream,
        0,
        b"MIT-MAGIC-COOKIE-1".to_vec(),
        cookie.to_vec(),
    )?;
    connection.xtest_get_version(2, 2)?.reply()?;
    Ok((connection, format!(":{number}")))
}

fn serve_blocking(workspace: &Path, runtime_path: &Path, stop: Arc<AtomicBool>) -> Result<()> {
    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err(invalid("desktop workspace must be a directory"));
    }
    let mut runtime = Runtime::claim(runtime_path)?;
    // Resolve relative runtime paths before changing child working directories.
    runtime.path = runtime.path.canonicalize()?;
    let listener = UnixListener::bind(runtime.path.join("hand.sock"))?;
    runtime.socket = true;
    fs::set_permissions(
        runtime.path.join("hand.sock"),
        fs::Permissions::from_mode(0o600),
    )?;
    listener.set_nonblocking(true)?;
    let mut children = Children::new()?;
    let (connection, display) = start_x(&mut runtime, &mut children, &stop)?;
    let mut desktop = Desktop::new(connection, stop.clone())?;
    for (program, args) in [
        ("openbox", vec!["--sm-disable"]),
        (
            "xterm",
            vec![
                "-u8",
                "-geometry",
                "100x30+40+40",
                "-title",
                "Nanocodex workspace",
            ],
        ),
    ] {
        children.spawn(
            Command::new(program)
                .args(args)
                .env("DISPLAY", &display)
                .env("XAUTHORITY", runtime.path.join("Xauthority"))
                .env("XDG_RUNTIME_DIR", &runtime.path)
                .env("LANG", "C.UTF-8")
                .env("LC_ALL", "C.UTF-8")
                .current_dir(&workspace)
                .stdout(Stdio::null()),
        )?;
        // Start the terminal after the window manager owns the display. Starting
        // both concurrently can leave a visible terminal without active focus.
        let property = if program == "openbox" {
            b"_NET_SUPPORTING_WM_CHECK".as_slice()
        } else {
            b"_NET_ACTIVE_WINDOW".as_slice()
        };
        let atom = desktop
            .connection
            .intern_atom(false, property)?
            .reply()?
            .atom;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut requested_focus = false;
        loop {
            children.alive()?;
            desktop.connection.stream().reset(OP_TIMEOUT);
            desktop.check_cancel()?;
            let reply = desktop
                .connection
                .get_property(false, desktop.root, atom, xproto::AtomEnum::WINDOW, 0, 1)?
                .reply()?;
            if reply
                .value32()
                .and_then(|mut values| values.next())
                .is_some_and(|window| window != 0)
            {
                break;
            }
            if program == "xterm" && !requested_focus {
                let clients = desktop
                    .connection
                    .intern_atom(false, b"_NET_CLIENT_LIST")?
                    .reply()?
                    .atom;
                let clients = desktop
                    .connection
                    .get_property(false, desktop.root, clients, xproto::AtomEnum::WINDOW, 0, 1)?
                    .reply()?;
                if let Some(focus) = clients
                    .value32()
                    .and_then(|mut values| values.next())
                    .filter(|window| *window != 0)
                {
                    let event = xproto::ClientMessageEvent::new(
                        32,
                        focus,
                        atom,
                        xproto::ClientMessageData::from([2, x11rb::CURRENT_TIME, 0, 0, 0]),
                    );
                    desktop
                        .connection
                        .send_event(
                            false,
                            desktop.root,
                            xproto::EventMask::SUBSTRUCTURE_REDIRECT
                                | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                            event,
                        )?
                        .check()?;
                    requested_focus = true;
                }
            }
            if Instant::now() >= deadline {
                return Err(invalid(format!(
                    "{program} did not publish {}",
                    String::from_utf8_lossy(property)
                )));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    desktop.capture()?;
    children.alive()?;
    runtime.write_private(
        "ready",
        &serde_json::to_vec(
            &json!({"status":"ready", "display":display,"width":WIDTH,"height":HEIGHT}),
        )?,
    )?;
    runtime.ready = true;
    let mut last_input = Instant::now();
    while !stop.load(Ordering::Acquire) {
        children.alive()?;
        if last_input.elapsed() > OP_TIMEOUT && desktop.has_held_input() {
            desktop.release()?;
        }
        let (mut stream, _) = match listener.accept() {
            Ok(peer) => peer,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let result = (|| -> Result<(Value, bool)> {
            let bytes = read_line(
                &mut stream,
                REQUEST_LIMIT,
                Instant::now() + Duration::from_secs(2),
                Some(&stop),
            )?;
            let action = parse(serde_json::from_slice(&bytes)?)?;
            let shutdown = matches!(action, Action::Shutdown {});
            if !matches!(action, Action::Observe {}) {
                last_input = Instant::now();
            }
            desktop.connection.stream().reset(OP_TIMEOUT);
            Ok((desktop.apply(action)?, shutdown))
        })();
        let (response, shutdown) = match result {
            Ok(value) => value,
            Err(error) => {
                desktop.release()?;
                (json!({"status":"error", "error":error.to_string()}), false)
            }
        };
        let mut bytes = serde_json::to_vec(&response)?;
        if bytes.len() > RESPONSE_LIMIT {
            return Err(invalid("desktop response too large"));
        }
        bytes.push(b'\n');
        if stream.write_all(&bytes).is_err() {
            desktop.release()?;
        }
        if shutdown {
            break;
        }
    }
    desktop.release()?;
    Ok(())
}

// The x11rb default transport has unbounded polling. Apply an absolute deadline
// to handshake, replies, and writes without a helper process or unsafe FFI.
struct TimedStream {
    inner: DefaultStream,
    deadline: Mutex<Instant>,
}
impl TimedStream {
    fn new(stream: UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: DefaultStream::from_unix_stream(stream)?.0,
            deadline: Mutex::new(Instant::now() + OP_TIMEOUT),
        })
    }
    fn reset(&self, duration: Duration) {
        *self.deadline.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now() + duration;
    }
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .checked_duration_since(Instant::now())
            .ok_or_else(timed_out)
    }
}
impl Stream for TimedStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        loop {
            let mut flags = PollFlags::empty();
            if mode.readable() {
                flags |= PollFlags::POLLIN;
            }
            if mode.writable() {
                flags |= PollFlags::POLLOUT;
            }
            let mut descriptors = [PollFd::new(self.inner.as_fd(), flags)];
            let timeout = PollTimeout::try_from(self.remaining()?.as_millis().max(1))
                .map_err(io::Error::other)?;
            match poll(&mut descriptors, timeout) {
                Ok(0) => return Err(timed_out()),
                Ok(_) => return Ok(()),
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(io::Error::from(error)),
            }
        }
    }
    fn read(&self, buf: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.remaining()?;
        self.inner.read(buf, fds)
    }
    fn write(&self, buf: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.remaining()?;
        self.inner.write(buf, fds)
    }
}

struct Desktop {
    connection: RustConnection<TimedStream>,
    root: u32,
    keys: BTreeSet<u8>,
    buttons: BTreeSet<u8>,
    stop: Arc<AtomicBool>,
}
impl Desktop {
    fn new(connection: RustConnection<TimedStream>, stop: Arc<AtomicBool>) -> Result<Self> {
        let screen = &connection.setup().roots[0];
        if screen.width_in_pixels != WIDTH || screen.height_in_pixels != HEIGHT {
            return Err(invalid("unexpected desktop dimensions"));
        }
        Ok(Self {
            root: screen.root,
            connection,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            stop,
        })
    }
    fn has_held_input(&self) -> bool {
        !self.keys.is_empty() || !self.buttons.is_empty()
    }
    fn check_cancel(&self) -> Result<()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(invalid("desktop cancelled"));
        }
        self.connection.stream().remaining()?;
        Ok(())
    }
    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<()> {
        self.connection
            .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)?
            .check()?;
        Ok(())
    }
    fn keycode(&mut self, code: u8, down: bool) -> Result<()> {
        // Track before issuing the event: a lost reply must still release it.
        if down {
            self.keys.insert(code);
        }
        self.fake(
            if down {
                xproto::KEY_PRESS_EVENT
            } else {
                xproto::KEY_RELEASE_EVENT
            },
            code,
            0,
            0,
        )?;
        if !down {
            self.keys.remove(&code);
        }
        Ok(())
    }
    fn button(&mut self, button: u8, down: bool) -> Result<()> {
        if down {
            self.buttons.insert(button);
        }
        self.fake(
            if down {
                xproto::BUTTON_PRESS_EVENT
            } else {
                xproto::BUTTON_RELEASE_EVENT
            },
            button,
            0,
            0,
        )?;
        if !down {
            self.buttons.remove(&button);
        }
        Ok(())
    }
    fn move_to(&self, x: f64, y: f64) -> Result<()> {
        self.fake(
            xproto::MOTION_NOTIFY_EVENT,
            0,
            (x * f64::from(WIDTH - 1)).round() as i16,
            (y * f64::from(HEIGHT - 1)).round() as i16,
        )
    }
    fn hid(&mut self, usage: u16, down: bool) -> Result<()> {
        let symbol = input::hid_keysym(usage).ok_or_else(|| invalid("unsupported HID key"))?;
        let setup = self.connection.setup();
        let map = self
            .connection
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
            .reply()?;
        let code = input::find_keycode(
            setup.min_keycode,
            map.keysyms_per_keycode,
            &map.keysyms,
            symbol,
        )
        .ok_or_else(|| invalid(format!("HID key {usage} unavailable in X keyboard mapping")))?;
        self.keycode(code, down)
    }
    fn release(&mut self) -> Result<()> {
        self.connection.stream().reset(Duration::from_secs(1));
        let mut failure = None;
        for code in self.keys.clone() {
            if let Err(error) = self.keycode(code, false) {
                failure = Some(error);
            }
        }
        for button in self.buttons.clone() {
            if let Err(error) = self.button(button, false) {
                failure = Some(error);
            }
        }
        self.connection.flush()?;
        self.connection.stream().reset(OP_TIMEOUT);
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    fn text(&mut self, text: &str) -> Result<()> {
        // Xlib resolves keycodes asynchronously. Rewriting one spare keycode
        // per character races the receiving application, regardless of delay.
        // Transfer UTF-8 as an X selection instead: the receiver owns the
        // property containing the complete text before SelectionNotify arrives.
        use x11rb::{protocol::Event, wrapper::ConnectionExt as _};
        self.release()?;
        if text.is_empty() {
            return Ok(());
        }
        let atom = |name: &[u8]| -> Result<u32> {
            Ok(self.connection.intern_atom(false, name)?.reply()?.atom)
        };
        let utf8 = atom(b"UTF8_STRING")?;
        let targets = atom(b"TARGETS")?;
        let clipboard = atom(b"CLIPBOARD")?;
        let text_atom = atom(b"TEXT")?;
        let primary = u32::from(xproto::AtomEnum::PRIMARY);
        let window = self.connection.generate_id()?;
        self.connection
            .create_window(
                0,
                window,
                self.root,
                0,
                0,
                1,
                1,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                0,
                &xproto::CreateWindowAux::new(),
            )?
            .check()?;
        let result = (|| -> Result<()> {
            for selection in [primary, clipboard] {
                self.connection
                    .set_selection_owner(window, selection, x11rb::CURRENT_TIME)?
                    .check()?;
                if self
                    .connection
                    .get_selection_owner(selection)?
                    .reply()?
                    .owner
                    != window
                {
                    return Err(invalid("failed to acquire text selection"));
                }
            }
            self.hid(225, true)?;
            self.hid(73, true)?;
            self.hid(73, false)?;
            self.hid(225, false)?;
            loop {
                self.check_cancel()?;
                let Some(event) = self.connection.poll_for_event()? else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                };
                let Event::SelectionRequest(request) = event else {
                    continue;
                };
                if request.owner != window || ![primary, clipboard].contains(&request.selection) {
                    continue;
                }
                let property = if request.property == 0 {
                    request.target
                } else {
                    request.property
                };
                let is_text = request.target == utf8 || request.target == text_atom;
                let accepted = if request.target == targets {
                    self.connection
                        .change_property32(
                            xproto::PropMode::REPLACE,
                            request.requestor,
                            property,
                            xproto::AtomEnum::ATOM,
                            &[targets, utf8, text_atom],
                        )?
                        .check()?;
                    true
                } else if is_text {
                    self.connection
                        .change_property8(
                            xproto::PropMode::REPLACE,
                            request.requestor,
                            property,
                            utf8,
                            text.as_bytes(),
                        )?
                        .check()?;
                    true
                } else {
                    false
                };
                let reply = xproto::SelectionNotifyEvent {
                    response_type: xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: request.time,
                    requestor: request.requestor,
                    selection: request.selection,
                    target: request.target,
                    property: if accepted { property } else { 0 },
                };
                self.connection
                    .send_event(false, request.requestor, xproto::EventMask::NO_EVENT, reply)?
                    .check()?;
                if is_text {
                    return Ok(());
                }
            }
        })();
        // The receiver's property survives destruction of our temporary owner.
        self.connection.stream().reset(Duration::from_secs(1));
        let released = self.release();
        let destroyed = self.connection.destroy_window(window)?.check();
        result?;
        released?;
        destroyed?;
        Ok(())
    }
    fn scroll(&mut self, dx: f64, dy: f64) -> Result<()> {
        for (delta, negative, positive) in [(dy, 4, 5), (dx, 6, 7)] {
            let count = (delta.abs() / 120.0).ceil() as u8;
            let button = if delta < 0.0 { negative } else { positive };
            for _ in 0..count {
                self.check_cancel()?;
                self.button(button, true)?;
                self.button(button, false)?;
            }
        }
        Ok(())
    }
    fn input(&mut self, input: Input) -> Result<()> {
        self.check_cancel()?;
        match input {
            Input::Move { x, y } => self.move_to(x, y),
            Input::Button { x, y, button, down } => {
                self.move_to(x, y)?;
                self.button(input::x_button(button), down)
            }
            Input::Key { key, down } => self.hid(key, down),
            Input::Text { text } => self.text(&text),
            Input::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                self.move_to(x, y)?;
                self.scroll(delta_x, delta_y)
            }
            Input::ReleaseAll {} => self.release(),
        }
    }
    fn apply(&mut self, action: Action) -> Result<Value> {
        self.check_cancel()?;
        match action {
            Action::Observe {} => (),
            Action::Release {} | Action::Shutdown {} => {
                self.release()?;
                return Ok(json!({"status":"ok"}));
            }
            Action::Input { input } => {
                self.input(input)?;
                return Ok(json!({"status":"ok"}));
            }
            Action::Click { x, y, button } => {
                self.release()?;
                self.move_to(x, y)?;
                self.button(input::x_button(button), true)?;
                self.button(input::x_button(button), false)?;
            }
            Action::Type { text } => self.text(&text)?,
            Action::Key { key, modifiers } => {
                self.release()?;
                for &modifier in &modifiers {
                    self.hid(modifier, true)?;
                }
                self.hid(key, true)?;
                self.hid(key, false)?;
                for &modifier in modifiers.iter().rev() {
                    self.hid(modifier, false)?;
                }
            }
            Action::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                self.release()?;
                self.move_to(x, y)?;
                self.scroll(delta_x, delta_y)?;
            }
            Action::Drag {
                x,
                y,
                end_x,
                end_y,
                button,
                duration_ms,
            } => {
                self.release()?;
                self.connection.stream().reset(OP_TIMEOUT);
                self.move_to(x, y)?;
                self.button(input::x_button(button), true)?;
                let count = (duration_ms / 25).max(2);
                for step in 1..=count {
                    self.check_cancel()?;
                    let fraction = f64::from(step) / f64::from(count);
                    self.move_to(x + (end_x - x) * fraction, y + (end_y - y) * fraction)?;
                    thread::sleep(Duration::from_millis(u64::from(duration_ms / count)));
                }
                self.button(input::x_button(button), false)?;
            }
        }
        self.connection.stream().reset(OP_TIMEOUT);
        self.capture()
    }
    fn capture(&self) -> Result<Value> {
        self.check_cancel()?;
        let setup = self.connection.setup();
        let screen = &setup.roots[0];
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == screen.root_visual)
            .ok_or_else(|| invalid("root visual missing"))?;
        if visual.class != VisualClass::TRUE_COLOR {
            return Err(invalid("desktop requires a TrueColor root visual"));
        }
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .ok_or_else(|| invalid("root pixmap format missing"))?;
        let reply = self
            .connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                0,
                0,
                WIDTH,
                HEIGHT,
                u32::MAX,
            )?
            .reply()?;
        if reply.depth != screen.root_depth || reply.visual != screen.root_visual {
            return Err(invalid("unexpected root image format"));
        }
        let rgb = pixels::decode(
            &reply.data,
            u32::from(WIDTH),
            u32::from(HEIGHT),
            format.bits_per_pixel,
            format.scanline_pad,
            setup.image_byte_order == ImageOrder::LSB_FIRST,
            [visual.red_mask, visual.green_mask, visual.blue_mask],
        )?;
        let mut frame = image::RgbImage::from_raw(u32::from(WIDTH), u32::from(HEIGHT), rgb)
            .ok_or_else(|| invalid("invalid RGB frame"))?;
        if frame.width().max(frame.height()) > 1280 {
            let scale = 1280.0 / f64::from(frame.width().max(frame.height()));
            frame = image::imageops::resize(
                &frame,
                (f64::from(frame.width()) * scale) as u32,
                (f64::from(frame.height()) * scale) as u32,
                FilterType::Triangle,
            );
        }
        loop {
            for quality in [75, 60, 45, 30] {
                self.check_cancel()?;
                let mut jpeg = Vec::new();
                JpegEncoder::new_with_quality(&mut jpeg, quality).encode(
                    frame.as_raw(),
                    frame.width(),
                    frame.height(),
                    ExtendedColorType::Rgb8,
                )?;
                if jpeg.len().div_ceil(3) * 4 <= FRAME_LIMIT {
                    return Ok(
                        json!({"status":"ok","jpeg":STANDARD.encode(jpeg),"width":frame.width(),"height":frame.height()}),
                    );
                }
            }
            if frame.width().min(frame.height()) <= 160 {
                return Err(invalid("JPEG exceeds desktop frame limit"));
            }
            frame = image::imageops::resize(
                &frame,
                frame.width() / 2,
                frame.height() / 2,
                FilterType::Triangle,
            );
        }
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auth_cookie_has_correct_xauthority_wire_format() {
        let record = auth_record(&[7; 32]);
        assert_eq!(&record[..8], &[255, 255, 0, 0, 0, 0, 0, 18]);
        assert_eq!(&record[8..26], b"MIT-MAGIC-COOKIE-1");
        assert_eq!(&record[26..28], &[0, 32]);
        assert_eq!(&record[28..], &[7; 32]);
    }
    #[test]
    fn socket_reader_rejects_oversize_and_trailing_requests() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"12345\n").unwrap();
        assert!(read_line(&mut reader, 4, Instant::now() + OP_TIMEOUT, None).is_err());
        writer.write_all(b"{}\n{}\n").unwrap();
        assert!(read_line(&mut reader, 32, Instant::now() + OP_TIMEOUT, None).is_err());
    }
    #[test]
    fn runtime_never_replaces_existing_socket() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = directory.path().join("hand.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(Runtime::claim(directory.path()).is_err());
        assert!(UnixStream::connect(&socket).is_ok());
    }
    #[test]
    fn runtime_rejects_symlinks_and_public_directories() {
        let directory = tempfile::tempdir().unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        assert!(Runtime::claim(&alias).is_err());
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Runtime::claim(directory.path()).is_err());
    }
    #[test]
    #[ignore = "requires Xvfb, openbox, and xterm on Linux"]
    fn live_capture_unicode_raw_input_and_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let runtime = directory.path().join("runtime");
        fs::create_dir(&workspace).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let daemon = {
            let workspace = workspace.clone();
            let runtime = runtime.clone();
            let stop = stop.clone();
            thread::spawn(move || serve_blocking(&workspace, &runtime, stop))
        };
        struct Running(Arc<AtomicBool>, Option<thread::JoinHandle<Result<()>>>);
        impl Drop for Running {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
                if let Some(thread) = self.1.take() {
                    let _ = thread.join();
                }
            }
        }
        let mut running = Running(stop, Some(daemon));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !runtime.join("ready").exists() {
            assert!(Instant::now() < deadline, "desktop readiness timed out");
            assert!(
                !running.1.as_ref().unwrap().is_finished(),
                "desktop exited before ready"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fs::metadata(&runtime).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(runtime.join("hand.sock")).unwrap().mode() & 0o777,
            0o600
        );
        let ready: Value =
            serde_json::from_slice(&fs::read(runtime.join("ready")).unwrap()).unwrap();
        let display = ready["display"].as_str().unwrap();
        let number = display.trim_start_matches(':');
        let socket = format!("/tmp/.X11-unix/X{number}");
        let unauthorized = RustConnection::connect_to_stream(
            TimedStream::new(UnixStream::connect(&socket).unwrap()).unwrap(),
            0,
        );
        assert!(
            unauthorized.is_err(),
            "X11 accepted a client without the cookie"
        );
        let authority = fs::read(runtime.join("Xauthority")).unwrap();
        let connection = RustConnection::connect_to_stream_with_auth_info(
            TimedStream::new(UnixStream::connect(&socket).unwrap()).unwrap(),
            0,
            b"MIT-MAGIC-COOKIE-1".to_vec(),
            authority[authority.len() - 16..].to_vec(),
        )
        .unwrap();
        let setup = connection.setup();
        let root = setup.roots[0].root;
        let first = setup.min_keycode;
        let count = setup.max_keycode - first + 1;
        let before = connection
            .get_keyboard_mapping(first, count)
            .unwrap()
            .reply()
            .unwrap();
        // The ready transport can precede the window manager mapping xterm.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            connection.stream().reset(OP_TIMEOUT);
            let focus = connection.get_input_focus().unwrap().reply().unwrap().focus;
            if focus != 0 && focus != 1 && focus != root {
                break;
            }
            assert!(Instant::now() < deadline, "xterm did not receive focus");
            thread::sleep(Duration::from_millis(25));
        }
        let frame = request(&runtime, json!({"action":"observe"})).unwrap();
        assert_eq!(frame["status"], "ok");
        let jpeg = frame["jpeg"].as_str().unwrap();
        assert!(jpeg.len() <= FRAME_LIMIT);
        let image = image::load_from_memory(&STANDARD.decode(jpeg).unwrap()).unwrap();
        assert_eq!((image.width(), image.height()), (1280, 800));
        assert!(image.to_rgb8().pixels().any(|p| p.0 != [0, 0, 0]));
        assert_eq!(
            request(
                &runtime,
                json!({"action":"type","text":"printf '%s' 'λ世界😀' > unicode.txt"})
            )
            .unwrap()["status"],
            "ok"
        );
        request(&runtime, json!({"action":"key","key":40})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !workspace.join("unicode.txt").exists() {
            assert!(
                Instant::now() < deadline,
                "Unicode command was not delivered to xterm"
            );
            thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            fs::read_to_string(workspace.join("unicode.txt")).unwrap(),
            "λ世界😀"
        );
        // Exercise long text outside the shell line editor's command-length
        // limit. The application must receive every byte of a second paste.
        request(&runtime, json!({"action":"type", "text":"cat > long.txt"})).unwrap();
        request(&runtime, json!({"action":"key","key":40})).unwrap();
        let long_text = "0123456789abcdef".repeat(128);
        let reply = request(&runtime, json!({"action":"type", "text":long_text})).unwrap();
        assert_eq!(reply["status"], "ok", "{reply}");
        request(&runtime, json!({"action":"key","key":40})).unwrap();
        request(&runtime, json!({"action":"key","key":7,"modifiers":[224]})).unwrap();
        let expected = format!("{long_text}\n");
        let deadline = Instant::now() + Duration::from_secs(3);
        while fs::read_to_string(workspace.join("long.txt"))
            .ok()
            .as_deref()
            != Some(&expected)
        {
            assert!(
                Instant::now() < deadline,
                "long paste was not delivered exactly: {:?}",
                fs::read_to_string(workspace.join("long.txt")).map(|value| value.len())
            );
            thread::sleep(Duration::from_millis(25));
        }
        connection.stream().reset(OP_TIMEOUT);
        let after = connection
            .get_keyboard_mapping(first, count)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(
            before.keysyms, after.keysyms,
            "temporary text mapping was not restored"
        );
        request(
            &runtime,
            json!({"action":"input","input":{"kind":"button","x":1,"y":1,"button":0,"down":true}}),
        )
        .unwrap();
        let pointer = connection.query_pointer(root).unwrap().reply().unwrap();
        assert_eq!((pointer.root_x, pointer.root_y), (1279, 799));
        assert!(pointer.mask.contains(xproto::KeyButMask::BUTTON1));
        request(&runtime, json!({"action":"disconnect"})).unwrap();
        assert!(
            !connection
                .query_pointer(root)
                .unwrap()
                .reply()
                .unwrap()
                .mask
                .contains(xproto::KeyButMask::BUTTON1)
        );
        request(
            &runtime,
            json!({"action":"input","input":{"kind":"key","key":225,"down":true}}),
        )
        .unwrap();
        let shift = input::find_keycode(
            first,
            before.keysyms_per_keycode,
            &before.keysyms,
            input::hid_keysym(225).unwrap(),
        )
        .unwrap();
        let held = connection.query_keymap().unwrap().reply().unwrap().keys;
        assert_ne!(held[usize::from(shift) / 8] & (1 << (shift % 8)), 0);
        // Bypass request's validation to exercise the server's failure release.
        let mut bad = UnixStream::connect(runtime.join("hand.sock")).unwrap();
        bad.write_all(b"{\"action\":\"invalid\"}\n").unwrap();
        let reply = read_line(&mut bad, 1024, Instant::now() + OP_TIMEOUT, None).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&reply).unwrap()["status"],
            "error"
        );
        let held = connection.query_keymap().unwrap().reply().unwrap().keys;
        assert_eq!(held[usize::from(shift) / 8] & (1 << (shift % 8)), 0);
        request(
            &runtime,
            json!({"action":"drag","x":0.1,"y":0.9,"endX":0.8,"endY":0.9,"durationMs":50}),
        )
        .unwrap();
        let pointer = connection.query_pointer(root).unwrap().reply().unwrap();
        assert_eq!(pointer.root_x, 1023);
        assert!(!pointer.mask.contains(xproto::KeyButMask::BUTTON1));
        request(&runtime, json!({"action":"shutdown"})).unwrap();
        running.1.take().unwrap().join().unwrap().unwrap();
        assert!(!runtime.join("ready").exists());
        assert!(!runtime.join("hand.sock").exists());
        assert!(!runtime.join("Xauthority").exists());
        assert!(!Path::new(&socket).exists());
    }
}
