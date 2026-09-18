//! Explicit same-user Hyprland app control. Never falls back to global input.
use super::{Action, App, Desktop, Image, Target};
use crate::{Error, Result, ax::Node};
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn failure(e: impl std::fmt::Display) -> Error {
    Error::action(format!("Hyprland background: {e}"))
}
fn env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| failure(format!("missing {name}")))
}
fn peer(fd: i32) -> Result<i32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
        || size as usize != std::mem::size_of_val(&cred)
        || cred.uid != unsafe { libc::geteuid() }
        || cred.pid <= 0
    {
        return Err(failure("untrusted compositor peer"));
    }
    Ok(cred.pid)
}
fn output(command: &mut Command, limit: u64) -> Result<Vec<u8>> {
    super::check_native_cancellation()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(failure)?;
    let stream = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stream
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Err(error) = super::check_native_cancellation() {
            let _ = child.kill();
            let _ = child.wait();
            break Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(failure("helper timed out or failed; no retry"));
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| failure("helper reader failed"))?
        .map_err(failure)?;
    if !status?.success() || bytes.len() as u64 > limit {
        return Err(failure("helper failed or exceeded output limit"));
    }
    Ok(bytes)
}
#[derive(Clone, Debug, Deserialize)]
struct Window {
    address: String,
    #[serde(rename = "stableId")]
    stable_id: String,
    pid: i32,
    class: String,
    title: String,
    size: [f64; 2],
    mapped: bool,
    hidden: bool,
    xwayland: bool,
}
#[derive(Clone)]
struct Identity {
    window: Window,
    start: String,
    executable: String,
}
fn identity(window: Window) -> Result<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", window.pid)).map_err(failure)?;
    let start = stat
        .rsplit_once(") ")
        .and_then(|(_, s)| s.split_whitespace().nth(19))
        .ok_or_else(|| failure("invalid process identity"))?
        .to_owned();
    let executable = std::fs::read_link(format!("/proc/{}/exe", window.pid))
        .map_err(failure)?
        .to_string_lossy()
        .into_owned();
    Ok(Identity {
        window,
        start,
        executable,
    })
}
struct Connection {
    fd: OwnedFd,
    sequence: u64,
    drag_options: bool,
}
impl Connection {
    fn connect(path: &std::path::Path, expected: i32) -> Result<(Option<Self>, usize)> {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        if bytes.len() >= addr.sun_path.len() {
            return Err(failure("input socket path too long"));
        }
        addr.sun_family = libc::AF_UNIX as _;
        for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
            *dst = *src as _;
        }
        let raw =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
        if raw < 0 {
            return Err(failure(std::io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let timeout = libc::timeval {
            tv_sec: 3,
            tv_usec: 0,
        };
        for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
            if unsafe {
                libc::setsockopt(
                    raw,
                    libc::SOL_SOCKET,
                    option,
                    (&timeout as *const libc::timeval).cast(),
                    std::mem::size_of_val(&timeout) as _,
                )
            } != 0
            {
                return Err(failure("cannot bound input transport"));
            }
        }
        if unsafe {
            libc::connect(
                raw,
                (&addr as *const libc::sockaddr_un).cast(),
                std::mem::size_of_val(&addr) as _,
            )
        } != 0
        {
            return Err(failure(std::io::Error::last_os_error()));
        }
        if peer(raw)? != expected {
            return Err(failure("input and capture compositor mismatch"));
        }
        let mut c = Self {
            fd,
            sequence: 0,
            drag_options: false,
        };
        let hello = c.request("HELLO")?;
        if hello["protocol"] != 3 {
            return Err(failure("unsupported input protocol"));
        }
        c.drag_options = hello["background_drag_options"] == true;
        // Older v3 builds expose exactly two seats. New builds advertise a
        // bounded capacity; never guess additional sockets on an old plugin.
        let lanes = Self::lane_capacity(&hello)?;
        // Only an explicit refusal proves no lane was claimed. Transport
        // failures have unknown outcomes and must never trigger a retry.
        c.send("CLAIM")?;
        let claim = c.receive_value()?;
        if !Self::claimed(claim)? {
            return Ok((None, lanes));
        }
        Ok((Some(c), lanes))
    }
    fn lane_capacity(hello: &Value) -> Result<usize> {
        match hello.get("background_lanes") {
            None => Ok(2),
            Some(value) => value
                .as_u64()
                .filter(|n| (1..=16).contains(n))
                .map(|n| n as usize)
                .ok_or_else(|| failure("invalid background lane capacity")),
        }
    }
    fn claimed(response: Value) -> Result<bool> {
        if response["ok"] == false && response["code"] == "lane_busy" {
            return Ok(false);
        }
        Self::accepted(response).map(|_| true)
    }
    fn send(&mut self, message: &str) -> Result<()> {
        super::check_native_cancellation()?;
        let sent = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                message.as_ptr().cast(),
                message.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if sent != message.len() as isize {
            return Err(failure("input send failed; outcome unknown, no retry"));
        }
        Ok(())
    }
    fn request(&mut self, message: &str) -> Result<Value> {
        self.send(message)?;
        self.receive()
    }
    fn receive(&mut self) -> Result<Value> {
        Self::accepted(self.receive_value()?)
    }
    fn accepted(response: Value) -> Result<Value> {
        if response["ok"] != true {
            return Err(failure(format!("input refused: {}", response["code"])));
        }
        Ok(response)
    }
    fn receive_value(&mut self) -> Result<Value> {
        // Poll in short bounded intervals so revocation closes the owned socket
        // promptly, releasing any held plugin input. Never resend a command.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            super::check_native_cancellation()?;
            let mut poll = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 10) };
            if ready > 0 {
                break;
            }
            if ready < 0 || Instant::now() >= deadline {
                return Err(failure("input receipt missing; outcome unknown, no retry"));
            }
        }
        super::check_native_cancellation()?;
        let mut buffer = [0u8; 16384];
        let n = unsafe {
            libc::recv(
                self.fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_TRUNC,
            )
        };
        if n <= 0 || n as usize > buffer.len() {
            return Err(failure("input receipt missing; outcome unknown, no retry"));
        }
        serde_json::from_slice(&buffer[..n as usize]).map_err(failure)
    }
}
pub struct Hyprland {
    signature: String,
    directory: PathBuf,
    capture: PathBuf,
    compositor: i32,
    bound: HashMap<String, Identity>,
    observed: HashMap<String, [f64; 2]>,
    connections: HashMap<(String, String), Connection>,
    visual_scope: String,
}
impl Hyprland {
    pub fn from_environment() -> Result<Self> {
        let signature = env("HYPRLAND_INSTANCE_SIGNATURE")?;
        if !signature
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(failure("invalid instance signature"));
        }
        let runtime = PathBuf::from(env("XDG_RUNTIME_DIR")?);
        let display = PathBuf::from(env("WAYLAND_DISPLAY")?);
        let capture = PathBuf::from(env("NANOCODEX_HYPRLAND_CAPTURE")?);
        if !runtime.is_absolute()
            || !capture.is_absolute()
            || !capture.is_file()
            || std::env::var_os("WAYLAND_SOCKET").is_some()
        {
            return Err(failure(
                "explicit local runtime and capture executable required",
            ));
        }
        let display = if display.is_absolute() {
            display
        } else {
            if display.components().count() != 1
                || !matches!(
                    display.components().next(),
                    Some(std::path::Component::Normal(_))
                )
            {
                return Err(failure("invalid Wayland display"));
            }
            runtime.join(display)
        };
        let directory = runtime.join("hypr").join(&signature);
        let ipc = UnixStream::connect(directory.join(".socket.sock")).map_err(failure)?;
        let wayland = UnixStream::connect(display).map_err(failure)?;
        let compositor = peer(ipc.as_raw_fd())?;
        if peer(wayland.as_raw_fd())? != compositor {
            return Err(failure(
                "Wayland and Hyprland belong to different compositors",
            ));
        }
        Ok(Self {
            signature,
            directory,
            capture,
            compositor,
            bound: HashMap::new(),
            observed: HashMap::new(),
            connections: HashMap::new(),
            visual_scope: "initial".into(),
        })
    }
    fn windows(&self) -> Result<Vec<Window>> {
        let data = output(
            Command::new("hyprctl").args(["-i", &self.signature, "clients", "-j"]),
            2 * 1024 * 1024,
        )?;
        let windows: Vec<Window> = serde_json::from_slice(&data).map_err(failure)?;
        Ok(windows
            .into_iter()
            .filter(|w| {
                w.pid > 0
                    && w.mapped
                    && !w.hidden
                    && !w.xwayland
                    && w.size.iter().all(|x| x.is_finite() && *x > 0.)
            })
            .collect())
    }
    fn checked(&self, app: &App) -> Result<Identity> {
        let previous = self
            .bound
            .get(&app.id)
            .ok_or_else(|| failure("unbound application"))?;
        let window = self
            .windows()?
            .into_iter()
            .find(|w| {
                w.address == previous.window.address
                    && w.stable_id == previous.window.stable_id
                    && w.pid == app.pid
            })
            .ok_or_else(|| failure("target window closed"))?;
        let current = identity(window)?;
        if current.start != previous.start || current.executable != app.path {
            return Err(failure("target process changed"));
        }
        Ok(current)
    }
    fn operation(
        &mut self,
        app: &App,
        cap: u8,
        command: &str,
        args: String,
        points: &[[f64; 2]],
    ) -> Result<()> {
        let connection_id = (self.visual_scope.clone(), app.id.clone());
        if let Err(error) = super::check_native_cancellation() {
            self.connections.remove(&connection_id);
            return Err(error);
        }
        let current = match self.checked(app) {
            Ok(current) => current,
            Err(error) => {
                self.connections.remove(&connection_id);
                self.observed.remove(&app.id);
                return Err(error);
            }
        };
        if !points.is_empty() && self.observed.get(&app.id) != Some(&current.window.size) {
            return Err(failure(
                "take a fresh app screenshot before coordinate input",
            ));
        }
        for point in points {
            super::window_point(
                [0., 0., current.window.size[0], current.window.size[1]],
                *point,
            )?;
        }
        if !self.connections.contains_key(&connection_id) {
            let mut claimed = None;
            let mut lane = 0;
            let mut capacity = 1;
            while lane < capacity {
                super::check_native_cancellation()?;
                let socket = if lane == 0 {
                    "cua-input-v3.sock".into()
                } else {
                    format!("cua-input-v3-{}.sock", lane + 1)
                };
                let (connection, advertised) =
                    Connection::connect(&self.directory.join(socket), self.compositor)?;
                if lane == 0 {
                    capacity = advertised;
                } else if advertised != capacity {
                    return Err(failure("inconsistent background lane capacity"));
                }
                if connection.is_some() {
                    claimed = connection;
                    break;
                }
                lane += 1;
            }
            let connection =
                claimed.ok_or_else(|| failure("all background input lanes are busy"))?;
            self.connections.insert(connection_id.clone(), connection);
        }
        let connection = self.connections.get_mut(&connection_id).unwrap();
        let result = (|| {
            if command == "DRAG" && args.split_whitespace().count() == 7 && !connection.drag_options
            {
                return Err(Error::unsupported(
                    "Plugin does not advertise background drag options",
                ));
            }
            let target = connection.request(&format!(
                "TARGET {} {} {cap}",
                app.pid,
                current.window.address.trim_start_matches("0x")
            ))?;
            if target["width"].as_f64() != Some(current.window.size[0])
                || target["height"].as_f64() != Some(current.window.size[1])
            {
                return Err(failure("target resized before input"));
            }
            let token = target["target"]
                .as_str()
                .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| failure("invalid target token"))?;
            let revision = target["revision"]
                .as_u64()
                .ok_or_else(|| failure("invalid target revision"))?;
            connection.sequence = connection
                .sequence
                .checked_add(1)
                .ok_or_else(|| failure("sequence exhausted"))?;
            let receipt = connection.request(&format!(
                "{command} {} {token} {revision} {args}",
                connection.sequence
            ))?;
            if command == "DRAG" {
                if receipt["phase"] != "started" {
                    return Err(failure("invalid drag start receipt"));
                }
                connection.receive()?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.connections.remove(&connection_id);
        }
        result
    }
}
fn key(character: char) -> Result<(u16, u8)> {
    let lower = character.to_ascii_lowercase();
    let code = match lower {
        'a' => 30,
        'b' => 48,
        'c' => 46,
        'd' => 32,
        'e' => 18,
        'f' => 33,
        'g' => 34,
        'h' => 35,
        'i' => 23,
        'j' => 36,
        'k' => 37,
        'l' => 38,
        'm' => 50,
        'n' => 49,
        'o' => 24,
        'p' => 25,
        'q' => 16,
        'r' => 19,
        's' => 31,
        't' => 20,
        'u' => 22,
        'v' => 47,
        'w' => 17,
        'x' => 45,
        'y' => 21,
        'z' => 44,
        '1'..='9' => 2 + (lower as u16 - '1' as u16),
        '0' => 11,
        ' ' => 57,
        '\n' => 28,
        '\t' => 15,
        '-' => 12,
        '=' => 13,
        '[' => 26,
        ']' => 27,
        ';' => 39,
        '\'' => 40,
        '`' => 41,
        '\\' => 43,
        ',' => 51,
        '.' => 52,
        '/' => 53,
        _ => {
            return Err(Error::unsupported(
                "Background text currently supports US ASCII letters, digits and unshifted punctuation",
            ));
        }
    };
    Ok((code, u8::from(character.is_ascii_uppercase())))
}
fn chord(value: &str) -> Result<(u16, u8)> {
    let parts: Vec<_> = value.split('+').collect();
    let mut modifiers = 0;
    for part in &parts[..parts.len() - 1] {
        let bit = match part.to_ascii_lowercase().as_str() {
            "shift" => 1,
            "ctrl" | "control" => 2,
            "alt" | "option" => 4,
            "super" | "cmd" | "command" | "meta" => 8,
            _ => return Err(Error::invalid("Unsupported background modifier")),
        };
        if modifiers & bit != 0 {
            return Err(Error::invalid("Duplicate modifier"));
        }
        modifiers |= bit;
    }
    let last = parts.last().unwrap().to_ascii_lowercase();
    let code = match last.as_str() {
        "enter" | "return" => 28,
        "escape" | "esc" => 1,
        "backspace" => 14,
        "tab" => 15,
        "space" => 57,
        "left" => 105,
        "right" => 106,
        "up" => 103,
        "down" => 108,
        "delete" => 111,
        "home" => 102,
        "end" => 107,
        _ => {
            let mut chars = last.chars();
            let c = chars.next().ok_or_else(|| Error::invalid("Empty key"))?;
            if chars.next().is_some() {
                return Err(Error::unsupported("Unsupported background key"));
            }
            let (k, m) = key(c)?;
            modifiers |= m;
            k
        }
    };
    Ok((code, modifiers))
}
impl Desktop for Hyprland {
    fn action_in_scope(&mut self, app: &App, action: Action, scope: &str) -> Result<()> {
        let previous = std::mem::replace(&mut self.visual_scope, scope.to_owned());
        let result = self.action(app, action);
        self.visual_scope = previous;
        result
    }
    fn reset_visual_scope(&mut self, scope: &str) -> Result<()> {
        self.connections.retain(|(owner, _), _| owner != scope);
        Ok(())
    }

