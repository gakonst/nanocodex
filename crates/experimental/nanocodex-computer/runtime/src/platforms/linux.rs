use super::{process, required, unsigned};
use crate::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    Idle,
    Dragging,
    Ended,
}
/// Monotonic settling deadline; a later action may extend but cannot shorten it.
pub struct Settler {
    delay: Duration,
    ready: Instant,
}
impl Settler {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            ready: Instant::now(),
        }
    }
    pub fn remaining(&self) -> Duration {
        self.ready.saturating_duration_since(Instant::now())
    }
    pub fn wait(&self) {
        let left = self.remaining();
        if !left.is_zero() {
            thread::sleep(left);
        }
    }
    pub fn defer(&mut self) {
        self.ready = self.ready.max(Instant::now() + self.delay);
    }
}
pub struct Linux {
    executable: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    mouse_size: Option<u64>,
    settler: Settler,
    drags: BTreeMap<u64, Drag>,
    next_drag: u64,
}
impl Linux {
    pub fn new(
        executable: PathBuf,
        args: Vec<String>,
        timeout: Duration,
        post_action_sleep_ms: u64,
        mouse_size: Option<u64>,
    ) -> Result<Self> {
        if post_action_sleep_ms > 5000 || mouse_size.is_some_and(|n| n == 0 || n > 1000) {
            return Err(Error::invalid("Invalid Linux settle/mouse-size setting"));
        }
        Ok(Self {
            executable,
            args,
            timeout,
            mouse_size,
            settler: Settler::new(Duration::from_millis(post_action_sleep_ms)),
            drags: BTreeMap::new(),
            next_drag: 1,
        })
    }
    fn command(&mut self, method: &str, params: &Value) -> Result<Value> {
        self.settler.wait();
        let mut args = self.args.clone();
        if let Some(size) = self.mouse_size {
            args.extend(["--mouse-size-px".into(), size.to_string()]);
        }
        args.push(method.into());
        let output = process::run(
            &self.executable,
            &args,
            &serde_json::to_vec(params)?,
            self.timeout,
        )?;
        if method != "get_screenshot" {
            self.settler.defer();
        }
        if output.iter().all(u8::is_ascii_whitespace) {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_slice(&output)?)
        }
    }
    pub fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "capabilities" => Ok(
                json!({"target":"linux","source_contract":"per-command JSON stdin/stdout","methods":["click","drag","move","press_key","scroll","type_text","get_screenshot","drag_handle.create","drag_handle.start","drag_handle.move_to","drag_handle.end","end_turn"]}),
            ),
            "click" | "drag" | "move" | "press_key" | "scroll" | "type_text" | "get_screenshot" => {
                self.command(method, params)
            }
            "drag_handle.create" => {
                if self.drags.len() >= 128 {
                    return Err(Error::action("Drag handle limit reached"));
                }
                let id = self.next_drag;
                self.next_drag += 1;
                self.drags.insert(id, Drag::Idle);
                Ok(json!({"handle":id}))
            }
            "drag_handle.start" | "drag_handle.move_to" | "drag_handle.end" => {
                let id = unsigned(params, "handle", 0)?;
                let state = *self
                    .drags
                    .get(&id)
                    .ok_or_else(|| Error::action("Unknown drag handle"))?;
                let action = method.strip_prefix("drag_handle.").unwrap();
                match (action, state) {
                    ("start", Drag::Idle)
                    | ("move_to", Drag::Dragging)
                    | ("end", Drag::Dragging) => (),
                    ("start", _) => {
                        return Err(Error::action("drag handle can only be started once"));
                    }
                    _ => {
                        return Err(Error::action(
                            "drag handle must be started before moving or ending",
                        ));
                    }
                }
                let mut args = params.clone();
                args.as_object_mut().unwrap().remove("handle");
                args["action"] = json!(action);
                let result = self.command("drag_handle", &args)?;
                self.drags.insert(
                    id,
                    if action == "end" {
                        Drag::Ended
                    } else {
                        Drag::Dragging
                    },
                );
                Ok(result)
            }
            "end_turn" => {
                self.end_turn()?;
                Ok(Value::Null)
            }
            _ => Err(Error::unsupported(format!(
                "Unsupported Linux helper method: {method}"
            ))),
        }
    }
    pub fn end_turn(&mut self) -> Result<()> {
        if self.drags.values().any(|s| *s == Drag::Dragging) {
            self.command("drag_handle", &json!({"action":"end"}))?;
        }
        self.drags.clear();
        Ok(())
    }
}
/// Actual X11 desktop control through xdotool and ImageMagick `import`.
/// Registration is explicit and no command is executed while constructing it.
pub struct X11 {
    xdotool: PathBuf,
    screenshot: PathBuf,
    timeout: Duration,
}
impl X11 {
    pub fn new(xdotool: PathBuf, screenshot: PathBuf, timeout: Duration) -> Self {
        Self {
            xdotool,
            screenshot,
            timeout,
        }
    }
    fn command(&self, args: Vec<String>, input: &[u8]) -> Result<Vec<u8>> {
        process::run(&self.xdotool, &args, input, self.timeout)
    }
    pub fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        let mut args = Vec::new();
        let mut stdin = Vec::new();
        match method {
            "capabilities" => {
                return Ok(
                    json!({"target":"linux-x11","methods":["list_windows","get_window","activate_window","move","click","drag","scroll","press_key","type_text","get_screenshot"],"unavailable":["Wayland","AT-SPI accessibility tree","element actions"]}),
                );
            }
            "list_windows" => {
                let output = self.command(
                    vec![
                        "search".into(),
                        "--onlyvisible".into(),
                        "--name".into(),
                        ".*".into(),
                    ],
                    &[],
                )?;
                let ids = String::from_utf8(output)
                    .map_err(|_| Error::action("Invalid X11 output encoding"))?
                    .split_whitespace()
                    .map(str::parse::<u64>)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| Error::action("Invalid X11 window identifier"))?;
                return Ok(json!(ids));
            }
            "get_window" => {
                let id = window_id(params)?;
                let name = self.command(vec!["getwindowname".into(), id.clone()], &[])?;
                let pid = self.command(vec!["getwindowpid".into(), id.clone()], &[])?;
                let geometry = self.command(
                    vec!["getwindowgeometry".into(), "--shell".into(), id.clone()],
                    &[],
                )?;
                let text = String::from_utf8_lossy(&geometry);
                let fields: BTreeMap<_, _> = text
                    .lines()
                    .filter_map(|line| line.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                return Ok(
                    json!({"id":id,"title":String::from_utf8_lossy(&name).trim_end(),"pid":String::from_utf8_lossy(&pid).trim(),"geometry":fields}),
                );
            }
            "activate_window" => {
                args.extend(["windowactivate".into(), "--sync".into(), window_id(params)?])
            }
            "move" => args.extend([
                "mousemove".into(),
                "--sync".into(),
                coordinate(params, "x")?,
                coordinate(params, "y")?,
            ]),
            "click" => {
                let count = unsigned(params, "click_count", 1)?;
                if !(1..=10).contains(&count) {
                    return Err(Error::invalid("click_count must be 1..10"));
                }
                let button = button(params)?;
                args.extend([
                    "mousemove".into(),
                    "--sync".into(),
                    coordinate(params, "x")?,
                    coordinate(params, "y")?,
                    "click".into(),
                    "--repeat".into(),
                    count.to_string(),
                    "--delay".into(),
                    "100".into(),
                    button,
                ]);
            }
            "mouse_down" | "mouse_up" => {
                if params.get("x").is_some() || params.get("y").is_some() {
                    args.extend([
                        "mousemove".into(),
                        "--sync".into(),
                        coordinate(params, "x")?,
                        coordinate(params, "y")?,
                    ]);
                }
                args.extend([
                    if method == "mouse_down" {
                        "mousedown"
                    } else {
                        "mouseup"
                    }
                    .into(),
                    button(params)?,
                ]);
            }
            "drag" => {
                let from_x = coordinate(params, "from_x")?;
                let from_y = coordinate(params, "from_y")?;
                let to_x = coordinate(params, "to_x")?;
                let to_y = coordinate(params, "to_y")?;
                self.command(
                    vec![
                        "mousemove".into(),
                        "--sync".into(),
                        from_x,
                        from_y,
                        "mousedown".into(),
                        "1".into(),
                    ],
                    &[],
                )?;
                let moved =
                    self.command(vec!["mousemove".into(), "--sync".into(), to_x, to_y], &[]);
                let released = self.command(vec!["mouseup".into(), "1".into()], &[]);
                moved?;
                released?;
                return Ok(Value::Null);
            }
            "scroll" => {
                let direction = required(params, "direction")?;
                let wheel = match direction {
                    "up" => "4",
                    "down" => "5",
                    "left" => "6",
                    "right" => "7",
                    _ => return Err(Error::invalid("Unknown scroll direction")),
                };
                let steps = unsigned(params, "steps", 3)?;
                if !(1..=1000).contains(&steps) {
                    return Err(Error::invalid("Scroll steps must be 1..1000"));
                }
                if params.get("x").is_some() || params.get("y").is_some() {
                    args.extend([
                        "mousemove".into(),
                        "--sync".into(),
                        coordinate(params, "x")?,
                        coordinate(params, "y")?,
                    ]);
                }
                args.extend([
                    "click".into(),
                    "--repeat".into(),
                    steps.to_string(),
                    "--delay".into(),
                    "10".into(),
                    wheel.into(),
                ]);
            }
            "press_key" => {
                let key = required(params, "key")?;
                if key.starts_with('-') || key.contains('\0') {
                    return Err(Error::invalid("Invalid X11 key chord"));
                }
                args.extend(["key".into(), "--clearmodifiers".into(), key.into()]);
            }
            "type_text" => {
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("text required"))?;
                args.extend([
                    "type".into(),
                    "--clearmodifiers".into(),
                    "--file".into(),
                    "-".into(),
                ]);
                stdin = text.as_bytes().to_vec();
            }
            "get_screenshot" => {
                let window = params
                    .get("window")
                    .map(|_| window_id(params))
                    .transpose()?
                    .unwrap_or("root".into());
                let png = process::run(
                    &self.screenshot,
                    &["-window".into(), window, "png:-".into()],
                    &[],
                    self.timeout,
                )?;
                if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return Err(Error::action("Screenshot tool did not return PNG"));
                }
                return Ok(json!({"mime_type":"image/png","data":STANDARD.encode(png)}));
            }
            _ => {
                return Err(Error::unsupported(format!(
                    "Unsupported X11 action: {method}"
                )));
            }
        }
        self.command(args, &stdin)?;
        Ok(Value::Null)
    }
}
fn window_id(params: &Value) -> Result<String> {
    let v = params
        .get("window")
        .ok_or_else(|| Error::invalid("window ID required"))?;
    let id = v
        .as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        .filter(|id| *id > 0)
        .ok_or_else(|| Error::invalid("window must be a positive numeric X11 window ID"))?;
    Ok(id.to_string())
}
fn coordinate(params: &Value, key: &str) -> Result<String> {
    let n = params
        .get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= i32::MIN as f64 && *n <= i32::MAX as f64)
        .ok_or_else(|| Error::invalid(format!("{key} must be a finite i32 coordinate")))?;
    Ok(n.round().to_string())
}
fn button(params: &Value) -> Result<String> {
    Ok(match params
        .get("mouse_button")
        .and_then(Value::as_str)
        .unwrap_or("left")
    {
        "left" => "1",
        "middle" => "2",
        "right" => "3",
        _ => return Err(Error::invalid("Invalid mouse button")),
    }
    .into())
}

