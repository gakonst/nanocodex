//! Native main-display observation and input. Called on a blocking worker.
//! Authorization and raw-input lease ownership belong to the publisher.
#![allow(unsafe_code)]

use crate::{
    Error,
    capture::{encode_jpeg, target_dimensions},
};
use block2::RcBlock;
use image::RgbImage;
use objc2::{rc::autoreleasepool, runtime::AnyClass, sel};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::*;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCScreenshotConfiguration, SCScreenshotDynamicRange, SCScreenshotManager, SCScreenshotOutput,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Mutex, OnceLock, mpsc},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Error>;
fn error(message: &str) -> Error {
    std::io::Error::other(message.to_owned()).into()
}
const CAPTURE_PERMISSION: &str = "macOS screen capture unavailable: enable Screen & System Audio Recording for this application in System Settings > Privacy & Security, then relaunch it if macOS requests it";
const INPUT_PERMISSION: &str = "macOS input unavailable: enable Accessibility for this application in System Settings > Privacy & Security";

/// AVFoundation enumerates capture screens in CoreGraphics active-display order.
/// Resolve the same main display used by screenshots and normalized input.
pub fn main_display_index() -> Result<usize> {
    let mut displays = [0; 32];
    let mut count = 0;
    // SAFETY: the array and count remain writable for this synchronous call.
    let status =
        unsafe { CGGetActiveDisplayList(displays.len() as u32, displays.as_mut_ptr(), &mut count) };
    if status != CGError::Success || count as usize > displays.len() {
        return Err(error("active displays unavailable"));
    }
    displays[..count as usize]
        .iter()
        .position(|id| *id == CGMainDisplayID())
        .ok_or_else(|| error("main display unavailable"))
}

/// Physical pixel dimensions of the main display, for native-resolution video.
pub fn main_display_pixel_dimensions() -> Result<(u32, u32)> {
    let display = CGMainDisplayID();
    let width = u32::try_from(CGDisplayPixelsWide(display))?;
    let height = u32::try_from(CGDisplayPixelsHigh(display))?;
    if width < 2 || height < 2 {
        return Err(error("main display unavailable"));
    }
    Ok((width, height))
}

#[derive(Default)]
struct Held {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u32>,
    position: CGPoint,
}
static HELD: OnceLock<Mutex<Held>> = OnceLock::new();