    fn window_lane_binding(&mut self, identifier: &str) -> Result<Option<App>> {
        if !identifier.starts_with("hyprland:") {
            return Ok(None);
        }
        self.apps()?
            .into_iter()
            .find(|app| app.id == identifier)
            .map(Some)
            .ok_or_else(|| failure("exact target window is unavailable"))
    }
    fn session_key(&self, app: &App) -> String {
        app.id.clone()
    }
    fn sky_target(&self) -> &'static str {
        "linux"
    }
    fn app_interface(&self) -> bool {
        true
    }
    fn apps(&mut self) -> Result<Vec<App>> {
        let mut apps = Vec::new();
        for window in self.windows()? {
            let Ok(i) = identity(window) else { continue };
            let id = format!(
                "hyprland:{}:{}:{}:{}",
                i.window.pid, i.start, i.window.address, i.window.stable_id
            );
            apps.push(App {
                window_id: None,
                id: id.clone(),
                name: i.window.class.clone(),
                path: i.executable.clone(),
                pid: i.window.pid,
            });
            self.bound.entry(id).or_insert(i);
        }
        let live: std::collections::HashSet<_> = apps.iter().map(|app| app.id.as_str()).collect();
        self.connections
            .retain(|(_, id), _| live.contains(id.as_str()));
        self.observed.retain(|id, _| live.contains(id.as_str()));
        self.bound.retain(|id, _| live.contains(id.as_str()));
        Ok(apps)
    }
    fn validate_app(&mut self, app: &App) -> Result<bool> {
        Ok(self.checked(app).is_ok())
    }
    fn snapshot(&mut self, app: &App) -> Result<Node> {
        let i = self.checked(app)?;
        Ok(Node{identity:app.id.clone(),role:"AXWindow".into(),title:Some(i.window.title),frame:Some([0.,0.,i.window.size[0],i.window.size[1]]),detail:Some("Background Wayland window; use app screenshot coordinates. Accessibility elements unavailable.".into()),..Default::default()})
    }
    fn prepare_screenshot(&mut self, app: &App) -> Result<()> {
        self.checked(app).map(|_| ())
    }
    fn screenshot(&mut self, app: &App) -> Result<Image> {
        let i = self.checked(app)?;
        let mut bytes = output(
            Command::new(&self.capture).arg(&i.window.address),
            32 * 1024 * 1024,
        )?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(failure("capture did not return PNG"));
        }
        if bytes.len() < 24 {
            return Err(failure("truncated PNG"));
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        if width == 0 || height == 0 || width as u64 * height as u64 > 64 * 1024 * 1024 {
            return Err(failure("invalid capture dimensions"));
        }
        let logical = [i.window.size[0] as u32, i.window.size[1] as u32];
        if [width, height] != logical {
            let image = image::load_from_memory(&bytes)
                .map_err(failure)?
                .resize_exact(
                    logical[0],
                    logical[1],
                    image::imageops::FilterType::Triangle,
                );
            bytes.clear();
            image
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .map_err(failure)?;
        }
        let after = self.checked(app)?;
        if after.window.size != i.window.size {
            return Err(failure("window resized during capture"));
        }
        Ok(Image {
            mime_type: "image/png".into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }
    fn screenshot_for_observation(&mut self, app: &App) -> Result<Image> {
        self.observed.remove(&app.id);
        let image = self.screenshot(app)?;
        self.observed
            .insert(app.id.clone(), self.checked(app)?.window.size);
        Ok(image)
    }
    fn invalidate_screenshot(&mut self, app: &App) {
        self.observed.remove(&app.id);
    }
    fn action(&mut self, app: &App, action: Action) -> Result<()> {
        let result = self.perform_action(app, action);
        if result.is_err() {
            self.connections
                .remove(&(self.visual_scope.clone(), app.id.clone()));
        }
        result
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec!["background-app-input", "get_screenshot"]
    }
    fn end_session(&mut self, _: &str) -> Result<()> {
        self.connections.clear();
        self.observed.clear();
        Ok(())
    }
}
impl Hyprland {
    fn perform_action(&mut self, app: &App, action: Action) -> Result<()> {
        super::check_native_cancellation()?;
        match action {
            Action::Click {
                target: Target::Point { point },
                button,
                count,
            } => {
                if button > 2 || !(1..=2).contains(&count) {
                    return Err(Error::invalid("Unsupported click"));
                }
                let button = [272, 273, 274][button as usize];
                self.operation(
                    app,
                    1,
                    "CLICK",
                    format!("{} {} {button} {count}", point[0], point[1]),
                    &[point],
                )
            }
            Action::PressKey { key: value } => {
                let (k, m) = chord(&value)?;
                self.operation(app, 2, "KEY", format!("{k} {m}"), &[])
            }
            Action::TypeText { text } => {
                let keys = text.chars().map(key).collect::<Result<Vec<_>>>()?;
                if keys.len() > 4096 {
                    return Err(Error::invalid("Background text limit is 4096 characters"));
                }
                for (k, m) in keys {
                    self.operation(app, 2, "KEY", format!("{k} {m}"), &[])?;
                }
                Ok(())
            }
            Action::Drag {
                from,
                to,
                button,
                modifiers,
            } => {
                if button > 2 {
                    return Err(Error::invalid("Invalid drag button"));
                }
                let mut bits = 0;
                for m in modifiers {
                    let bit = match m.as_str() {
                        "shift" => 1,
                        "ctrl" => 2,
                        "alt" => 4,
                        "super" => 8,
                        _ => return Err(Error::invalid("Invalid drag modifier")),
                    };
                    if bits & bit != 0 {
                        return Err(Error::invalid("Duplicate modifier"));
                    }
                    bits |= bit;
                }
                let mut args = format!("{} {} {} {} 250", from[0], from[1], to[0], to[1]);
                if button != 0 || bits != 0 {
                    args.push_str(&format!(" {} {bits}", [272, 273, 274][button as usize]));
                }
                self.operation(app, 8, "DRAG", args, &[from, to])
            }
            Action::Scroll {
                target: Target::Point { point },
                direction,
                pages,
            } => {
                if !pages.is_finite() || pages <= 0. || pages > 10. {
                    return Err(Error::invalid("Invalid scroll pages"));
                }
                let (axis, sign) = match direction.as_str() {
                    "up" => (0, -1.),
                    "down" => (0, 1.),
                    "left" => (1, -1.),
                    "right" => (1, 1.),
                    _ => return Err(Error::invalid("Invalid scroll direction")),
                };
                self.operation(
                    app,
                    4,
                    "SCROLL",
                    format!("{} {} {axis} {}", point[0], point[1], sign * pages * 100.),
                    &[point],
                )
            }
            _ => Err(Error::unsupported(
                "Operation is unavailable for background Wayland windows",
            )),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revoked_delivery_sends_nothing_and_wait_is_interruptible() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let mut pair = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    pair.as_mut_ptr(),
                )
            },
            0
        );
        let mut connection = Connection {
            fd: unsafe { OwnedFd::from_raw_fd(pair[0]) },
            sequence: 0,
            drag_options: true,
        };
        let peer = unsafe { OwnedFd::from_raw_fd(pair[1]) };
        let cancelled = Arc::new(AtomicBool::new(true));
        super::super::set_native_cancellation(Some(cancelled.clone()));
        assert_eq!(
            connection.send("KEY 1 token 1 17 0").unwrap_err().code,
            -32800
        );
        let mut byte = 0u8;
        assert_eq!(
            unsafe {
                libc::recv(
                    peer.as_raw_fd(),
                    (&mut byte as *mut u8).cast(),
                    1,
                    libc::MSG_DONTWAIT,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
        cancelled.store(false, Ordering::Release);
        let revoke = cancelled.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            revoke.store(true, Ordering::Release);
        });
        let start = Instant::now();
        assert_eq!(connection.receive_value().unwrap_err().code, -32800);
        assert!(start.elapsed() < Duration::from_millis(500));
        thread.join().unwrap();
        super::super::set_native_cancellation(None);
    }
    #[test]
    fn revoked_helper_does_not_launch() {
        use std::sync::{Arc, atomic::AtomicBool};
        super::super::set_native_cancellation(Some(Arc::new(AtomicBool::new(true))));
        assert_eq!(
            output(&mut Command::new("/nonexistent-revoked-helper"), 10)
                .unwrap_err()
                .code,
            -32800
        );
        super::super::set_native_cancellation(None);
    }
    #[test]
    fn legacy_two_lane_capacity_and_bounded_new_capacity() {
        assert_eq!(
            Connection::lane_capacity(&serde_json::json!({})).unwrap(),
            2
        );
        assert_eq!(
            Connection::lane_capacity(&serde_json::json!({"background_lanes":8})).unwrap(),
            8
        );
        for value in [
            serde_json::json!(0),
            serde_json::json!(17),
            serde_json::json!(-1),
            serde_json::json!("8"),
            Value::Null,
        ] {
            assert!(
                Connection::lane_capacity(&serde_json::json!({"background_lanes":value})).is_err()
            );
        }
    }
    #[test]
    fn only_explicit_busy_claim_allows_another_lane() {
        assert!(!Connection::claimed(serde_json::json!({"ok":false,"code":"lane_busy"})).unwrap());
        assert!(Connection::claimed(serde_json::json!({"ok":true,"lane":1})).unwrap());
        for response in [
            serde_json::json!({"code":"lane_busy"}),
            serde_json::json!({"ok":false,"code":"session_unavailable"}),
            Value::Null,
        ] {
            assert!(Connection::claimed(response).is_err());
        }
    }
    #[test]
    fn ending_engine_releases_all_its_target_lanes() {
        fn connection() -> (Connection, OwnedFd) {
            let mut pair = [-1; 2];
            assert_eq!(
                unsafe {
                    libc::socketpair(
                        libc::AF_UNIX,
                        libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                        0,
                        pair.as_mut_ptr(),
                    )
                },
                0
            );
            (
                Connection {
                    fd: unsafe { OwnedFd::from_raw_fd(pair[0]) },
                    sequence: 0,
                    drag_options: true,
                },
                unsafe { OwnedFd::from_raw_fd(pair[1]) },
            )
        }
        let (a, peer_a) = connection();
        let (b, peer_b) = connection();
        let mut desktop = Hyprland {
            signature: String::new(),
            directory: PathBuf::new(),
            capture: PathBuf::new(),
            compositor: 1,
            bound: HashMap::new(),
            observed: HashMap::from([("a".into(), [100., 100.]), ("b".into(), [200., 200.])]),
            connections: HashMap::from([
                (("first".into(), "a".into()), a),
                (("second".into(), "b".into()), b),
            ]),
            visual_scope: "initial".into(),
        };
        desktop.reset_visual_scope("first").unwrap();
        assert!(
            !desktop
                .connections
                .contains_key(&("first".into(), "a".into()))
        );
        assert!(
            desktop
                .connections
                .contains_key(&("second".into(), "b".into()))
        );
        let mut pending = 0u8;
        assert_eq!(
            unsafe {
                libc::recv(
                    peer_b.as_raw_fd(),
                    (&mut pending as *mut u8).cast(),
                    1,
                    libc::MSG_DONTWAIT,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            unsafe {
                libc::recv(
                    peer_a.as_raw_fd(),
                    (&mut pending as *mut u8).cast(),
                    1,
                    libc::MSG_DONTWAIT,
                )
            },
            0
        );
        desktop.end_session("opaque-engine-owner").unwrap();
        assert!(!desktop.observed.contains_key("a"));
        assert!(desktop.connections.is_empty());
        assert!(desktop.observed.is_empty());
        let mut byte = 0u8;
        assert_eq!(
            unsafe {
                libc::recv(
                    peer_a.as_raw_fd(),
                    (&mut byte as *mut u8).cast(),
                    1,
                    libc::MSG_DONTWAIT,
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                libc::recv(
                    peer_b.as_raw_fd(),
                    (&mut byte as *mut u8).cast(),
                    1,
                    libc::MSG_DONTWAIT,
                )
            },
            0
        );
    }
    #[test]
    fn reject_text_before_any_delivery() {
        assert!(
            "hello🧪"
                .chars()
                .map(key)
                .collect::<Result<Vec<_>>>()
                .is_err()
        );
        assert_eq!(key('W').unwrap(), (17, 1));
    }
    #[test]
    fn chords_do_not_silently_drop_modifiers() {
        assert_eq!(chord("ctrl+shift+a").unwrap(), (30, 3));
        assert!(chord("ctrl+ctrl+a").is_err());
        assert!(chord("hyper+a").is_err());
    }
}

/// Validate compositor endpoints before transferring only this explicit context.
pub(super) fn window_lane_environment() -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    if env("NANOCODEX_COMPUTER_BACKGROUND")? != "hyprland" {
        return Err(failure("unsupported background backend"));
    }
    let _validated = Hyprland::from_environment()?;
    [
        "NANOCODEX_COMPUTER_BACKGROUND",
        "XDG_RUNTIME_DIR",
        "WAYLAND_DISPLAY",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "NANOCODEX_HYPRLAND_CAPTURE",
    ]
    .into_iter()
    .map(|name| Ok((name.into(), env(name)?.into())))
    .collect()
}
