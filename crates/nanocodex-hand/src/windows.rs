//! Primary-display capture and input for an interactive Windows user session.
//! The publisher owns authorization and input leases. Session 0 is unsupported.
#![allow(unsafe_code)]
use crate::Error;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{RgbImage, codecs::jpeg::JpegEncoder};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    mem::size_of,
    ptr::null_mut,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use windows_sys::Win32::{
    Graphics::Gdi::*,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
type Result<T> = std::result::Result<T, Error>;
fn error(message: &str) -> Error {
    std::io::Error::other(message.to_owned()).into()
}
#[derive(Default)]
struct Held {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u32>,
    relative_remainder: (f64, f64),
}
static HELD: OnceLock<Mutex<Held>> = OnceLock::new();

/// Capture never takes the input mutex: encoding cannot delay lease revocation.
pub fn ensure_interactive_session() -> Result<()> {
    use windows_sys::Win32::System::{
        RemoteDesktop::ProcessIdToSessionId, Threading::GetCurrentProcessId,
    };
    let mut session = 0;
    // SAFETY: writable session ID output and current process ID.
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 || session == 0 {
        return Err(error(
            "Windows screen requires the signed-in desktop helper; services in Session 0 cannot capture or control the desktop",
        ));
    }
    Ok(())
}

pub fn request(input: Value) -> Result<Value> {
    ensure_interactive_session()?;
    let lock = HELD.get_or_init(|| Mutex::new(Held::default()));
    let plan = serde_json::from_value::<Request>(input)
        .map_err(|_| error("invalid Windows screen request"))
        .and_then(Request::plan);
    if let Ok(plan) = &plan {
        if plan.steps.is_empty() && plan.observe {
            return capture();
        }
    }
    let mut held = match lock.lock() {
        Ok(held) => held,
        Err(poisoned) => {
            let mut held = poisoned.into_inner();
            let _ = held.release();
            lock.clear_poison();
            return Err(error("Windows input state recovered; retry request"));
        }
    };
    let result: Result<bool> = (|| {
        let plan = plan?;
        // An agent action promises a screenshot; preflight the active display.
        let _dpi = DpiGuard::new()?;
        if plan.observe {
            dimensions()?;
        }
        if plan.release || !plan.raw {
            held.release()?;
        }
        for step in plan.steps {
            if !step.delay.is_zero() {
                std::thread::sleep(step.delay);
            }
            held.apply(step.input)?;
        }
        if !plan.raw {
            held.release()?;
        }
        Ok(plan.observe)
    })();
    if result.is_err() {
        let _ = held.release();
    }
    drop(held);
    if result? {
        capture()
    } else {
        Ok(json!({"status":"ok"}))
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    action: String,
    x: Option<f64>,
    y: Option<f64>,
    end_x: Option<f64>,
    end_y: Option<f64>,
    button: Option<u32>,
    text: Option<String>,
    key: Option<u16>,
    #[serde(default)]
    modifiers: Vec<u16>,
    delta_x: Option<f64>,
    delta_y: Option<f64>,
    duration_ms: Option<u64>,
    input: Option<Raw>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Raw {
    kind: String,
    x: Option<f64>,
    y: Option<f64>,
    button: Option<u32>,
    down: Option<bool>,
    key: Option<u16>,
    text: Option<String>,
    delta_x: Option<f64>,
    delta_y: Option<f64>,
    // The publisher validates these lease fields before entering this module.
    #[serde(rename = "sequence")]
    _sequence: Option<u64>,
    #[serde(rename = "generation")]
    _generation: Option<String>,
}
#[derive(Debug)]
enum Input {
    Move(f64, f64),
    RelativeMove(f64, f64),
    Button(f64, f64, u32, bool),
    PointerButton(u32, bool),
    Key(u16, bool),
    Text(String),
    Scroll(f64, f64, i32, i32),
    PointerScroll(i32, i32),
    Release,
}
struct Step {
    input: Input,
    delay: Duration,
}
struct Plan {
    steps: Vec<Step>,
    raw: bool,
    release: bool,
    observe: bool,
}

fn coordinate(value: f64) -> Result<f64> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(error("screen coordinates must be finite numbers in [0,1]"))
    }
}
fn point(x: Option<f64>, y: Option<f64>) -> Result<(f64, f64)> {
    Ok((
        coordinate(x.ok_or_else(|| error("missing x coordinate"))?)?,
        coordinate(y.ok_or_else(|| error("missing y coordinate"))?)?,
    ))
}
fn delta(value: Option<f64>) -> Result<i32> {
    Ok(relative_delta(value)?.round() as i32)
}
fn relative_delta(value: Option<f64>) -> Result<f64> {
    let value = value.ok_or_else(|| error("missing input delta"))?;
    if !value.is_finite() || value.abs() > 4096.0 {
        return Err(error("input delta must be finite and at most 4096"));
    }
    Ok(value)
}
fn optional_point(x: Option<f64>, y: Option<f64>) -> Result<Option<(f64, f64)>> {
    match (x, y) {
        (None, None) => Ok(None),
        _ => point(x, y).map(Some),
    }
}
fn valid_text(text: Option<String>) -> Result<String> {
    let text = text.ok_or_else(|| error("missing input text"))?;
    if text.is_empty() || text.len() > 4096 || text.contains('\0') {
        return Err(error(
            "input text must contain 1..4096 UTF-8 bytes and no NUL",
        ));
    }
    Ok(text)
}
fn valid_key(key: Option<u16>) -> Result<u16> {
    let key = key.ok_or_else(|| error("missing HID key"))?;
    hid_to_scan(key).ok_or_else(|| error("unsupported HID keyboard usage"))?;
    Ok(key)
}
fn valid_button(button: u32) -> Result<u32> {
    if button > 2 {
        return Err(error("mouse button must be 0, 1, or 2"));
    }
    Ok(button)
}
impl Raw {
    fn validate(self) -> Result<Input> {
        // Reject fields belonging to another kind, including otherwise ignored coordinates.
        let fields: &[&str] = match self.kind.as_str() {
            "move" => &["x", "y"],
            "relativeMove" => &["deltaX", "deltaY"],
            "button" => &["x", "y", "button", "down"],
            "key" => &["key", "down"],
            "text" => &["text"],
            "scroll" => &["x", "y", "deltaX", "deltaY"],
            "releaseAll" => &[],
            _ => return Err(error("unsupported raw screen input kind")),
        };
        for (name, present) in [
            ("x", self.x.is_some()),
            ("y", self.y.is_some()),
            ("button", self.button.is_some()),
            ("down", self.down.is_some()),
            ("key", self.key.is_some()),
            ("text", self.text.is_some()),
            ("deltaX", self.delta_x.is_some()),
            ("deltaY", self.delta_y.is_some()),
        ] {
            let optional_coordinate =
                matches!(self.kind.as_str(), "button" | "scroll") && matches!(name, "x" | "y");
            if present != fields.contains(&name) && !optional_coordinate {
                return Err(error("invalid fields for raw screen input kind"));
            }
        }
        let down = || {
            self.down
                .ok_or_else(|| error("missing key or button state"))
        };
        Ok(match self.kind.as_str() {
            "move" => {
                let (x, y) = point(self.x, self.y)?;
                Input::Move(x, y)
            }
            "relativeMove" => {
                Input::RelativeMove(relative_delta(self.delta_x)?, relative_delta(self.delta_y)?)
            }
            "button" => {
                let button = valid_button(self.button.unwrap_or(3))?;
                match optional_point(self.x, self.y)? {
                    Some((x, y)) => Input::Button(x, y, button, down()?),
                    None => Input::PointerButton(button, down()?),
                }
            }
            "key" => Input::Key(valid_key(self.key)?, down()?),
            "text" => Input::Text(valid_text(self.text)?),
            "scroll" => {
                let (dx, dy) = (delta(self.delta_x)?, delta(self.delta_y)?);
                match optional_point(self.x, self.y)? {
                    Some((x, y)) => Input::Scroll(x, y, dx, dy),
                    None => Input::PointerScroll(dx, dy),
                }
            }
            "releaseAll" => Input::Release,
            _ => unreachable!(),
        })
    }
}
impl Request {
    fn plan(self) -> Result<Plan> {
        for v in [self.x, self.y, self.end_x, self.end_y]
            .into_iter()
            .flatten()
        {
            coordinate(v)?;
        }
        let mut steps = Vec::new();
        let mut add = |input| {
            steps.push(Step {
                input,
                delay: Duration::ZERO,
            })
        };
        match self.action.as_str() {
            "observe" | "release" => {}
            "input" => add(self
                .input
                .ok_or_else(|| error("missing raw input"))?
                .validate()?),
            "click" => {
                let (x, y) = point(self.x, self.y)?;
                let button = valid_button(self.button.unwrap_or(0))?;
                add(Input::Button(x, y, button, true));
                add(Input::Button(x, y, button, false));
            }
            "type" => add(Input::Text(valid_text(self.text)?)),
            "key" => {
                let key = valid_key(self.key)?;
                let mut seen = BTreeSet::new();
                if self.modifiers.len() > 4 {
                    return Err(error("at most four HID modifiers are allowed"));
                }
                for &modifier in &self.modifiers {
                    if !(224..=231).contains(&modifier) || !seen.insert(modifier) || modifier == key
                    {
                        return Err(error("invalid or duplicate HID modifier"));
                    }
                    add(Input::Key(modifier, true));
                }
                add(Input::Key(key, true));
                add(Input::Key(key, false));
                for modifier in self.modifiers.into_iter().rev() {
                    add(Input::Key(modifier, false));
                }
            }
            "scroll" => {
                let (x, y) = point(self.x, self.y)?;
                add(Input::Scroll(
                    x,
                    y,
                    delta(self.delta_x)?,
                    delta(self.delta_y)?,
                ));
            }
            "drag" => {
                let (x, y) = point(self.x, self.y)?;
                let (end_x, end_y) = point(self.end_x, self.end_y)?;
                let ms = self.duration_ms.unwrap_or(300);
                if !(50..=1500).contains(&ms) {
                    return Err(error("drag duration must be 50..1500 milliseconds"));
                }
                let button = valid_button(self.button.unwrap_or(0))?;
                add(Input::Button(x, y, button, true));
                let count = (ms / 33).max(2);
                for i in 1..=count {
                    let f = i as f64 / count as f64;
                    steps.push(Step {
                        input: Input::Move(x + (end_x - x) * f, y + (end_y - y) * f),
                        delay: Duration::from_millis(ms / count),
                    });
                }
                steps.push(Step {
                    input: Input::Button(end_x, end_y, button, false),
                    delay: Duration::ZERO,
                });
            }
            _ => return Err(error("unsupported screen action")),
        }
        Ok(Plan {
            steps,
            raw: self.action == "input",
            release: self.action == "release",
            observe: !matches!(self.action.as_str(), "input" | "release"),
        })
    }
}

/// Continuous primary-display H.264 capture used by the shared WebRTC publisher.
/// FFmpeg is optional; startup failure selects the native JPEG fallback.
pub fn video_command() -> Result<std::process::Command> {
    use std::os::windows::process::CommandExt;
    ensure_interactive_session()?;
    let _dpi = DpiGuard::new()?;
    let (width, height) = dimensions()?;
    let max_dimension = video_setting("NANOCODEX_SCREEN_MAX_DIMENSION", 1280, 1280, 7680)?;
    let bitrate = video_setting("NANOCODEX_SCREEN_BITRATE_KBPS", 6000, 1000, 100000)?;
    let scale = (max_dimension as f64 / width.max(height) as f64).min(1.0);
    let output_width = ((width as f64 * scale) as u32 / 2 * 2).max(2);
    let output_height = ((height as f64 * scale) as u32 / 2 * 2).max(2);
    // Bound the VBV buffer to approximately two 60 Hz frames.
    let buffer = format!("{}k", bitrate / 30);
    let level = video_level(output_width, output_height, bitrate);
    let bitrate = format!("{bitrate}k");
    let bundled = std::env::current_exe()?.with_file_name("ffmpeg.exe");
    let mut command = std::process::Command::new(if bundled.is_file() {
        bundled
    } else {
        std::path::PathBuf::from("ffmpeg.exe")
    });
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW for the desktop helper.
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "gdigrab",
        "-framerate",
        "60",
        "-draw_mouse",
        "1",
        "-offset_x",
        "0",
        "-offset_y",
        "0",
        "-video_size",
        &format!("{width}x{height}"),
        "-i",
        "desktop",
        "-an",
        "-r",
        "60",
        "-vf",
        &format!("scale={output_width}:{output_height}"),
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-profile:v",
        "baseline",
        "-level",
        level,
        "-pix_fmt",
        "yuv420p",
        "-b:v",
        &bitrate,
        "-maxrate",
        &bitrate,
        "-bufsize",
        &buffer,
        "-g",
        "30",
        "-bf",
        "0",
        "-x264-params",
        "repeat-headers=1:aud=1:scenecut=0",
        "-flush_packets",
        "1",
        "-f",
        "h264",
        "pipe:1",
    ]);
    Ok(command)
}