enum DesktopBackend {
    Tools(X11),
    #[cfg(target_os = "linux")]
    Native(Box<super::linux_x11::NativeX11>),
}
impl DesktopBackend {
    fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        match self {
            Self::Tools(provider) => provider.execute(method, params),
            #[cfg(target_os = "linux")]
            Self::Native(provider) => provider.execute(method, params),
        }
    }
    fn key(&mut self, down: bool, key: &str) -> Result<()> {
        match self {
            Self::Tools(provider) => provider
                .command(
                    vec![if down { "keydown" } else { "keyup" }.into(), key.into()],
                    &[],
                )
                .map(|_| ()),
            #[cfg(target_os = "linux")]
            Self::Native(provider) => {
                if down {
                    provider.key_down(key)
                } else {
                    provider.key_up(key)
                }
            }
        }
    }
    fn sky_screenshot(&mut self) -> Result<Value> {
        match self {
            Self::Tools(provider) => provider.execute("get_screenshot", &json!({})),
            #[cfg(target_os = "linux")]
            Self::Native(provider) => provider.execute("get_desktop_screenshot", &json!({})),
        }
    }
    fn release_all(&mut self) -> Result<()> {
        match self {
            Self::Tools(_) => Ok(()),
            #[cfg(target_os = "linux")]
            Self::Native(provider) => provider.release_all(),
        }
    }
}