/// Input, validation failures, and release share one input lock. Pure screen
/// observations do not hold it while ScreenCaptureKit captures or JPEG encodes.
/// No permission prompts are opened here; preflight checks return diagnostics.
pub fn request(input: Value) -> Result<Value> {
    // Live viewers must not make key-up or pointer events wait behind a screen
    // capture. Only the exact read-only request can take this path; malformed
    // actions and compound agent operations retain serialized validation/input.
    let observation_error = if input == json!({"action":"observe"}) {
        match autoreleasepool(|_| capture()) {
            Ok(frame) => return Ok(frame),
            Err(error) => Some(error),
        }
    } else {
        None
    };
    let lock = HELD.get_or_init(|| Mutex::new(Held::default()));
    let mut held = match lock.lock() {
        Ok(held) => held,
        Err(poisoned) => {
            let mut held = poisoned.into_inner();
            let _ = held.release();
            lock.clear_poison();
            return Err(error(
                "macOS input state recovered after an interrupted request; retry",
            ));
        }
    };
    if let Some(error) = observation_error {
        // Preserve the existing failure cleanup, including permission changes.
        let _ = held.release();
        return Err(error);
    }
    let result = autoreleasepool(|_| {
        let request: Request =
            serde_json::from_value(input).map_err(|_| error("invalid macOS screen request"))?;
        let plan = request.plan()?; // Validate the complete action before posting anything.
        // Agent actions promise an observation. Check that prerequisite before
        // modifying the desktop so a missing recording grant has no side effects.
        if plan.observe {
            capture_available()?;
        }
        if plan.release {
            held.release()?;
        }
        if !plan.steps.is_empty() {
            if !CGPreflightPostEventAccess() {
                return Err(error(INPUT_PERMISSION));
            }
            let bounds = display_bounds()?;
            if !plan.raw {
                held.release()?;
            }
            for step in plan.steps {
                if !step.delay.is_zero() {
                    std::thread::sleep(step.delay);
                }
                held.apply(step.input, bounds)?;
            }
            if !plan.raw {
                held.release()?;
            }
        }
        if plan.observe {
            capture()
        } else {
            Ok(json!({"status":"ok"}))
        }
    });
    if result.is_err() {
        let _ = held.release();
    }
    result
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
    Button(f64, f64, u32, bool),
    Key(u16, bool),
    Text(String),
    Scroll(f64, f64, i32, i32),
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
    let value = value.ok_or_else(|| error("missing scroll delta"))?;
    if !value.is_finite() || value.abs() > 4096.0 {
        return Err(error("scroll delta must be finite and at most 4096"));
    }
    Ok(value.round() as i32)
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
    hid_to_mac(key).ok_or_else(|| error("unsupported HID keyboard usage"))?;
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
            if present != fields.contains(&name) {
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
            "button" => {
                let (x, y) = point(self.x, self.y)?;
                Input::Button(x, y, valid_button(self.button.unwrap_or(3))?, down()?)
            }
            "key" => Input::Key(valid_key(self.key)?, down()?),
            "text" => Input::Text(valid_text(self.text)?),
            "scroll" => {
                let (x, y) = point(self.x, self.y)?;
                Input::Scroll(x, y, delta(self.delta_x)?, delta(self.delta_y)?)
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

fn display_bounds() -> Result<CGRect> {
    let bounds = CGDisplayBounds(CGMainDisplayID());
    if !bounds.origin.x.is_finite()
        || !bounds.origin.y.is_finite()
        || !bounds.size.width.is_finite()
        || !bounds.size.height.is_finite()
        || bounds.size.width < 1.0
        || bounds.size.height < 1.0
        || bounds.size.width > 65536.0
        || bounds.size.height > 65536.0
    {
        return Err(error(
            "main display is unavailable or has invalid dimensions",
        ));
    }
    Ok(bounds)
}
fn logical_point(x: f64, y: f64, bounds: CGRect) -> Result<CGPoint> {
    // CGEvent uses top-left display points, not the Retina framebuffer pixels.
    // At 1.0 keep the event inside the last point of the selected display.
    Ok(CGPoint::new(
        bounds.origin.x + coordinate(x)? * (bounds.size.width - 1.0),
        bounds.origin.y + coordinate(y)? * (bounds.size.height - 1.0),
    ))
}
fn modifier_flags(keys: &BTreeSet<u16>) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    for key in keys {
        flags |= match key {
            224 | 228 => CGEventFlags::MaskControl,
            225 | 229 => CGEventFlags::MaskShift,
            226 | 230 => CGEventFlags::MaskAlternate,
            227 | 231 => CGEventFlags::MaskCommand,
            _ => CGEventFlags::empty(),
        };
    }
    flags
}
impl Held {
    fn post(&self, event: Option<objc2_core_foundation::CFRetained<CGEvent>>) -> Result<()> {
        let event = event.ok_or_else(|| error("macOS could not allocate an input event"))?;
        CGEvent::set_flags(Some(&event), modifier_flags(&self.keys));
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        Ok(())
    }
    fn key(&mut self, key: u16, down: bool) -> Result<()> {
        let mac = hid_to_mac(key).ok_or_else(|| error("unsupported HID keyboard usage"))?;
        let event = CGEvent::new_keyboard_event(None, mac, down)
            .ok_or_else(|| error("macOS could not allocate a keyboard event"))?;
        if down {
            self.keys.insert(key);
        } else {
            self.keys.remove(&key);
        }
        self.post(Some(event))
    }
    fn button(&mut self, button: u32, down: bool) -> Result<()> {
        let kind = match (button, down) {
            (0, true) => CGEventType::LeftMouseDown,
            (0, false) => CGEventType::LeftMouseUp,
            (1, true) => CGEventType::RightMouseDown,
            (1, false) => CGEventType::RightMouseUp,
            (_, true) => CGEventType::OtherMouseDown,
            (_, false) => CGEventType::OtherMouseUp,
        };
        let event = CGEvent::new_mouse_event(None, kind, self.position, CGMouseButton(button));
        self.post(event)?;
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
        Ok(())
    }
    fn release(&mut self) -> Result<()> {
        if (!self.keys.is_empty() || !self.buttons.is_empty()) && !CGPreflightPostEventAccess() {
            // Permission can be revoked mid-lease. Keep the state for a later
            // cleanup attempt; CGEventPost has no delivery acknowledgement.
            return Err(error(INPUT_PERMISSION));
        }
        // Attempt every release even if an individual event cannot be allocated.
        // Keep any failed release in the set so the next request can retry it.
        let mut failure = None;
        for key in self.keys.clone() {
            if let Err(err) = self.key(key, false) {
                failure = Some(err);
            }
        }
        for button in self.buttons.clone() {
            if let Err(err) = self.button(button, false) {
                failure = Some(err);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn apply(&mut self, input: Input, bounds: CGRect) -> Result<()> {
        match input {
            Input::Move(x, y) => {
                self.position = logical_point(x, y, bounds)?;
                let button = self.buttons.first().copied().unwrap_or(0);
                let kind = if self.buttons.is_empty() {
                    CGEventType::MouseMoved
                } else {
                    match button {
                        0 => CGEventType::LeftMouseDragged,
                        1 => CGEventType::RightMouseDragged,
                        _ => CGEventType::OtherMouseDragged,
                    }
                };
                self.post(CGEvent::new_mouse_event(
                    None,
                    kind,
                    self.position,
                    CGMouseButton(button),
                ))
            }
            Input::Button(x, y, button, down) => {
                self.position = logical_point(x, y, bounds)?;
                self.button(button, down)
            }
            Input::Key(key, down) => self.key(key, down),
            Input::Text(text) => {
                // One Unicode scalar per pair avoids truncation by event consumers,
                // and never splits a UTF-16 surrogate pair. No clipboard mutation.
                for character in text.chars() {
                    let mut buffer = [0u16; 2];
                    let utf16 = character.encode_utf16(&mut buffer);
                    let down = CGEvent::new_keyboard_event(None, 0, true)
                        .ok_or_else(|| error("macOS could not allocate a Unicode event"))?;
                    let up = CGEvent::new_keyboard_event(None, 0, false)
                        .ok_or_else(|| error("macOS could not allocate a Unicode event"))?;
                    // SAFETY: utf16 is a live buffer with exactly the supplied length.
                    unsafe {
                        CGEvent::keyboard_set_unicode_string(
                            Some(&down),
                            utf16.len() as _,
                            utf16.as_ptr(),
                        );
                        CGEvent::keyboard_set_unicode_string(
                            Some(&up),
                            utf16.len() as _,
                            utf16.as_ptr(),
                        );
                    }
                    self.post(Some(down))?;
                    self.post(Some(up))?;
                }
                Ok(())
            }
            Input::Scroll(x, y, dx, dy) => {
                self.position = logical_point(x, y, bounds)?;
                let event =
                    CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 2, dy, dx, 0)
                        .ok_or_else(|| error("macOS could not allocate a scroll event"))?;
                CGEvent::set_location(Some(&event), self.position);
                self.post(Some(event))
            }
            Input::Release => self.release(),
        }
    }
}

/// USB HID keyboard usages to Apple's hardware-independent virtual key codes.
fn hid_to_mac(hid: u16) -> Option<u16> {
    Some(match hid {
        4 => 0,
        5 => 11,
        6 => 8,
        7 => 2,
        8 => 14,
        9 => 3,
        10 => 5,
        11 => 4,
        12 => 34,
        13 => 38,
        14 => 40,
        15 => 37,
        16 => 46,
        17 => 45,
        18 => 31,
        19 => 35,
        20 => 12,
        21 => 15,
        22 => 1,
        23 => 17,
        24 => 32,
        25 => 9,
        26 => 13,
        27 => 7,
        28 => 16,
        29 => 6,
        30 => 18,
        31 => 19,
        32 => 20,
        33 => 21,
        34 => 23,
        35 => 22,
        36 => 26,
        37 => 28,
        38 => 25,
        39 => 29,
        40 => 36,
        41 => 53,
        42 => 51,
        43 => 48,
        44 => 49,
        45 => 27,
        46 => 24,
        47 => 33,
        48 => 30,
        49 => 42,
        50 => 42,
        51 => 41,
        52 => 39,
        53 => 50,
        54 => 43,
        55 => 47,
        56 => 44,
        57 => 57,
        58 => 122,
        59 => 120,
        60 => 99,
        61 => 118,
        62 => 96,
        63 => 97,
        64 => 98,
        65 => 100,
        66 => 101,
        67 => 109,
        68 => 103,
        69 => 111,
        70 => 105,
        71 => 107,
        72 => 113,
        73 => 114,
        74 => 115,
        75 => 116,
        76 => 117,
        77 => 119,
        78 => 121,
        79 => 124,
        80 => 123,
        81 => 125,
        82 => 126,
        83 => 71,
        84 => 75,
        85 => 67,
        86 => 78,
        87 => 69,
        88 => 76,
        89 => 83,
        90 => 84,
        91 => 85,
        92 => 86,
        93 => 87,
        94 => 88,
        95 => 89,
        96 => 91,
        97 => 92,
        98 => 82,
        99 => 65,
        100 => 10,
        101 => 110,
        103 => 81,
        104 => 105,
        105 => 107,
        106 => 113,
        107 => 106,
        108 => 64,
        109 => 79,
        110 => 80,
        111 => 90,
        224 => 59,
        225 => 56,
        226 => 58,
        227 => 55,
        228 => 62,
        229 => 60,
        230 => 61,
        231 => 54,
        _ => return None,
    })
}

fn capture_available() -> Result<()> {
    if !CGPreflightScreenCaptureAccess() {
        return Err(error(CAPTURE_PERMISSION));
    }
    if AnyClass::get(c"SCScreenshotConfiguration").is_none()
        || !AnyClass::get(c"SCScreenshotManager").is_some_and(|class| {
            class
                .class_method(sel!(captureScreenshotWithRect:configuration:completionHandler:))
                .is_some()
        })
    {
        return Err(error("native screen capture requires macOS 26 or later"));
    }
    Ok(())
}
fn capture() -> Result<Value> {
    let began = Instant::now();
    capture_available()?;
    // Resolve current main-display bounds every time; no window enumeration,
    // cached pixels, or stale display metadata after hot-plug.
    let bounds = display_bounds()?;
    let (width, height) = target_dimensions(bounds.size.width, bounds.size.height)?;
    capture_rect(bounds, width, height, began)
}
/// The availability check in capture() guards all macOS 26-only objects.
fn capture_rect(bounds: CGRect, width: u32, height: u32, began: Instant) -> Result<Value> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let completion = RcBlock::new(move |output: *mut SCScreenshotOutput, err: *mut NSError| {
        autoreleasepool(|_| {
            let image_ms = began.elapsed().as_secs_f64() * 1000.0;
            let encode_started = Instant::now();
            // SAFETY: callback arguments are owned by ScreenCaptureKit for this
            // invocation; the retained SDR image lives through conversion.
            let result = unsafe {
                output.as_ref().filter(|_| err.is_null()).and_then(|output| output.sdrImage())
            }.ok_or_else(|| error("ScreenCaptureKit could not capture the main display; check Screen Recording permission and the active desktop session"))
                .and_then(|image| encode_capture(&image));
            tracing::debug!(target: "nanocodex_hand", stage = "screen.macos.capture", path = "rectangle",
                content_ms = 0.0, image_ms, encode_ms = encode_started.elapsed().as_secs_f64() * 1000.0,
                total_ms = began.elapsed().as_secs_f64() * 1000.0, success = result.is_ok());
            let _ = sender.send(result);
        });
    });
    // SAFETY: capture() verifies API availability. The async API copies the
    // completion block and retains configuration for the request.
    unsafe {
        let configuration = SCScreenshotConfiguration::new();
        configuration.setWidth(width as isize);
        configuration.setHeight(height as isize);
        configuration.setShowsCursor(true);
        configuration.setDynamicRange(SCScreenshotDynamicRange::SDR);
        SCScreenshotManager::captureScreenshotWithRect_configuration_completionHandler(
            bounds,
            &configuration,
            Some(&completion),
        );
    }
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| error("ScreenCaptureKit timed out waiting for the active desktop"))?
}
fn encode_capture(image: &CGImage) -> Result<Value> {
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    if target_dimensions(width as f64, height as f64)? != (width as u32, height as u32) {
        return Err(error("unexpected screen capture dimensions"));
    }
    let mut rgba = vec![0u8; width * height * 4];
    let space = CGColorSpace::new_device_rgb()
        .ok_or_else(|| error("could not create capture color space"))?;
    // SAFETY: the backing buffer covers height * bytes_per_row and outlives the
    // context, which is dropped before Rust reads that buffer. Big-endian RGBA,
    // opaque skip-last avoids interpreting the source image's BGRA/HDR layout.
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            Some(&space),
            (1 << 14) | 5,
        )
    }
    .ok_or_else(|| error("could not create capture bitmap"))?;
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(width as f64, height as f64),
        ),
        Some(image),
    );
    drop(context);
    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let frame = RgbImage::from_raw(width as u32, height as u32, rgb)
        .ok_or_else(|| error("invalid capture bitmap"))?;
    encode_jpeg(&frame)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn plan(value: Value) -> Result<Plan> {
        serde_json::from_value::<Request>(value)
            .map_err(|_| error("invalid request"))?
            .plan()
    }
    #[test]
    fn hid_mapping_covers_letters_navigation_and_both_modifier_sides() {
        for hid in 4..=29 {
            assert!(hid_to_mac(hid).is_some());
        }
        assert_eq!(hid_to_mac(4), Some(0));
        assert_eq!(hid_to_mac(40), Some(36));
        assert_eq!(hid_to_mac(79), Some(124));
        assert_eq!(hid_to_mac(80), Some(123));
        assert_eq!(hid_to_mac(227), Some(55));
        assert_eq!(hid_to_mac(231), Some(54));
        for hid in [0, 1, 3, 112, 223, 232, u16::MAX] {
            assert_eq!(hid_to_mac(hid), None);
        }
        let flags = modifier_flags(&BTreeSet::from([225, 229, 227]));
        assert!(flags.contains(CGEventFlags::MaskShift | CGEventFlags::MaskCommand));
        assert!(!flags.contains(CGEventFlags::MaskControl));
    }
    #[test]
    fn normalized_coordinates_use_logical_bounds_and_stay_inside() {
        let bounds = CGRect::new(CGPoint::new(-1440.0, 100.0), CGSize::new(1440.0, 900.0));
        assert_eq!(
            logical_point(0.0, 0.0, bounds).unwrap(),
            CGPoint::new(-1440.0, 100.0)
        );
        assert_eq!(
            logical_point(1.0, 1.0, bounds).unwrap(),
            CGPoint::new(-1.0, 999.0)
        );
        assert_eq!(
            logical_point(0.5, 0.5, bounds).unwrap(),
            CGPoint::new(-720.5, 549.5)
        );
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.001, 1.001] {
            assert!(coordinate(invalid).is_err());
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
}
