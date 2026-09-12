//! In-process X11/XTEST desktop provider. No shell commands, helper executables,
//! clipboard mutation, or global keyboard-map changes are used for ordinary input.
use super::{required, unsigned};
use crate::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    thread,
    time::Duration,
};
use x11rb::{
    connection::Connection,
    image::{Image, PixelLayout},
    protocol::{
        xfixes::ConnectionExt as _, xkb::ConnectionExt as _, xproto::*, xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};
#[path = "linux_xkb.rs"]
mod keyboard;

fn failure(error: impl std::fmt::Display) -> Error {
    Error::action(format!("X11: {error}"))
}

pub struct NativeX11 {
    connection: RustConnection,
    root: Window,
    owned_keys: Vec<Keycode>,
    key_chords: BTreeMap<String, Vec<Keycode>>,
    owned_buttons: BTreeSet<u8>,
    xkb: libloading::Library,
    cursor: bool,
    clipboard: Option<super::linux_clipboard::Clipboard>,
}

impl NativeX11 {
    pub fn connect() -> Result<Self> {
        let (connection, screen) = x11rb::connect(None).map_err(failure)?;
        connection
            .xtest_get_version(2, 2)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        let root = connection.setup().roots[screen].root;
        if !connection
            .xkb_use_extension(1, 0)
            .map_err(failure)?
            .reply()
            .map_err(failure)?
            .supported
        {
            return Err(Error::unsupported("XKB is unavailable"));
        }
        let cursor = connection
            .xfixes_query_version(4, 0)
            .map_err(failure)?
            .reply()
            .is_ok();
        // xkbcommon is the platform keysym-name registry, not a desktop helper.
        // A fixed system soname prevents task input from selecting library paths.
        let xkb = unsafe { libloading::Library::new("libxkbcommon.so.0") }.map_err(|error| {
            Error::unsupported(format!("Linux keysym registry unavailable: {error}"))
        })?;
        Ok(Self {
            connection,
            root,
            owned_keys: Vec::new(),
            key_chords: BTreeMap::new(),
            owned_buttons: BTreeSet::new(),
            xkb,
            cursor,
            clipboard: None,
        })
    }

    fn atom(&self, name: &str) -> Result<Atom> {
        Ok(self
            .connection
            .intern_atom(false, name.as_bytes())
            .map_err(failure)?
            .reply()
            .map_err(failure)?
            .atom)
    }

    fn property(&self, window: Window, name: &str, kind: Atom) -> Result<GetPropertyReply> {
        let result = self
            .connection
            .get_property(false, window, self.atom(name)?, kind, 0, 262144)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        if result.bytes_after != 0 {
            return Err(Error::action("X11 property exceeds 1 MiB"));
        }
        Ok(result)
    }

    fn window(params: &Value) -> Result<Window> {
        params["window"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id != 0)
            .ok_or_else(|| Error::invalid("window must be a positive X11 identifier"))
    }

    pub(super) fn position(params: &Value) -> Result<(i16, i16)> {
        fn coordinate(params: &Value, key: &str) -> Result<i16> {
            params[key]
                .as_f64()
                .filter(|v| {
                    v.is_finite() && v.round() >= i16::MIN as f64 && v.round() <= i16::MAX as f64
                })
                .map(|v| v.round() as i16)
                .ok_or_else(|| Error::invalid(format!("{key} exceeds X11 coordinate range")))
        }
        Ok((coordinate(params, "x")?, coordinate(params, "y")?))
    }

    fn sync(&self) -> Result<()> {
        self.connection
            .get_input_focus()
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        Ok(())
    }

    fn event(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<()> {
        self.connection
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .map_err(failure)?
            .check()
            .map_err(failure)
    }

    fn motion(&self, params: &Value) -> Result<()> {
        let (x, y) = Self::position(params)?;
        self.event(MOTION_NOTIFY_EVENT, 0, x, y)
    }

    fn button(params: &Value) -> Result<u8> {
        match params
            .get("mouse_button")
            .and_then(Value::as_str)
            .unwrap_or("left")
        {
            "left" => Ok(1),
            "middle" => Ok(2),
            "right" => Ok(3),
            _ => Err(Error::invalid("Invalid mouse button")),
        }
    }

    fn button_down(&mut self, button: u8) -> Result<()> {
        if self.owned_buttons.contains(&button) {
            return Err(Error::action("Mouse button is already held"));
        }
        let pointer = self
            .connection
            .query_pointer(self.root)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        if button <= 5 && u16::from(pointer.mask) & (1 << (7 + button)) != 0 {
            return Err(Error::action(
                "Mouse button is held by another input source",
            ));
        }
        self.event(BUTTON_PRESS_EVENT, button, 0, 0)?;
        self.owned_buttons.insert(button);
        Ok(())
    }

    fn button_up(&mut self, button: u8) -> Result<()> {
        if self.owned_buttons.contains(&button) {
            self.event(BUTTON_RELEASE_EVENT, button, 0, 0)?;
            self.owned_buttons.remove(&button);
        }
        Ok(())
    }

    fn keysym(&self, input: &str) -> Result<u32> {
        let name = match input {
            "Ctrl" | "CTRL" | "ctrl" | "Control" | "control" => "Control_L",
            "Alt" | "alt" | "Option" => "Alt_L",
            "Shift" | "shift" => "Shift_L",
            "Super" | "super" | "Meta" | "Win" | "Command" => "Super_L",
            "Enter" | "enter" => "Return",
            "Esc" | "esc" => "Escape",
            "Backspace" => "BackSpace",
            "PageUp" => "Prior",
            "PageDown" => "Next",
            _ => input,
        };
        let name = CString::new(name).map_err(|_| Error::invalid("Invalid keysym name"))?;
        let symbol = unsafe {
            let lookup: libloading::Symbol<
                '_,
                unsafe extern "C" fn(*const std::ffi::c_char, u32) -> u32,
            > = self.xkb.get(b"xkb_keysym_from_name\0").map_err(failure)?;
            lookup(name.as_ptr(), 0)
        };
        if symbol == 0 {
            return Err(Error::invalid(format!("Unknown X11 keysym: {input}")));
        }
        Ok(symbol)
    }

    fn chord(&self, input: &str) -> Result<Vec<Keycode>> {
        let names: Vec<_> = input.split('+').map(str::trim).collect();
        if names.is_empty() || names.len() > 16 || names.iter().any(|s| s.is_empty()) {
            return Err(Error::invalid("Invalid X11 key chord"));
        }
        let symbols = names
            .into_iter()
            .map(|name| self.keysym(name))
            .collect::<Result<Vec<_>>>()?;
        keyboard::chord(&self.connection, &symbols)
    }

    pub fn key_down(&mut self, chord: &str) -> Result<()> {
        let codes = self.chord(chord)?;
        let state = self
            .connection
            .query_keymap()
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        for code in &codes {
            if self.owned_keys.contains(code)
                || state.keys[*code as usize / 8] & (1 << (*code % 8)) != 0
            {
                return Err(Error::action(
                    "Key is already held by another operation or input source",
                ));
            }
        }
        let start = self.owned_keys.len();
        for code in &codes {
            let code = *code;
            if let Err(error) = self.event(KEY_PRESS_EVENT, code, 0, 0) {
                let _ = self.release_keys_from(start);
                return Err(error);
            }
            self.owned_keys.push(code);
        }
        self.key_chords.insert(chord.to_owned(), codes);
        Ok(())
    }

    fn release_keys_from(&mut self, start: usize) -> Result<()> {
        let mut first = None;
        for index in (start..self.owned_keys.len()).rev() {
            match self.event(KEY_RELEASE_EVENT, self.owned_keys[index], 0, 0) {
                Ok(()) => {
                    self.owned_keys.remove(index);
                }
                Err(error) => {
                    first.get_or_insert(error);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    pub fn key_up(&mut self, chord: &str) -> Result<()> {
        let codes = self
            .key_chords
            .get(chord)
            .cloned()
            .ok_or_else(|| Error::action("Key chord is not held by this operation"))?;
        let mut first = None;
        for code in codes.into_iter().rev() {
            if let Some(index) = self.owned_keys.iter().position(|c| *c == code) {
                match self.event(KEY_RELEASE_EVENT, code, 0, 0) {
                    Ok(()) => {
                        self.owned_keys.remove(index);
                    }
                    Err(error) => {
                        first.get_or_insert(error);
                    }
                }
            }
        }
        if first.is_none() {
            self.key_chords.remove(chord);
        }
        first.map_or(Ok(()), Err)
    }

    pub fn release_all(&mut self) -> Result<()> {
        let mut first = self.release_keys_from(0).err();
        if self.owned_keys.is_empty() {
            self.key_chords.clear();
        }
        for button in self.owned_buttons.clone() {
            if let Err(error) = self.button_up(button) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }

    fn windows(&self) -> Result<Value> {
        let clients = self.property(self.root, "_NET_CLIENT_LIST", AtomEnum::WINDOW.into())?;
        let mut candidates: Vec<_> = clients.value32().map(|v| v.collect()).unwrap_or_default();
        if candidates.is_empty() {
            candidates = self
                .connection
                .query_tree(self.root)
                .map_err(failure)?
                .reply()
                .map_err(failure)?
                .children;
        }
        if candidates.len() > 20_000 {
            return Err(Error::action("X11 window limit exceeded"));
        }
        let mut visible = Vec::new();
        for id in candidates {
            // A window may disappear between enumeration and this query.
            if let Ok(reply) = self
                .connection
                .get_window_attributes(id)
                .map_err(failure)?
                .reply()
                && reply.map_state == MapState::VIEWABLE
                && reply.class == WindowClass::INPUT_OUTPUT
            {
                visible.push(id);
            }
        }
        Ok(json!(visible))
    }

    fn window_info(&self, window: Window) -> Result<Value> {
        let geometry = self
            .connection
            .get_geometry(window)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        let position = self
            .connection
            .translate_coordinates(window, self.root, 0, 0)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        let mut title = self
            .property(window, "_NET_WM_NAME", self.atom("UTF8_STRING")?)?
            .value;
        if title.is_empty() {
            title = self
                .property(window, "WM_NAME", AtomEnum::ANY.into())?
                .value;
        }
        let pid = self
            .property(window, "_NET_WM_PID", AtomEnum::CARDINAL.into())?
            .value32()
            .and_then(|mut v| v.next())
            .unwrap_or(0);
        Ok(
            json!({"id":window.to_string(),"title":String::from_utf8_lossy(&title),"pid":pid.to_string(),
            "geometry":{"X":position.dst_x.to_string(),"Y":position.dst_y.to_string(),"WIDTH":geometry.width.to_string(),"HEIGHT":geometry.height.to_string()}}),
        )
    }

    fn activate(&self, window: Window) -> Result<()> {
        let wm = self.property(
            self.root,
            "_NET_SUPPORTING_WM_CHECK",
            AtomEnum::WINDOW.into(),
        )?;
        if wm.value32().and_then(|mut v| v.next()).is_some() {
            let event = ClientMessageEvent::new(
                32,
                window,
                self.atom("_NET_ACTIVE_WINDOW")?,
                [2, 0, 0, 0, 0],
            );
            self.connection
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    event,
                )
                .map_err(failure)?
                .check()
                .map_err(failure)?;
        } else {
            self.connection
                .configure_window(
                    window,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )
                .map_err(failure)?
                .check()
                .map_err(failure)?;
            self.connection
                .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
                .map_err(failure)?
                .check()
                .map_err(failure)?;
        }
        // A WM can decline focus; don't send subsequent input to the old window.
        for _ in 0..100 {
            let mut focus = self
                .connection
                .get_input_focus()
                .map_err(failure)?
                .reply()
                .map_err(failure)?
                .focus;
            for _ in 0..64 {
                if focus == window {
                    return Ok(());
                }
                if focus == self.root || focus <= 1 {
                    break;
                }
                focus = self
                    .connection
                    .query_tree(focus)
                    .map_err(failure)?
                    .reply()
                    .map_err(failure)?
                    .parent;
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(Error::action(
            "Window manager did not focus the requested X11 window",
        ))
    }

    fn screenshot(&self, window: Window, jpeg: bool) -> Result<Value> {
        let geometry = self
            .connection
            .get_geometry(window)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        let (width, height) = (geometry.width, geometry.height);
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 32 * 1024 * 1024 {
            return Err(Error::action("X11 screenshot exceeds pixel budget"));
        }
        let (image, visual) =
            Image::get(&self.connection, window, 0, 0, width, height).map_err(failure)?;
        let visual = self
            .connection
            .setup()
            .roots
            .iter()
            .flat_map(|s| &s.allowed_depths)
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == visual)
            .ok_or_else(|| Error::action("Unknown X11 screenshot visual"))?;
        if visual.class != VisualClass::TRUE_COLOR {
            return Err(Error::unsupported(
                "X11 indexed-color screenshots are unavailable",
            ));
        }
        let layout = PixelLayout::from_visual_type(*visual).map_err(failure)?;
        let mut pixels = image::RgbImage::new(width.into(), height.into());
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            let (r, g, b) = layout.decode(image.get_pixel(x as u16, y as u16));
            *pixel = image::Rgb([(r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8]);
        }
        if self.cursor {
            let cursor = self
                .connection
                .xfixes_get_cursor_image()
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            let origin = self
                .connection
                .translate_coordinates(window, self.root, 0, 0)
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            if cursor.width == 0
                || cursor.height == 0
                || cursor.width > 1024
                || cursor.height > 1024
                || cursor.cursor_image.len()
                    != usize::from(cursor.width) * usize::from(cursor.height)
            {
                return Err(Error::action("Invalid XFixes cursor image"));
            }
            // Installed LinuxOptions defaults the pointer's maximum dimension to
            // 12 pixels. Resize premultiplied colors and alpha together.
            let original =
                image::RgbaImage::from_fn(cursor.width.into(), cursor.height.into(), |x, y| {
                    let argb = cursor.cursor_image[y as usize * cursor.width as usize + x as usize];
                    image::Rgba([
                        (argb >> 16) as u8,
                        (argb >> 8) as u8,
                        argb as u8,
                        (argb >> 24) as u8,
                    ])
                });
            let ratio = 12.0 / f64::from(cursor.width.max(cursor.height));
            let cursor_width = (f64::from(cursor.width) * ratio).round().max(1.0) as u32;
            let cursor_height = (f64::from(cursor.height) * ratio).round().max(1.0) as u32;
            let resized = image::imageops::resize(
                &original,
                cursor_width,
                cursor_height,
                image::imageops::FilterType::Triangle,
            );
            let hot_x = (f64::from(cursor.xhot) * cursor_width as f64 / f64::from(cursor.width))
                .round() as i32;
            let hot_y = (f64::from(cursor.yhot) * cursor_height as f64 / f64::from(cursor.height))
                .round() as i32;
            for (cx, cy, source) in resized.enumerate_pixels() {
                let x = cx as i32 + i32::from(cursor.x) - hot_x - i32::from(origin.dst_x);
                let y = cy as i32 + i32::from(cursor.y) - hot_y - i32::from(origin.dst_y);
                if x >= 0 && y >= 0 && x < i32::from(width) && y < i32::from(height) {
                    let alpha = u32::from(source.0[3]);
                    let dest = pixels.get_pixel_mut(x as u32, y as u32);
                    for (channel, color) in dest.0.iter_mut().zip(source.0) {
                        *channel = (u32::from(color)
                            + (u32::from(*channel) * (255 - alpha) + 127) / 255)
                            .min(255) as u8;
                    }
                }
            }
        }

        if jpeg {
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
                .encode_image(&pixels)
                .map_err(failure)?;
            return Ok(json!({"mime_type":"image/jpeg","data":STANDARD.encode(bytes)}));
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        pixels
            .write_to(&mut bytes, image::ImageFormat::Png)
            .map_err(failure)?;
        Ok(json!({"mime_type":"image/png","data":STANDARD.encode(bytes.into_inner())}))
    }

    pub fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "list_windows" => return self.windows(),
            "get_window" => return self.window_info(Self::window(params)?),
            "activate_window" => self.activate(Self::window(params)?)?,
            "get_screenshot" => {
                return self.screenshot(
                    if params.get("window").is_some() {
                        Self::window(params)?
                    } else {
                        self.root
                    },
                    false,
                );
            }
            "get_desktop_screenshot" => return self.screenshot(self.root, true),
            "move" => self.motion(params)?,
            "mouse_down" | "mouse_up" => {
                let button = Self::button(params)?;
                if params.get("x").is_some() || params.get("y").is_some() {
                    self.motion(params)?;
                }
                if method == "mouse_down" {
                    self.button_down(button)?;
                } else {
                    self.button_up(button)?;
                }
            }
            "click" => {
                let button = Self::button(params)?;
                let count = unsigned(params, "click_count", 1)?;
                if !(1..=10).contains(&count) {
                    return Err(Error::invalid("click_count must be 1..10"));
                }
                self.motion(params)?;
                for index in 0..count {
                    self.button_down(button)?;
                    self.button_up(button)?;
                    if index + 1 < count {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            "drag" => {
                let from = json!({"x":params["from_x"],"y":params["from_y"]});
                let to = json!({"x":params["to_x"],"y":params["to_y"]});
                Self::position(&from)?;
                Self::position(&to)?;
                self.motion(&from)?;
                self.button_down(1)?;
                let movement = self.motion(&to);
                let release = self.button_up(1);
                movement?;
                release?;
            }
            "scroll" => {
                let button = match required(params, "direction")? {
                    "up" => 4,
                    "down" => 5,
                    "left" => 6,
                    "right" => 7,
                    _ => return Err(Error::invalid("Invalid scroll direction")),
                };
                let steps = unsigned(params, "steps", 3)?;
                if !(1..=1000).contains(&steps) {
                    return Err(Error::invalid("steps must be 1..1000"));
                }
                if params.get("x").is_some() || params.get("y").is_some() {
                    self.motion(params)?;
                }
                for index in 0..steps {
                    self.button_down(button)?;
                    self.button_up(button)?;
                    if index + 1 < steps {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            "press_key" => {
                let key = required(params, "key")?;
                self.key_down(key)?;
                self.key_up(key)?;
            }
            "type_text" => {
                let text = params["text"]
                    .as_str()
                    .ok_or_else(|| Error::invalid("text must be a string"))?;
                if text.len() > 1024 * 1024 {
                    return Err(Error::invalid("text exceeds 1 MiB"));
                }
                if text.is_empty() {
                    return Ok(Value::Null);
                }
                self.chord("Control_L+v")?;
                if self.clipboard.is_none() {
                    self.clipboard = Some(super::linux_clipboard::Clipboard::new()?);
                }
                let recipient = self
                    .connection
                    .get_input_focus()
                    .map_err(failure)?
                    .reply()
                    .map_err(failure)?
                    .focus;
                self.clipboard.as_ref().unwrap().begin(text, recipient)?;
                let input = (|| {
                    if self
                        .connection
                        .get_input_focus()
                        .map_err(failure)?
                        .reply()
                        .map_err(failure)?
                        .focus
                        != recipient
                    {
                        return Err(Error::action("X11 focus changed while preparing paste"));
                    }
                    self.key_down("Control_L+v")?;
                    self.key_up("Control_L+v")
                })();
                let restored = self.clipboard.as_ref().unwrap().finish(input.is_ok());
                input?;
                restored?;
            }

            _ => {
                return Err(Error::unsupported(format!(
                    "Unsupported native X11 method: {method}"
                )));
            }
        }
        self.sync()?;
        Ok(Value::Null)
    }
}

impl Drop for NativeX11 {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}