/// Desktop integration for visible X11 windows. This intentionally exposes only
/// the window itself until an AT-SPI tree provider is configured.
pub struct LinuxDesktop {
    provider: DesktopBackend,
    #[cfg(target_os = "linux")]
    audio: Option<super::windows_audio_model::Audio>,
    sky_drags: BTreeMap<String, Drag>,
    sky_settler: Settler,
}
impl LinuxDesktop {
    pub fn new(xdotool: PathBuf, screenshot: PathBuf) -> Self {
        Self {
            provider: DesktopBackend::Tools(X11::new(xdotool, screenshot, Duration::from_secs(15))),
            #[cfg(target_os = "linux")]
            audio: None,
            sky_drags: BTreeMap::new(),
            sky_settler: Settler::new(Duration::from_millis(100)),
        }
    }
    pub fn from_environment() -> Result<Self> {
        if std::env::var_os("DISPLAY").is_none() {
            return Err(Error::unsupported(
                "X11 DISPLAY is unavailable; Wayland requires another backend",
            ));
        }
        #[cfg(target_os = "linux")]
        return Ok(Self {
            provider: DesktopBackend::Native(Box::new(super::linux_x11::NativeX11::connect()?)),
            audio: None,
            sky_drags: BTreeMap::new(),
            sky_settler: Settler::new(Duration::from_millis(100)),
        });
        #[cfg(not(target_os = "linux"))]
        Err(Error::unsupported("Native X11 requires Linux"))
    }