fn video_setting(name: &str, default: u32, min: u32, max: u32) -> Result<u32> {
    let value = match std::env::var(name) {
        Ok(value) => value.parse::<u32>()?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.into()),
    };
    if !(min..=max).contains(&value) {
        return Err(error(&format!("{name} must be between {min} and {max}")));
    }
    Ok(value)
}

fn video_level(width: u32, height: u32, bitrate_kbps: u32) -> &'static str {
    let macroblocks_per_second =
        u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16)) * 60;
    match macroblocks_per_second {
        0..=216000 if bitrate_kbps <= 20000 => "3.2",
        0..=522240 if bitrate_kbps <= 50000 => "4.2",
        0..=983040 => "5.1",
        0..=2073600 => "5.2",
        0..=4177920 => "6.0",
        0..=8355840 => "6.1",
        _ => "6.2",
    }
}

struct DpiGuard(DPI_AWARENESS_CONTEXT);
impl DpiGuard {
    fn new() -> Result<Self> {
        // SAFETY: thread-local awareness is restored before returning the worker.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.is_null() {
            return Err(error("could not set Windows screen DPI awareness"));
        }
        Ok(Self(previous))
    }
}
impl Drop for DpiGuard {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}
fn dimensions() -> Result<(i32, i32)> {
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    if !(1..=32768).contains(&w) || !(1..=32768).contains(&h) {
        return Err(error(
            "Windows screen unavailable; run the Hand in the signed-in desktop session",
        ));
    }
    Ok((w, h))
}
fn send(input: INPUT) -> Result<()> {
    // SAFETY: input is initialized and remains live for the synchronous call.
    if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } != 1 {
        return Err(error(
            "Windows input blocked; the Hand must run in the interactive desktop at the target application's integrity level",
        ));
    }
    Ok(())
}
fn keyboard(scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
fn mouse(flags: u32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
impl Held {
    fn key(&mut self, key: u16, down: bool) -> Result<()> {
        let (scan, extended) =
            hid_to_scan(key).ok_or_else(|| error("unsupported HID keyboard usage"))?;
        send(keyboard(
            scan,
            KEYEVENTF_SCANCODE
                | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }
                | if down { 0 } else { KEYEVENTF_KEYUP },
        ))?;
        if down {
            self.keys.insert(key);
        } else {
            self.keys.remove(&key);
        }
        Ok(())
    }
    fn button(&mut self, button: u32, down: bool) -> Result<()> {
        let flags = match (button, down) {
            (0, true) => MOUSEEVENTF_LEFTDOWN,
            (0, false) => MOUSEEVENTF_LEFTUP,
            (1, true) => MOUSEEVENTF_RIGHTDOWN,
            (1, false) => MOUSEEVENTF_RIGHTUP,
            (2, true) => MOUSEEVENTF_MIDDLEDOWN,
            (2, false) => MOUSEEVENTF_MIDDLEUP,
            _ => return Err(error("unsupported mouse button")),
        };
        send(mouse(flags, 0))?;
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
        Ok(())
    }
    fn release(&mut self) -> Result<()> {
        self.relative_remainder = (0.0, 0.0);
        let mut failure = None;
        for key in self.keys.clone() {
            if let Err(e) = self.key(key, false) {
                failure = Some(e);
            }
        }
        for button in self.buttons.clone() {
            if let Err(e) = self.button(button, false) {
                failure = Some(e);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn apply(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Move(x, y) => move_pointer(x, y),
            Input::RelativeMove(dx, dy) => {
                let (dx, dy) = relative_motion(&mut self.relative_remainder, dx, dy);
                if dx == 0 && dy == 0 {
                    return Ok(());
                }
                let mut input = mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE, 0);
                // SAFETY: mouse() initialized this union as MOUSEINPUT.
                unsafe {
                    input.Anonymous.mi.dx = dx;
                    input.Anonymous.mi.dy = dy;
                }
                send(input)
            }
            Input::Button(x, y, button, down) => {
                move_pointer(x, y)?;
                self.button(button, down)
            }
            Input::PointerButton(button, down) => self.button(button, down),
            Input::Key(key, down) => self.key(key, down),
            Input::Release => self.release(),
            Input::Text(text) => {
                for unit in text.encode_utf16() {
                    send(keyboard(unit, KEYEVENTF_UNICODE))?;
                    if let Err(e) = send(keyboard(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)) {
                        let _ = send(keyboard(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                        return Err(e);
                    }
                }
                Ok(())
            }
            Input::Scroll(x, y, dx, dy) => {
                move_pointer(x, y)?;
                scroll_pointer(dx, dy)
            }
            Input::PointerScroll(dx, dy) => scroll_pointer(dx, dy),
        }
    }
}
fn relative_motion(remainder: &mut (f64, f64), dx: f64, dy: f64) -> (i32, i32) {
    let (x, y) = (remainder.0 + dx, remainder.1 + dy);
    let pixels = (x.trunc() as i32, y.trunc() as i32);
    *remainder = (x - f64::from(pixels.0), y - f64::from(pixels.1));
    pixels
}
fn scroll_pointer(dx: i32, dy: i32) -> Result<()> {
    if dx != 0 {
        send(mouse(MOUSEEVENTF_HWHEEL, dx as u32))?;
    }
    if dy != 0 {
        send(mouse(MOUSEEVENTF_WHEEL, (-dy) as u32))?;
    }
    Ok(())
}
fn move_pointer(x: f64, y: f64) -> Result<()> {
    let (width, height) = dimensions()?;
    let (x, y) = (
        (coordinate(x)? * (width - 1) as f64).round() as i32,
        (coordinate(y)? * (height - 1) as f64).round() as i32,
    );
    // SAFETY: bounded physical primary-display coordinates in the current session.
    if unsafe { SetCursorPos(x, y) } == 0 {
        return Err(error("could not move Windows pointer"));
    }
    Ok(())
}
/// USB HID usages to set-1 physical scancodes. The extended flag distinguishes
/// navigation/right modifiers from numeric-pad/left modifier keys.
fn hid_to_scan(hid: u16) -> Option<(u16, bool)> {
    const LETTERS: [u16; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
    let code = match hid {
        4..=29 => LETTERS[(hid - 4) as usize],
        30..=38 => hid - 28,
        39 => 0x0b,
        40 => 0x1c,
        41 => 0x01,
        42 => 0x0e,
        43 => 0x0f,
        44 => 0x39,
        45 => 0x0c,
        46 => 0x0d,
        47 => 0x1a,
        48 => 0x1b,
        49 | 50 => 0x2b,
        51 => 0x27,
        52 => 0x28,
        53 => 0x29,
        54 => 0x33,
        55 => 0x34,
        56 => 0x35,
        57 => 0x3a,
        58..=67 => hid - 58 + 0x3b,
        68 => 0x57,
        69 => 0x58,
        70 => 0x137,
        71 => 0x46,
        73 => 0x152,
        74 => 0x147,
        75 => 0x149,
        76 => 0x153,
        77 => 0x14f,
        78 => 0x151,
        79 => 0x14d,
        80 => 0x14b,
        81 => 0x150,
        82 => 0x148,
        83 => 0x45,
        84 => 0x135,
        85 => 0x37,
        86 => 0x4a,
        87 => 0x4e,
        88 => 0x11c,
        89 => 0x4f,
        90 => 0x50,
        91 => 0x51,
        92 => 0x4b,
        93 => 0x4c,
        94 => 0x4d,
        95 => 0x47,
        96 => 0x48,
        97 => 0x49,
        98 => 0x52,
        99 => 0x53,
        100 => 0x56,
        101 => 0x15d,
        104..=114 => hid - 104 + 0x64,
        115 => 0x76,
        224 => 0x1d,
        225 => 0x2a,
        226 => 0x38,
        227 => 0x15b,
        228 => 0x11d,
        229 => 0x36,
        230 => 0x138,
        231 => 0x15c,
        _ => return None,
    };
    Some((code & 0xff, code > 0xff))
}

struct Capture {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: only non-null handles allocated on this thread are released.
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}
fn capture() -> Result<Value> {
    let _dpi = DpiGuard::new()?;
    let (source_width, source_height) = dimensions()?;
    let scale = (1280.0 / source_width.max(source_height) as f64).min(1.0);
    let width = (source_width as f64 * scale).round().max(1.0) as i32;
    let height = (source_height as f64 * scale).round().max(1.0) as i32;
    let mut resources = Capture {
        screen: null_mut(),
        memory: null_mut(),
        bitmap: null_mut(),
        previous: null_mut(),
    };
    // SAFETY: DIB dimensions are capped at 1280², buffer is read only while the
    // selected bitmap lives. GdiFlush completes writes before accessing its bits.
    unsafe {
        resources.screen = GetDC(null_mut());
        if resources.screen.is_null() {
            return Err(error("Windows desktop DC unavailable"));
        }
        resources.memory = CreateCompatibleDC(resources.screen);
        if resources.memory.is_null() {
            return Err(error("could not allocate Windows capture DC"));
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        resources.bitmap = CreateDIBSection(
            resources.screen,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        );
        if resources.bitmap.is_null() || bits.is_null() {
            return Err(error("could not allocate Windows capture bitmap"));
        }
        resources.previous = SelectObject(resources.memory, resources.bitmap);
        if resources.previous.is_null() || resources.previous as isize == -1 {
            resources.previous = null_mut();
            return Err(error("could not select Windows capture bitmap"));
        }
        SetStretchBltMode(resources.memory, COLORONCOLOR);
        if StretchBlt(
            resources.memory,
            0,
            0,
            width,
            height,
            resources.screen,
            0,
            0,
            source_width,
            source_height,
            SRCCOPY | CAPTUREBLT,
        ) == 0
            || GdiFlush() == 0
        {
            return Err(error(
                "Windows capture failed; unlock the interactive desktop",
            ));
        }
        let bytes =
            std::slice::from_raw_parts(bits.cast::<u8>(), width as usize * height as usize * 4);
        let rgb = bytes
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect();
        let frame = RgbImage::from_raw(width as u32, height as u32, rgb)
            .ok_or_else(|| error("invalid Windows capture bitmap"))?;
        drop(resources);
        encode_jpeg(&frame)
    }
}

fn encode_jpeg(frame: &RgbImage) -> Result<Value> {
    // Bound the base64 representation itself, stricter than a 500k JPEG bound.
    for quality in [65, 50, 35, 20, 10] {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(frame)
            .map_err(|_| error("could not encode screen JPEG"))?;
        if bytes.len().div_ceil(3) * 4 <= 500_000 {
            return Ok(
                json!({"status":"ok","jpeg":STANDARD.encode(bytes),"width":frame.width(),"height":frame.height()}),
            );
        }
    }
    Err(error("screen JPEG exceeds the 500000-byte transport limit"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn video_level_covers_60hz_resolution_and_bitrate() {
        assert_eq!(video_level(1280, 720, 6000), "3.2");
        assert_eq!(video_level(1280, 800, 6000), "4.2");
        assert_eq!(video_level(1280, 720, 40000), "4.2");
        assert_eq!(video_level(1920, 1080, 20000), "4.2");
        assert_eq!(video_level(2560, 1440, 40000), "5.1");
        assert_eq!(video_level(3840, 2160, 40000), "5.2");
        assert_eq!(video_level(7680, 4320, 100000), "6.1");
    }
    #[test]
    fn relative_input_keeps_fractional_motion_and_rejects_invalid_fields() {
        let mut remainder = (0.0, 0.0);
        assert_eq!(relative_motion(&mut remainder, 0.25, -0.5), (0, 0));
        assert_eq!(relative_motion(&mut remainder, 0.75, -0.5), (1, -1));
        for input in [
            json!({"kind":"relativeMove","deltaX":1.5,"deltaY":-2.5}),
            json!({"kind":"button","button":0,"down":true}),
            json!({"kind":"scroll","deltaX":0,"deltaY":120}),
        ] {
            assert!(plan(json!({"action":"input","input":input})).is_ok());
        }
        for input in [
            json!({"kind":"relativeMove","deltaX":4097,"deltaY":0}),
            json!({"kind":"relativeMove","deltaX":0}),
            json!({"kind":"relativeMove","deltaX":0,"deltaY":0,"x":0.5}),
            json!({"kind":"button","button":0,"down":true,"x":0.5}),
            json!({"kind":"button","button":0,"down":true,"y":0.5}),
            json!({"kind":"scroll","deltaX":0,"deltaY":1,"x":0.5}),
        ] {
            assert!(plan(json!({"action":"input","input":input})).is_err());
        }
        assert!(relative_delta(Some(f64::NAN)).is_err());
        assert!(relative_delta(Some(f64::INFINITY)).is_err());
    }
    fn plan(value: Value) -> Result<Plan> {
        serde_json::from_value::<Request>(value)
            .map_err(|_| error("invalid request"))?
            .plan()
    }
    #[test]
    fn hid_mapping_distinguishes_extended_keys_and_rejects_unknown_usages() {
        for key in 4..=69 {
            assert!(hid_to_scan(key).is_some(), "{key}");
        }
        for key in 224..=231 {
            assert!(hid_to_scan(key).is_some());
        }
        assert_eq!(hid_to_scan(4), Some((0x1e, false)));
        assert_eq!(hid_to_scan(40), Some((0x1c, false)));
        assert_eq!(hid_to_scan(88), Some((0x1c, true)));
        assert_eq!(hid_to_scan(224), Some((0x1d, false)));
        assert_eq!(hid_to_scan(228), Some((0x1d, true)));
        assert_eq!(hid_to_scan(79), Some((0x4d, true)));
        for key in [0, 3, 116, 223, 232, u16::MAX] {
            assert!(hid_to_scan(key).is_none());
        }
    }
    #[test]
    fn invalid_actions_are_rejected_before_input_is_posted() {
        for value in [
            json!({"action":"click","x":1.1,"y":0}),
            json!({"action":"click","x":0,"y":0,"button":3}),
            json!({"action":"key","key":4,"modifiers":[225,225]}),
            json!({"action":"key","key":4,"modifiers":[4]}),
            json!({"action":"type","text":"a\u{0}b"}),
            json!({"action":"type","text":"x".repeat(4097)}),
            json!({"action":"drag","x":0,"y":0,"endX":1,"endY":1,"durationMs":1501}),
            json!({"action":"scroll","x":0,"y":0,"deltaX":4097,"deltaY":0}),
            json!({"action":"input","input":{"kind":"key","key":4,"down":true,"x":0}}),
        ] {
            assert!(plan(value).is_err());
        }
    }
    #[test]
    fn key_chord_and_drag_plan_release_their_inputs() {
        let chord = plan(json!({"action":"key","key":4,"modifiers":[227,225]})).unwrap();
        assert!(matches!(chord.steps[0].input, Input::Key(227, true)));
        assert!(matches!(
            chord.steps.last().unwrap().input,
            Input::Key(227, false)
        ));
        let drag = plan(json!({"action":"drag","x":0,"y":0,"endX":1,"endY":1})).unwrap();
        assert!(matches!(
            drag.steps.last().unwrap().input,
            Input::Button(1.0, 1.0, 0, false)
        ));
        assert!(drag.steps.iter().map(|s| s.delay).sum::<Duration>() <= Duration::from_millis(300));
        assert!(
            plan(json!({"action":"input","input":{"kind":"text","text":"héllo 🦀"}}))
                .unwrap()
                .raw
        );
    }
    #[test]
    fn noisy_frame_fits_base64_budget_and_decodes() {
        let mut random = 1u32;
        let frame = RgbImage::from_fn(1280, 720, |_, _| {
            image::Rgb(std::array::from_fn(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random as u8
            }))
        });
        let result = encode_jpeg(&frame).unwrap();
        let jpeg = result["jpeg"].as_str().unwrap();
        assert!(jpeg.len() <= 500_000);
        let decoded = image::load_from_memory(&STANDARD.decode(jpeg).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1280, 720));
    }
}
