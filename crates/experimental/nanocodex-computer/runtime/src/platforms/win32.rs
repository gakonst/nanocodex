//! Independent Windows backend: real UI Automation trees/patterns on an MTA worker,
//! Win32 window identity/input and isolated HWND-scoped Windows Graphics Capture.
//! Compile-checked where the Windows target is available; no claim of OS replay here.
use crate::{
    Error, Result,
    ax::Node,
    native::{Action, App, Desktop, Image, Target},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{collections::BTreeMap, mem::size_of, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::*,
    System::Threading::*,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
#[derive(Default)]
pub struct Win32 {
    uia: Option<super::windows_uia_model::Worker>,
    capture: Option<super::windows_capture_model::Worker>,
    sky_trees: BTreeMap<usize, (u32, crate::ax::Revision)>,
    sky_images: BTreeMap<String, super::windows_capture_model::Binding>,
    next_image: u64,
    audio: Option<super::windows_audio_model::Audio>,
}
impl Win32 {
    pub fn new() -> Self {
        Self::default()
    }
    fn uia(&mut self) -> Result<&mut super::windows_uia_model::Worker> {
        if self.uia.is_none() {
            self.uia = Some(super::windows_uia::start()?);
        }
        Ok(self.uia.as_mut().expect("worker initialized"))
    }
    fn capture_window(
        &mut self,
        hwnd: HWND,
        app: &App,
    ) -> Result<super::windows_capture_model::Screenshot> {
        if self.capture.is_none() {
            self.capture = Some(super::windows_capture::start()?);
        }
        let shot = self
            .capture
            .as_mut()
            .expect("capture initialized")
            .capture(super::windows_uia_model::Root {
                hwnd: hwnd as usize,
                pid: app.pid as u32,
            })?;
        if unsafe { IsWindow(hwnd) } == 0
            || pid(hwnd) != app.pid as u32
            || super::windows_capture::geometry(shot.root)? != shot.geometry
        {
            return Err(Error::action(
                "Window identity or geometry changed after WGC capture",
            ));
        }
        Ok(shot)
    }
    fn observe_window(&mut self, hwnd: HWND, app: &App) -> Result<Node> {
        self.uia()?.observe(super::windows_uia_model::Root {
            hwnd: hwnd as usize,
            pid: app.pid as u32,
        })
    }
    fn action_window(
        &mut self,
        app: &App,
        action: Action,
        hwnd: HWND,
        screenshot: Option<super::windows_capture_model::Binding>,
    ) -> Result<()> {
        use super::windows_uia_model::{Operation, Root};
        let root = Root {
            hwnd: hwnd as usize,
            pid: app.pid as u32,
        };
        if unsafe { IsWindow(hwnd) } == 0 || pid(hwnd) != root.pid {
            return Err(Error::action(
                "Target window identity changed before UIA action",
            ));
        }
        activate(hwnd)?;
        let mut resolved_point = None;
        let action = match action {
            Action::SetValue { identity, value } => {
                self.uia()?
                    .perform(root, &identity, Operation::SetValue(value))?;
                return Ok(());
            }
            Action::SelectText { identity, range } => {
                self.uia()?
                    .perform(root, &identity, Operation::SelectText(range))?;
                return Ok(());
            }
            Action::Secondary { identity, action } => {
                self.uia()?.perform(
                    root,
                    &identity,
                    super::windows_uia_model::secondary(&action)?,
                )?;
                return Ok(());
            }
            Action::Click {
                target: Target::Element { identity },
                button,
                count,
            } => {
                let frame = rectangle(hwnd)?;
                let point = self
                    .uia()?
                    .perform(root, &identity, Operation::Point)?
                    .ok_or_else(|| Error::action("UIA target has no point"))?;
                if rectangle(hwnd)? != frame {
                    return Err(Error::action("Window moved while resolving UIA target"));
                }
                resolved_point = Some(super::windows_uia_model::BoundPoint::new(
                    root, frame, point,
                )?);
                Action::Click {
                    target: Target::Point {
                        point: [point[0] - frame[0], point[1] - frame[1]],
                    },
                    button,
                    count,
                }
            }
            Action::Scroll {
                target: Target::Element { identity },
                direction,
                pages,
            } => {
                let frame = rectangle(hwnd)?;
                let point = self
                    .uia()?
                    .perform(root, &identity, Operation::Point)?
                    .ok_or_else(|| Error::action("UIA target has no point"))?;
                if rectangle(hwnd)? != frame {
                    return Err(Error::action("Window moved while resolving UIA target"));
                }
                resolved_point = Some(super::windows_uia_model::BoundPoint::new(
                    root, frame, point,
                )?);
                Action::Scroll {
                    target: Target::Point {
                        point: [point[0] - frame[0], point[1] - frame[1]],
                    },
                    direction,
                    pages,
                }
            }
            action => action,
        };
        action_at(app, action, hwnd, resolved_point, screenshot)
    }
    fn sky_window(input: &Value) -> Result<(HWND, App, Value)> {
        let id = input["id"]
            .as_u64()
            .and_then(|id| usize::try_from(id).ok())
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::invalid("window id must be a positive integer"))?;
        let hwnd = id as HWND;
        if unsafe { IsWindow(hwnd) } == 0 || unsafe { IsWindowVisible(hwnd) } == 0 {
            return Err(Error::action("Window is no longer available"));
        }
        let process = pid(hwnd);
        let path = path_for(process);
        let canonical = format!("win32:{process}");
        if let Some(expected) = input.get("app").and_then(Value::as_str)
            && expected != canonical
            && !expected.eq_ignore_ascii_case(&path)
        {
            return Err(Error::action("Window belongs to a different application"));
        }
        let app = App {
            id: canonical.clone(),
            name: path.rsplit(['\\', '/']).next().unwrap_or(&path).into(),
            path,
            pid: process as i32,
        };
        Ok((
            hwnd,
            app,
            json!({"id":id,"app":canonical,"title":text(hwnd)}),
        ))
    }
    fn sky_element(&self, hwnd: HWND, app: &App, params: &Value) -> Result<Target> {
        if let Some(index) = params.get("element_index") {
            let index = index
                .as_u64()
                .ok_or_else(|| Error::invalid("element_index must be a nonnegative integer"))?;
            let (saved_pid, revision) = self
                .sky_trees
                .get(&(hwnd as usize))
                .ok_or_else(|| Error::action("Request window text before using element indexes"))?;
            if *saved_pid != app.pid as u32 {
                return Err(Error::action("Window identity was reused"));
            }
            let node = revision
                .by_id(index)
                .ok_or_else(|| Error::action("Element index not in latest window state"))?;
            return Ok(Target::Element {
                identity: node.identity.clone(),
            });
        }
        Ok(Target::Point {
            point: [finite(params, "x")?, finite(params, "y")?],
        })
    }
}
fn finite(params: &Value, key: &str) -> Result<f64> {
    params[key]
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| Error::invalid(format!("{key} must be finite")))
}
fn error(context: &str) -> Error {
    Error::action(format!("{context}: {}", std::io::Error::last_os_error()))
}
fn pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}
fn text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) }.clamp(0, 65535);
    let mut data = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, data.as_mut_ptr(), data.len() as i32) };
    String::from_utf16_lossy(&data[..copied.max(0) as usize])
}
fn rectangle(hwnd: HWND) -> Result<[f64; 4]> {
    super::windows_capture::window_frame(hwnd as usize)
}
unsafe extern "system" fn enumerate(hwnd: HWND, context: LPARAM) -> i32 {
    let rows = unsafe { &mut *(context as *mut Vec<usize>) };
    if rows.len() < 5000 && unsafe { IsWindowVisible(hwnd) } != 0 {
        rows.push(hwnd as usize);
    }
    1
}
fn windows() -> Result<Vec<HWND>> {
    let mut out: Vec<usize> = vec![];
    if unsafe { EnumWindows(Some(enumerate), &mut out as *mut _ as isize) } == 0 {
        return Err(error("EnumWindows"));
    }
    Ok(out.into_iter().map(|id| id as HWND).collect())
}
fn path_for(pid: u32) -> String {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return format!("pid:{pid}");
        }
        let mut path = vec![0u16; 32768];
        let mut count = path.len() as u32;
        let result = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut count);
        CloseHandle(process);
        if result == 0 {
            format!("pid:{pid}")
        } else {
            String::from_utf16_lossy(&path[..count as usize])
        }
    }
}
fn main_window(app: &App) -> Result<HWND> {
    let all = windows()?;
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.is_null() && pid(foreground) == app.pid as u32 {
        return Ok(foreground);
    }
    all.into_iter()
        .find(|window| pid(*window) == app.pid as u32)
        .ok_or_else(|| Error::action("Application has no visible window"))
}
fn activate(hwnd: HWND) -> Result<()> {
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd) == 0 && GetForegroundWindow() != hwnd {
            return Err(error("Window activation was denied"));
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while unsafe { GetForegroundWindow() } != hwnd {
        if std::time::Instant::now() >= deadline {
            return Err(Error::action(
                "Target window did not become the foreground window",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
fn send(inputs: &[INPUT]) -> Result<()> {
    if unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    } != inputs.len() as u32
    {
        return Err(error("SendInput was blocked or incomplete"));
    }
    Ok(())
}
fn keyboard(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
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
fn type_unicode(text: &str) -> Result<()> {
    for unit in text.encode_utf16() {
        send(&[
            keyboard(0, unit, KEYEVENTF_UNICODE),
            keyboard(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ])?;
    }
    Ok(())
}
fn press(chord: &str) -> Result<()> {
    let parsed = crate::keys::cdp_key(chord)?;
    let mask = parsed["modifiers"].as_u64().unwrap();
    let vk = parsed["windowsVirtualKeyCode"].as_u64().unwrap() as u16;
    let modifiers: Vec<_> = [(1, VK_MENU), (2, VK_CONTROL), (4, VK_LWIN), (8, VK_SHIFT)]
        .into_iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, vk)| vk)
        .collect();
    let mut inputs: Vec<_> = modifiers.iter().map(|vk| keyboard(*vk, 0, 0)).collect();
    let extended = if [
        VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_DELETE,
    ]
    .contains(&vk)
    {
        KEYEVENTF_EXTENDEDKEY
    } else {
        0
    };
    inputs.extend([
        keyboard(vk, 0, extended),
        keyboard(vk, 0, extended | KEYEVENTF_KEYUP),
    ]);
    inputs.extend(
        modifiers
            .iter()
            .rev()
            .map(|vk| keyboard(*vk, 0, KEYEVENTF_KEYUP)),
    );
    let result = send(&inputs);
    if result.is_err() {
        let mut release: Vec<_> = modifiers
            .iter()
            .map(|vk| keyboard(*vk, 0, KEYEVENTF_KEYUP))
            .collect();
        release.push(keyboard(vk, 0, extended | KEYEVENTF_KEYUP));
        let _ = send(&release);
    }
    result
}
fn point(
    root: HWND,
    target_ref: Target,
    resolved: Option<super::windows_uia_model::BoundPoint>,
) -> Result<[f64; 2]> {
    match target_ref {
        Target::Point { point } => match resolved {
            Some(bound) => bound.validate(
                super::windows_uia_model::Root {
                    hwnd: root as usize,
                    pid: pid(root),
                },
                rectangle(root)?,
                unsafe { GetForegroundWindow() } as usize,
            ),
            None => crate::native::window_point(rectangle(root)?, point),
        },
        Target::Element { .. } => Err(Error::action(
            "UIA element must be resolved before physical input",
        )),
    }
}
fn move_pointer(point: [f64; 2]) -> Result<()> {
    if !point
        .iter()
        .all(|n| n.is_finite() && *n >= i32::MIN as f64 && *n <= i32::MAX as f64)
    {
        return Err(Error::invalid("Invalid pointer coordinate"));
    }
    if unsafe { SetCursorPos(point[0].round() as i32, point[1].round() as i32) } == 0 {
        return Err(error("SetCursorPos"));
    }
    Ok(())
}
impl Desktop for Win32 {
    fn app_policy_target(&mut self, identifier: &str) -> Result<App> {
        let mut running = self.apps()?.into_iter().filter(|app| {
            app.id == identifier
                || app.path.eq_ignore_ascii_case(identifier)
                || app.name.eq_ignore_ascii_case(identifier)
        });
        if let Some(app) = running.next() {
            if running.next().is_some() {
                return Err(Error::invalid("Application identifier is ambiguous"));
            }
            return Ok(app);
        }
        let path = std::path::Path::new(identifier);
        if !path.is_absolute()
            || !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
        {
            return Err(Error::invalid(
                "Launching an undiscovered app requires an explicit absolute .exe path",
            ));
        }
        let canonical = std::fs::canonicalize(path)?;
        if !std::fs::metadata(&canonical)?.is_file() {
            return Err(Error::invalid("Application path is not a file"));
        }
        let path = canonical.to_string_lossy().into_owned();
        Ok(App {
            id: path.clone(),
            name: canonical
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path,
            pid: 0,
        })
    }
    fn sky_policy_target(&mut self, method: &str, params: &Value) -> Result<App> {
        if method == "launch_app" {
            return self.app_policy_target(
                params["app"]
                    .as_str()
                    .ok_or_else(|| Error::invalid("app is required"))?,
            );
        }
        let (_, app, _) = Self::sky_window(if method == "get_window" {
            params
        } else {
            &params["window"]
        })?;
        if !std::path::Path::new(&app.path).is_absolute() {
            return Err(Error::action(
                "Cannot determine window application path for approval",
            ));
        }
        Ok(app)
    }
    fn sky_target(&self) -> &'static str {
        "windows"
    }
    fn sky_execute(&mut self, method: &str, args: &Value) -> Result<Value> {
        let (_, params) = super::windows::lower(method, args)?;
        if method == "list_windows" {
            return Ok(json!(windows()?.into_iter().map(|hwnd|json!({"id":hwnd as usize,"app":format!("win32:{}",pid(hwnd)),"title":text(hwnd)})).collect::<Vec<_>>()));
        }
        if method == "list_apps" {
            let all = self.sky_execute("list_windows", &json!({}))?;
            return Ok(json!(self.apps()?.into_iter().map(|app|{
                let windows=all.as_array().unwrap().iter().filter(|window|window["app"]==app.id).cloned().collect::<Vec<_>>();
                json!({"id":app.id,"displayName":app.name,"isRunning":true,"windows":windows})
            }).collect::<Vec<_>>()));
        }
        if method == "launch_app" {
            let path = params["app"].as_str().unwrap();
            if self
                .apps()?
                .iter()
                .any(|app| app.id == path || app.path.eq_ignore_ascii_case(path))
            {
                return Ok(Value::Null);
            }
            if !path.to_lowercase().ends_with(".exe") {
                return Err(Error::invalid(
                    "Launching an undiscovered app requires an explicit .exe path",
                ));
            }
            std::process::Command::new(path)
                .spawn()
                .map_err(|e| Error::action(format!("Application launch failed: {e}")))?;
            return Ok(Value::Null);
        }
        let (hwnd, app, window) = Self::sky_window(if method == "get_window" {
            &params
        } else {
            &params["window"]
        })?;
        if method == "get_window" {
            return Ok(window);
        }
        if method == "get_window_state" {
            let result = (|| -> Result<Value> {
                let frame = rectangle(hwnd)?;
                // Finish image capture before publishing any replacement element IDs.
                let image = if params["include_screenshot"] == true {
                    Some(self.capture_window(hwnd, &app)?)
                } else {
                    None
                };
                let revision = if params["include_text"] == true {
                    Some(crate::ax::Revision::root(self.observe_window(hwnd, &app)?))
                } else {
                    None
                };
                if unsafe { IsWindow(hwnd) } == 0
                    || pid(hwnd) != app.pid as u32
                    || rectangle(hwnd)? != frame
                {
                    return Err(Error::action(
                        "Window identity or geometry changed during observation",
                    ));
                }
                if let Some(image) = &image {
                    image
                        .binding()
                        .validate(image.root, super::windows_capture::geometry(image.root)?)?;
                }
                let accessibility = revision
                    .as_ref()
                    .map(|revision| json!({"tree":revision.full_text()}))
                    .unwrap_or(Value::Null);
                let mut screenshots = Vec::new();
                if let Some(image) = image {
                    self.next_image = self
                        .next_image
                        .checked_add(1)
                        .ok_or_else(|| Error::action("Screenshot identifier overflow"))?;
                    let id = format!(
                        "skyre-wgc-{}-{}-{}",
                        hwnd as usize, self.next_image, image.generation
                    );
                    self.sky_images
                        .retain(|_, binding| binding.root.hwnd != hwnd as usize);
                    if self.sky_images.len() >= 128 {
                        self.sky_images.clear();
                    }
                    self.sky_images.insert(id.clone(), image.binding());
                    screenshots.push(json!({"id":id,"zIndex":0,"url":format!("data:image/png;base64,{}",STANDARD.encode(image.png)),"originX":frame[0],"originY":frame[1],"width":frame[2],"height":frame[3]}));
                }
                if let Some(revision) = revision {
                    self.sky_trees
                        .insert(hwnd as usize, (app.pid as u32, revision));
                }
                Ok(json!({"window":window,"screenshots":screenshots,"accessibility":accessibility}))
            })();
            if result.is_err() {
                // A failed combined capture must not let old IDs resolve against
                // a newly prepared UIA snapshot that the caller never received.
                self.sky_trees.clear();
                self.sky_images
                    .retain(|_, binding| binding.root.hwnd != hwnd as usize);
                if let Some(worker) = &mut self.uia {
                    let _ = worker.clear();
                }
            }
            return result;
        }
        let screenshot = if let Some(id) = params.get("screenshotId").and_then(Value::as_str) {
            let saved = *self
                .sky_images
                .get(id)
                .ok_or_else(|| Error::action("Screenshot is not cached for the target window"))?;
            let root = super::windows_uia_model::Root {
                hwnd: hwnd as usize,
                pid: app.pid as u32,
            };
            saved.validate(root, super::windows_capture::geometry(root)?)?;
            Some(saved)
        } else {
            None
        };
        activate(hwnd)?;
        if method == "activate_window" {
            return Ok(Value::Null);
        }
        let action = match method {
            "click" => Action::Click {
                target: self.sky_element(hwnd, &app, &params)?,
                button: match params["mouse_button"].as_str().unwrap() {
                    "left" => 0,
                    "right" => 1,
                    _ => 2,
                },
                count: params["click_count"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| Error::invalid("click_count must be an integer"))?,
            },
            "drag" => Action::Drag {
                from: [finite(&params, "from_x")?, finite(&params, "from_y")?],
                to: [finite(&params, "to_x")?, finite(&params, "to_y")?],
            },
            "press_key" => Action::PressKey {
                key: params["key"].as_str().unwrap().into(),
            },
            "type_text" => Action::TypeText {
                text: params["text"].as_str().unwrap().into(),
            },
            "set_value" | "perform_secondary_action" => {
                let Target::Element { identity } = self.sky_element(hwnd, &app, &params)? else {
                    return Err(Error::invalid("Element index required"));
                };
                if method == "set_value" {
                    Action::SetValue {
                        identity,
                        value: params["value"].as_str().unwrap().into(),
                    }
                } else {
                    Action::Secondary {
                        identity,
                        action: params["action"].as_str().unwrap().into(),
                    }
                }
            }
            "scroll" => {
                if let Some(binding) = screenshot {
                    binding.validate(
                        binding.root,
                        super::windows_capture::geometry(binding.root)?,
                    )?;
                }
                let frame = rectangle(hwnd)?;
                move_pointer(crate::native::window_point(
                    frame,
                    [finite(&params, "x")?, finite(&params, "y")?],
                )?)?;
                for (key, flag, sign) in [
                    ("scrollX", MOUSEEVENTF_HWHEEL, 1.),
                    ("scrollY", MOUSEEVENTF_WHEEL, -1.),
                ] {
                    let amount = (finite(&params, key)? * sign)
                        .round()
                        .clamp(i32::MIN as f64, i32::MAX as f64)
                        as i32;
                    if amount != 0 {
                        send(&[mouse(flag, amount as u32)])?;
                    }
                }
                return Ok(Value::Null);
            }
            _ => {
                return Err(Error::unsupported(format!(
                    "Unsupported native Windows Sky method: {method}"
                )));
            }
        };
        self.action_window(&app, action, hwnd, screenshot)?;
        Ok(Value::Null)
    }
    fn audio(&mut self, method: &str, owner: &str, params: &Value) -> Result<Value> {
        self.audio
            .get_or_insert_with(super::windows_audio::recorder)
            .execute(method, owner, params)
    }
    fn cancel_audio(&mut self, owner: &str) -> Result<()> {
        match &mut self.audio {
            Some(audio) => audio.end_session(owner),
            None => Ok(()),
        }
    }
    fn end_session(&mut self, owner: &str) -> Result<()> {
        self.sky_trees.clear();
        self.sky_images.clear();
        let audio_cleanup = match &mut self.audio {
            Some(audio) => audio.end_session(owner),
            None => Ok(()),
        };
        let uia_cleanup = match &mut self.uia {
            Some(worker) => worker.clear(),
            None => Ok(()),
        };
        audio_cleanup?;
        uia_cleanup?;
        Ok(())
    }
    fn apps(&mut self) -> Result<Vec<App>> {
        let mut apps = BTreeMap::new();
        for hwnd in windows()? {
            let process = pid(hwnd);
            let path = path_for(process);
            apps.entry(process).or_insert_with(|| App {
                id: format!("win32:{process}"),
                name: path.rsplit(['\\', '/']).next().unwrap_or(&path).into(),
                path,
                pid: process as i32,
            });
        }
        Ok(apps.into_values().collect())
    }
    fn snapshot(&mut self, app: &App) -> Result<Node> {
        self.observe_window(main_window(app)?, app)
    }
    fn action(&mut self, app: &App, action: Action) -> Result<()> {
        self.action_window(app, action, main_window(app)?, None)
    }
    fn screenshot(&mut self, app: &App) -> Result<Image> {
        let shot = self.capture_window(main_window(app)?, app)?;
        Ok(Image {
            mime_type: "image/png".into(),
            data: STANDARD.encode(shot.png),
        })
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec![
            "list_apps",
            "bind_app",
            "get_app_state",
            "get_screenshot",
            "click",
            "drag",
            "press_key",
            "type_text",
            "set_value",
            "select_text",
            "scroll",
            "perform_secondary_action",
        ]
    }
}
fn action_at(
    app: &App,
    action: Action,
    root: HWND,
    resolved: Option<super::windows_uia_model::BoundPoint>,
    screenshot: Option<super::windows_capture_model::Binding>,
) -> Result<()> {
    if unsafe { IsWindow(root) } == 0 || pid(root) != app.pid as u32 {
        return Err(Error::action("Target window identity changed before input"));
    }
    activate(root)?;
    if let Some(binding) = screenshot {
        let target = super::windows_uia_model::Root {
            hwnd: root as usize,
            pid: pid(root),
        };
        binding.validate(target, super::windows_capture::geometry(target)?)?;
    }
    match action {
        Action::TypeText { text } => type_unicode(&text),
        Action::PressKey { key } => press(&key),
        Action::Paste { .. } => Err(Error::unsupported(
            "Win32 transactional clipboard is not implemented; use typeText",
        )),
        Action::Click {
            target,
            button,
            count,
        } => {
            if !(1..=3).contains(&count) {
                return Err(Error::invalid("Invalid click count"));
            }
            let (down, up) = match button {
                0 => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                1 => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                2 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                _ => return Err(Error::invalid("Invalid mouse button")),
            };
            move_pointer(point(root, target, resolved)?)?;
            for _ in 0..count {
                if let Err(error) = send(&[mouse(down, 0), mouse(up, 0)]) {
                    let _ = send(&[mouse(up, 0)]);
                    return Err(error);
                }
            }
            Ok(())
        }
        Action::Drag { from, to } => {
            let frame = rectangle(root)?;
            move_pointer(crate::native::window_point(frame, from)?)?;
            send(&[mouse(MOUSEEVENTF_LEFTDOWN, 0)])?;
            let moved: Result<()> = (|| {
                for step in 1..=20 {
                    let t = step as f64 / 20.;
                    move_pointer(crate::native::window_point(
                        frame,
                        [
                            from[0] + (to[0] - from[0]) * t,
                            from[1] + (to[1] - from[1]) * t,
                        ],
                    )?)?;
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            })();
            let released = send(&[mouse(MOUSEEVENTF_LEFTUP, 0)]);
            moved?;
            released
        }
        Action::Scroll {
            target,
            direction,
            pages,
        } => {
            move_pointer(point(root, target, resolved)?)?;
            let (flags, sign) = match direction.as_str() {
                "up" => (MOUSEEVENTF_WHEEL, 1),
                "down" => (MOUSEEVENTF_WHEEL, -1),
                "left" => (MOUSEEVENTF_HWHEEL, -1),
                "right" => (MOUSEEVENTF_HWHEEL, 1),
                _ => return Err(Error::invalid("Invalid scroll direction")),
            };
            if !pages.is_finite() || pages <= 0. || pages > 100. {
                return Err(Error::invalid("Invalid scroll pages"));
            }
            send(&[mouse(
                flags,
                ((pages * 3. * WHEEL_DELTA as f64).round() as i32 * sign) as u32,
            )])
        }
        Action::SetValue { .. } | Action::SelectText { .. } | Action::Secondary { .. } => Err(
            Error::action("UIA action was not routed through its owning worker"),
        ),
    }
}