    fn window(app: &crate::native::App) -> Result<u64> {
        app.id
            .strip_prefix("x11:")
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| Error::action("Invalid X11 application identity"))
    }
    fn state(&mut self, app: &crate::native::App) -> Result<Value> {
        let state = self
            .provider
            .execute("get_window", &json!({"window":Self::window(app)?}))?;
        let current_pid = state["pid"].as_str().and_then(|s| s.parse::<i32>().ok());
        if current_pid != Some(app.pid) {
            return Err(Error::action(
                "X11 window identity was reused by another process",
            ));
        }
        Ok(state)
    }
    fn frame(&mut self, app: &crate::native::App) -> Result<[f64; 4]> {
        let state = self.state(app)?;
        let mut frame = [0.; 4];
        for (i, key) in ["X", "Y", "WIDTH", "HEIGHT"].into_iter().enumerate() {
            frame[i] = state["geometry"][key]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| Error::action("Malformed X11 window geometry"))?;
        }
        Ok(frame)
    }
    fn point(
        &mut self,
        app: &crate::native::App,
        target: crate::native::Target,
    ) -> Result<[f64; 2]> {
        let frame = self.frame(app)?;
        match target {
            crate::native::Target::Point { point } => crate::native::window_point(frame, point),
            crate::native::Target::Element { identity } => {
                if identity != app.id {
                    return Err(Error::unsupported(
                        "X11 window backend has no AT-SPI element resolver",
                    ));
                }
                Ok([frame[0] + frame[2] / 2., frame[1] + frame[3] / 2.])
            }
        }
    }
}
impl crate::native::Desktop for LinuxDesktop {
    fn sky_target(&self) -> &'static str {
        "linux"
    }
    fn sky_execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        self.sky_settler.wait();
        if matches!(method, "drag_start" | "drag_move" | "drag_end") {
            let id = required(params, "handle_id")?.to_owned();
            let state = self.sky_drags.get(&id).copied().unwrap_or(Drag::Idle);
            let (command, point) = match method {
                "drag_start" => {
                    if state != Drag::Idle {
                        return Err(Error::action("drag handle can only be started once"));
                    }
                    if self
                        .sky_drags
                        .values()
                        .any(|state| *state == Drag::Dragging)
                    {
                        return Err(Error::action("Another drag handle owns the pointer"));
                    }
                    if self.sky_drags.len() >= 128 {
                        return Err(Error::action("Drag handle limit reached"));
                    }
                    (
                        "mouse_down",
                        params
                            .get("point")
                            .ok_or_else(|| Error::invalid("point required"))?,
                    )
                }
                "drag_move" => {
                    if state != Drag::Dragging {
                        return Err(Error::action("drag handle must be started before moving"));
                    }
                    (
                        "move",
                        params
                            .get("point")
                            .ok_or_else(|| Error::invalid("point required"))?,
                    )
                }
                _ => {
                    if state != Drag::Dragging {
                        return Err(Error::action("drag handle must be started before ending"));
                    }
                    ("mouse_up", params)
                }
            };
            self.provider.execute(command, point)?;
            self.sky_drags.insert(
                id,
                if method == "drag_end" {
                    Drag::Ended
                } else {
                    Drag::Dragging
                },
            );
            self.sky_settler.defer();
            return Ok(Value::Null);
        }
        if method == "get_screenshot" {
            let image = self.provider.sky_screenshot()?;
            if image["mime_type"] == "image/jpeg" {
                return Ok(json!([image]));
            }
            let png = STANDARD
                .decode(
                    image["data"]
                        .as_str()
                        .ok_or_else(|| Error::action("Screenshot bytes missing"))?,
                )
                .map_err(|_| Error::action("Invalid screenshot encoding"))?;
            let mut reader =
                image::ImageReader::new(std::io::Cursor::new(png)).with_guessed_format()?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(16384);
            limits.max_image_height = Some(16384);
            limits.max_alloc = Some(160 * 1024 * 1024);
            reader.limits(limits);
            let decoded = reader.decode().map_err(|error| {
                Error::action(format!("Cannot decode desktop screenshot: {error}"))
            })?;
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
                .encode_image(&decoded)
                .map_err(|error| Error::action(format!("Cannot encode desktop JPEG: {error}")))?;
            return Ok(json!([{"mime_type":"image/jpeg","data":STANDARD.encode(jpeg)}]));
        }
        let held_key = params
            .get("key")
            .filter(|_| method != "press_key")
            .map(|key| {
                key.as_str()
                    .filter(|key| {
                        !key.trim().is_empty() && !key.starts_with('-') && !key.contains('\0')
                    })
                    .map(str::to_owned)
                    .ok_or_else(|| Error::invalid("Invalid X11 held key"))
            })
            .transpose()?;
        // Validate every drag point before pressing any mouse button.
        if method == "drag" {
            let path = params["path"]
                .as_array()
                .filter(|path| path.len() >= 2 && path.len() <= 10_000)
                .ok_or_else(|| Error::invalid("drag path must contain 2..10000 points"))?;
            for point in path {
                coordinate(point, "x")?;
                coordinate(point, "y")?;
                #[cfg(target_os = "linux")]
                if matches!(self.provider, DesktopBackend::Native(_)) {
                    super::linux_x11::NativeX11::position(point)?;
                }
            }
        }
        if let Some(key) = &held_key {
            self.provider.key(true, key)?;
        }
        let operation = (|| -> Result<Value> {
            match method {
                "drag" => {
                    let path = params["path"].as_array().unwrap();
                    self.provider.execute("mouse_down", &path[0])?;
                    let movement = (|| -> Result<()> {
                        for point in &path[1..] {
                            self.provider.execute("move", point)?;
                        }
                        Ok(())
                    })();
                    let release = self.provider.execute("mouse_up", &json!({}));
                    movement?;
                    release?;
                    Ok(Value::Null)
                }
                "scroll" => {
                    let mut args = params.clone();
                    args["direction"] = json!(match required(params, "direction")?
                        .trim()
                        .to_lowercase()
                        .as_str()
                    {
                        "u" | "up" => "up",
                        "d" | "down" => "down",
                        "l" | "left" => "left",
                        "r" | "right" => "right",
                        _ => return Err(Error::invalid("Invalid scroll direction")),
                    });
                    if let Some(pixels) = params.get("pixels") {
                        let pixels = pixels
                            .as_f64()
                            .filter(|n| n.is_finite() && *n >= 0.)
                            .ok_or_else(|| {
                                Error::invalid("pixels must be finite and nonnegative")
                            })?;
                        if pixels == 0. {
                            return Ok(Value::Null);
                        }
                        // X11 core wheels expose discrete steps, not pixel scrolling.
                        args["steps"] = json!((pixels / 100.).ceil().clamp(1., 1000.) as u64);
                    }
                    self.provider.execute(method, &args)
                }
                "click" => {
                    let mut args = params.clone();
                    if let Some(button) = params.get("mouse_button").and_then(Value::as_str) {
                        args["mouse_button"] = json!(match button.trim().to_lowercase().as_str() {
                            "l" | "left" => "left",
                            "r" | "right" => "right",
                            "m" | "middle" => "middle",
                            _ => return Err(Error::invalid("Invalid mouse button")),
                        });
                    }
                    if params.get("duration").is_some() {
                        let duration = unsigned(params, "duration", 0)?;
                        if duration > 60_000 {
                            return Err(Error::invalid("duration exceeds 60000ms"));
                        }
                        let count = unsigned(params, "click_count", 1)?;
                        if !(1..=10).contains(&count) {
                            return Err(Error::invalid("click_count must be 1..10"));
                        }
                        for _ in 0..count {
                            self.provider.execute("mouse_down", &args)?;
                            thread::sleep(Duration::from_millis(duration));
                            self.provider.execute("mouse_up", &args)?;
                        }
                        Ok(Value::Null)
                    } else {
                        self.provider.execute(method, &args)
                    }
                }
                "press_key" if params.get("duration").is_some() => {
                    let key = required(params, "key")?;
                    if key.starts_with('-') || key.contains('\0') {
                        return Err(Error::invalid("Invalid X11 key chord"));
                    }
                    let duration = unsigned(params, "duration", 0)?;
                    if duration > 60_000 {
                        return Err(Error::invalid("duration exceeds 60000ms"));
                    }
                    self.provider.key(true, key)?;
                    thread::sleep(Duration::from_millis(duration));
                    self.provider.key(false, key)?;
                    Ok(Value::Null)
                }
                "move" | "press_key" | "type_text" => self.provider.execute(method, params),
                _ => Err(Error::unsupported(format!(
                    "Unsupported Linux Sky method: {method}"
                ))),
            }
        })();
        let released = held_key
            .map(|key| self.provider.key(false, &key))
            .transpose();
        operation?;
        released?;
        self.sky_settler.defer();
        Ok(Value::Null)
    }
    #[cfg(target_os = "linux")]
    fn audio(&mut self, method: &str, owner: &str, params: &Value) -> Result<Value> {
        self.audio
            .get_or_insert_with(super::linux_audio::recorder)
            .execute(method, owner, params)
    }
    #[cfg(target_os = "linux")]
    fn cancel_audio(&mut self, owner: &str) -> Result<()> {
        self.audio
            .as_mut()
            .map(|audio| audio.end_session(owner))
            .transpose()?;
        Ok(())
    }
    fn end_session(&mut self, _owner: &str) -> Result<()> {
        #[cfg(target_os = "linux")]
        let audio_cleanup = self
            .audio
            .as_mut()
            .map(|audio| audio.end_session(_owner))
            .transpose();
        if self
            .sky_drags
            .values()
            .any(|state| *state == Drag::Dragging)
        {
            self.provider.execute("mouse_up", &json!({}))?;
        }
        self.provider.release_all()?;
        self.sky_drags.clear();
        #[cfg(target_os = "linux")]
        audio_cleanup?;
        Ok(())
    }
    fn apps(&mut self) -> Result<Vec<crate::native::App>> {
        let ids = self.provider.execute("list_windows", &json!({}))?;
        let mut apps = Vec::new();
        for id in ids
            .as_array()
            .ok_or_else(|| Error::action("Invalid X11 window listing"))?
        {
            let state = self.provider.execute("get_window", &json!({"window":id}))?;
            let pid = state["pid"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            apps.push(crate::native::App {
                id: format!("x11:{}", id.as_u64().unwrap()),
                name: state["title"].as_str().unwrap_or("").into(),
                path: format!("x11:{}", id.as_u64().unwrap()),
                pid,
            });
        }
        Ok(apps)
    }
    fn snapshot(&mut self, app: &crate::native::App) -> Result<crate::ax::Node> {
        let state = self.state(app)?;
        Ok(crate::ax::Node {
            identity: app.id.clone(),
            role: "AXWindow".into(),
            title: state["title"].as_str().map(String::from),
            enabled: true,
            frame: Some(self.frame(app)?),
            ..Default::default()
        })
    }
    fn action(&mut self, app: &crate::native::App, action: crate::native::Action) -> Result<()> {
        use crate::native::Action;
        let window = Self::window(app)?;
        self.state(app)?;
        self.provider
            .execute("activate_window", &json!({"window":window}))?;
        let (method, params) = match action {
            Action::Click {
                target,
                button,
                count,
            } => {
                let [x, y] = self.point(app, target)?;
                let button = match button {
                    0 => "left",
                    1 => "right",
                    2 => "middle",
                    _ => return Err(Error::invalid("Invalid mouse button")),
                };
                (
                    "click",
                    json!({"x":x,"y":y,"mouse_button":button,"click_count":count}),
                )
            }
            Action::Drag { from, to } => {
                let frame = self.frame(app)?;
                let [x, y] = crate::native::window_point(frame, from)?;
                let [tx, ty] = crate::native::window_point(frame, to)?;
                ("drag", json!({"from_x":x,"from_y":y,"to_x":tx,"to_y":ty}))
            }
            Action::PressKey { key } => ("press_key", json!({"key":key})),
            Action::TypeText { text } => ("type_text", json!({"text":text})),
            Action::Scroll {
                target,
                direction,
                pages,
            } => {
                let [x, y] = self.point(app, target)?;
                if !pages.is_finite() || pages <= 0. || pages > 100. {
                    return Err(Error::invalid("Invalid scroll pages"));
                }
                (
                    "scroll",
                    json!({"x":x,"y":y,"direction":direction,"steps":(pages*3.).ceil()as u64}),
                )
            }
            _ => {
                return Err(Error::unsupported(
                    "X11 window backend has no AT-SPI element or transactional paste implementation",
                ));
            }
        };
        self.provider.execute(method, &params)?;
        Ok(())
    }
    fn screenshot(&mut self, app: &crate::native::App) -> Result<crate::native::Image> {
        self.state(app)?;
        let value = self
            .provider
            .execute("get_screenshot", &json!({"window":Self::window(app)?}))?;
        self.state(app)?;
        Ok(serde_json::from_value(value)?)
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec![
            "list_apps",
            "bind_app",
            "get_app_state",
            "get_screenshot",
            "click",
            "drag",
            "scroll",
            "press_key",
            "type_text",
        ]
    }
}
