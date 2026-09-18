//! Addressed background events. No HID stream, cursor warp, app activation, or
//! front-process manipulation is permitted here. Private window-local metadata
//! is required because PID addressing alone does not preserve AppKit hit testing.
use super::*;
#[path = "macos_cursor.rs"]
mod cursor;

pub(super) fn clear_cursor_targets(targets: Vec<(i32, u32)>) {
    cursor::clear_targets(targets);
}
use foreign_types::ForeignType;
use std::sync::OnceLock;

type WindowLocation = unsafe extern "C" fn(*mut c_void, CGPoint);
fn window_location() -> Result<WindowLocation> {
    static SYMBOL: OnceLock<Option<WindowLocation>> = OnceLock::new();
    SYMBOL
        .get_or_init(|| unsafe {
            let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"CGEventSetWindowLocation".as_ptr());
            if symbol.is_null() {
                None
            } else {
                Some(std::mem::transmute::<*mut c_void, WindowLocation>(symbol))
            }
        })
        .ok_or_else(|| {
            Error::action(
                "Background pointer input unsupported: CGEventSetWindowLocation is unavailable",
            )
        })
}

pub(super) fn window_id(window: &Ax) -> Option<u32> {
    if let Some(id) = window.text("AXWindowNumber").and_then(|n| n.parse().ok()) {
        return Some(id);
    }
    type GetWindow = unsafe extern "C" fn(AXUIElementRef, *mut u32) -> i32;
    static SYMBOL: OnceLock<Option<GetWindow>> = OnceLock::new();
    let get = SYMBOL
        .get_or_init(|| unsafe {
            let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"_AXUIElementGetWindow".as_ptr());
            if symbol.is_null() {
                None
            } else {
                Some(std::mem::transmute::<*mut c_void, GetWindow>(symbol))
            }
        })
        .as_ref()?;
    let mut id = 0;
    (unsafe { get(window.ptr(), &mut id) } == 0 && id != 0).then_some(id)
}

/// Notify only the addressed application's window. Never defocus the human's
/// application or change the WindowServer front process (even temporarily).
pub(super) fn focus_window(pid: i32, id: Option<u32>) -> Result<()> {
    type PsnForPid = unsafe extern "C" fn(i32, *mut [u32; 2]) -> i32;
    type PostRecord = unsafe extern "C" fn(*const [u32; 2], *const u8) -> i32;
    static SYMBOLS: OnceLock<Option<(PsnForPid, PostRecord)>> = OnceLock::new();
    let (get, post) = SYMBOLS
        .get_or_init(|| unsafe {
            let get = libc::dlsym(libc::RTLD_DEFAULT, c"GetProcessForPID".as_ptr());
            let post = libc::dlsym(libc::RTLD_DEFAULT, c"SLPSPostEventRecordTo".as_ptr());
            if get.is_null() || post.is_null() {
                None
            } else {
                Some((
                    std::mem::transmute::<*mut c_void, PsnForPid>(get),
                    std::mem::transmute::<*mut c_void, PostRecord>(post),
                ))
            }
        })
        .ok_or_else(|| {
            Error::action(
                "Background window focus unsupported: target notification API unavailable",
            )
        })?;
    let id = id
        .filter(|id| *id != 0)
        .ok_or_else(|| Error::action("Background window focus requires exact window identity"))?;
    let mut psn = [0u32; 2];
    if pid <= 0 || unsafe { get(pid, &mut psn) } != 0 {
        return Err(Error::action("Background process identity unavailable"));
    }
    let mut record = [0u8; 248];
    record[4] = 248;
    record[8] = 13;
    record[0x3c..0x40].copy_from_slice(&id.to_le_bytes());
    record[0x8a] = 1;
    if unsafe { post(&psn, record.as_ptr()) } != 0 {
        return Err(Error::action(
            "Target rejected background window focus notification",
        ));
    }
    // AppKit-active and native-key are separate states. Without the exact-window
    // key records, the first mouse down merely makes an inactive window key and
    // is discarded by controls that do not accept first mouse. Address only this
    // app; never change WindowServer's front process or raise its windows.
    for kind in [1, 2] {
        let mut key_record = [0u8; 248];
        key_record[4] = 248;
        key_record[8] = kind;
        key_record[0x3a] = 0x10;
        key_record[0x3c..0x40].copy_from_slice(&id.to_le_bytes());
        key_record[0x20..0x30].fill(0xff);
        if unsafe { post(&psn, key_record.as_ptr()) } != 0 {
            return Err(Error::action(
                "Target rejected background key-window notification",
            ));
        }
    }
    pump(Duration::from_millis(10));
    Ok(())
}

fn local_point(
    pid: i32,
    id: Option<u32>,
    frame: Option<[f64; 4]>,
    point: [f64; 2],
) -> Result<(u32, CGPoint)> {
    let id = id.filter(|id| *id != 0).ok_or_else(|| {
        Error::action("Background pointer input unsupported: exact window ID unavailable")
    })?;
    let [x, y, w, h] =
        frame.ok_or_else(|| Error::action("Background pointer input requires window geometry"))?;
    if pid <= 0
        || ![x, y, w, h, point[0], point[1]]
            .into_iter()
            .all(f64::is_finite)
        || w <= 0.
        || h <= 0.
        || point[0] < x
        || point[1] < y
        || point[0] >= x + w
        || point[1] >= y + h
    {
        return Err(Error::action(
            "Background pointer position must be inside the exact target window",
        ));
    }
    Ok((id, CGPoint::new(point[0] - x, point[1] - y)))
}

pub(super) fn post_pointer(
    pid: i32,
    id: Option<u32>,
    frame: Option<[f64; 4]>,
    event: &CGEvent,
    point: [f64; 2],
) -> Result<()> {
    let (id, local) = local_point(pid, id, frame, point)?;
    let set_location = window_location()?;
    event.set_integer_value_field(EventField::EVENT_TARGET_UNIX_PROCESS_ID, i64::from(pid));
    // Window number used by the CGEvent -> NSEvent bridge, plus public routing fields.
    for field in [
        51,
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER,
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER_THAT_CAN_HANDLE_THIS_EVENT,
    ] {
        event.set_integer_value_field(field, i64::from(id));
    }
    unsafe {
        set_location(event.as_ptr().cast(), local);
    }
    event.post_to_pid(pid);
    show_cursor(
        pid,
        Some(id),
        frame,
        point,
        matches!(
            event.get_type(),
            CGEventType::LeftMouseDown | CGEventType::RightMouseDown | CGEventType::OtherMouseDown
        ),
    );
    Ok(())
}

/// AXPress also uses this visual path. Invalid/missing geometry suppresses
/// feedback without turning a successful input action into an overlay failure.
pub(super) fn show_cursor(
    pid: i32,
    id: Option<u32>,
    frame: Option<[f64; 4]>,
    point: [f64; 2],
    click: bool,
) {
    if let Ok((window, _)) = local_point(pid, id, frame, point) {
        if let Some(frame) = frame {
            cursor::post(cursor::Update {
                pid,
                window,
                frame,
                point,
                click,
            });
        }
    }
}

fn drag_flags(modifiers: &[String]) -> Result<CGEventFlags> {
    let mut flags = CGEventFlags::empty();
    for modifier in modifiers {
        let flag = match modifier.as_str() {
            "shift" => CGEventFlags::CGEventFlagShift,
            "ctrl" => CGEventFlags::CGEventFlagControl,
            "alt" => CGEventFlags::CGEventFlagAlternate,
            "super" => CGEventFlags::CGEventFlagCommand,
            _ => return Err(Error::invalid("Invalid drag modifier")),
        };
        if flags.contains(flag) {
            return Err(Error::invalid("Duplicate drag modifier"));
        }
        flags |= flag;
    }
    Ok(flags)
}

pub(super) fn drag(
    pid: i32,
    id: Option<u32>,
    frame: Option<[f64; 4]>,
    from: [f64; 2],
    to: [f64; 2],
    button: u8,
    modifiers: &[String],
) -> Result<()> {
    local_point(pid, id, frame, from)?;
    local_point(pid, id, frame, to)?;
    window_location()?;
    let flags = drag_flags(modifiers)?;
    let (button, down, movement, up) = match button {
        0 => (
            CGMouseButton::Left,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseDragged,
            CGEventType::LeftMouseUp,
        ),
        1 => (
            CGMouseButton::Right,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseDragged,
            CGEventType::RightMouseUp,
        ),
        2 => (
            CGMouseButton::Center,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseDragged,
            CGEventType::OtherMouseUp,
        ),
        _ => return Err(Error::invalid("Invalid drag button")),
    };
    let source = source()?;
    let mut events = Vec::with_capacity(22);
    for step in 0..=21 {
        let t = (step.min(20) as f64) / 20.;
        let point = [
            from[0] + t * (to[0] - from[0]),
            from[1] + t * (to[1] - from[1]),
        ];
        let kind = if step == 0 {
            down
        } else if step == 21 {
            up
        } else {
            movement
        };
        let event = CGEvent::new_mouse_event(
            source.clone(),
            kind,
            CGPoint::new(point[0], point[1]),
            button,
        )
        .map_err(|_| Error::action("Cannot allocate background drag event"))?;
        event.set_flags(flags);
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 1);
        events.push((event, point));
    }
    // Allocate and validate before down. All events, including up, keep the same
    // PID/window address. Modifier bits are event-local: no hardware key is held.
    let (release, _) = events.pop().expect("prepared drag release");
    let mut last = from;
    let mut pressed = false;
    for (event, point) in events {
        if let Err(error) = MacDesktop::require_background_pid(pid) {
            if pressed {
                release.set_location(CGPoint::new(last[0], last[1]));
                let _ = post_pointer(pid, id, frame, &release, last);
            }
            return Err(error);
        }
        if let Err(error) = post_pointer(pid, id, frame, &event, point) {
            if pressed {
                release.set_location(CGPoint::new(last[0], last[1]));
                let _ = post_pointer(pid, id, frame, &release, last);
            }
            return Err(error);
        }
        pressed = true;
        last = point;
        pump(Duration::from_millis(5));
    }
    // Always balance down, including a takeover just after the final movement.
    release.set_location(CGPoint::new(last[0], last[1]));
    post_pointer(pid, id, frame, &release, last)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unaddressable_and_outside_events_before_posting() {
        for (pid, id, frame, point) in [
            (0, Some(2), Some([0., 0., 10., 10.]), [1., 1.]),
            (1, None, Some([0., 0., 10., 10.]), [1., 1.]),
            (1, Some(0), Some([0., 0., 10., 10.]), [1., 1.]),
            (1, Some(2), None, [1., 1.]),
            (1, Some(2), Some([0., 0., 10., 10.]), [10., 1.]),
            (1, Some(2), Some([0., 0., 10., 10.]), [f64::NAN, 1.]),
        ] {
            assert!(local_point(pid, id, frame, point).is_err());
        }
        let (id, p) =
            local_point(42, Some(9), Some([-800., 20., 600., 400.]), [-700., 120.]).unwrap();
        assert_eq!(id, 9);
        assert_eq!([p.x, p.y], [100., 100.]);
    }
    #[test]
    fn drag_modifiers_are_local_and_reject_duplicates() {
        assert_eq!(
            drag_flags(&["shift".into(), "alt".into()]).unwrap(),
            CGEventFlags::CGEventFlagShift | CGEventFlags::CGEventFlagAlternate
        );
        assert!(drag_flags(&["shift".into(), "shift".into()]).is_err());
        assert!(drag_flags(&["capslock".into()]).is_err());
    }
    #[test]
    #[ignore = "live owned AppKit receiver; requires Accessibility permission"]
    fn owned_receiver_accepts_first_production_click() {
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("BackgroundFixture.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.nanocodex.background-fixture</string><key>CFBundleExecutable</key><string>BackgroundFixture</string><key>CFBundleName</key><string>BackgroundFixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
        let executable = bundle.join("Contents/MacOS/BackgroundFixture");
        assert!(
            Command::new("swiftc")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/native/macos_background_fixture.swift"
                ))
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        let log = dir.path().join("events");
        std::fs::write(&log, "").unwrap();
        let front = || {
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|a| a.processIdentifier())
        };
        let cursor = || {
            let p = CGEvent::new(source().unwrap()).unwrap().location();
            [p.x, p.y]
        };
        let before_front = front();
        let before_cursor = cursor();
        let mut child = Command::new(&executable)
            .arg(&log)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        struct Stop<'a>(&'a mut std::process::Child);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _stop = Stop(&mut child);
        let deadline = Instant::now() + Duration::from_secs(10);
        let header = loop {
            let contents = std::fs::read_to_string(&log).unwrap();
            if contents.starts_with("ready ") {
                break contents;
            }
            assert!(Instant::now() < deadline, "fixture startup timed out");
            pump(Duration::from_millis(10));
        };
        let pid: i32 = header.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut desktop = MacDesktop::new();
        let app = desktop.bind(&pid.to_string()).unwrap();
        desktop.snapshot(&app).unwrap();
        let identity = desktop
            .handles
            .iter()
            .find(|(_, ax)| ax.text("AXRole").as_deref() == Some("AXTextArea"))
            .unwrap()
            .0
            .clone();
        eprintln!(
            "fixture={header} target={:?} frame={:?} id={:?}",
            desktop.handle(&identity).unwrap().frame(),
            desktop.current_context(&app).unwrap().frame,
            desktop.current_context(&app).unwrap().window_id
        );
        desktop
            .action(
                &app,
                Action::Click {
                    target: Target::Element { identity },
                    button: 0,
                    count: 1,
                },
            )
            .unwrap();
        desktop
            .action(
                &app,
                Action::TypeText {
                    text: "fresh-click".into(),
                },
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let receipt = std::fs::read_to_string(&log).unwrap();
            if receipt.contains("text-state fresh-click") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fresh production click did not focus editor: {receipt}"
            );
            pump(Duration::from_millis(10));
        }
        assert_eq!(front(), before_front);
        assert_eq!(cursor(), before_cursor);
    }
    #[test]
    #[ignore = "live owned AppKit receiver; requires Accessibility permission"]
    fn owned_receiver_accepts_pointer_and_keyboard_without_front_or_cursor_changes() {
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("BackgroundFixture.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.nanocodex.background-fixture</string><key>CFBundleExecutable</key><string>BackgroundFixture</string><key>CFBundleName</key><string>BackgroundFixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
        let executable = bundle.join("Contents/MacOS/BackgroundFixture");
        assert!(
            Command::new("swiftc")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/native/macos_background_fixture.swift"
                ))
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        let log = dir.path().join("events");
        std::fs::write(&log, "").unwrap();
        let front = || {
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|a| a.processIdentifier())
        };
        let cursor = || {
            let p = CGEvent::new(source().unwrap()).unwrap().location();
            [p.x, p.y]
        };
        let before_front = front();
        let before_cursor = cursor();
        let mut child = Command::new(&executable)
            .arg(&log)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        struct Stop<'a>(&'a mut std::process::Child);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _stop = Stop(&mut child);
        let deadline = Instant::now() + Duration::from_secs(10);
        let header = loop {
            let contents = std::fs::read_to_string(&log).unwrap();
            if contents.starts_with("ready ") {
                break contents;
            }
            assert!(Instant::now() < deadline, "fixture startup timed out");
            pump(Duration::from_millis(10));
        };
        let values: Vec<f64> = header
            .split_whitespace()
            .skip(1)
            .map(|v| v.parse().unwrap())
            .collect();
        let pid = values[0] as i32;
        let id = values[1] as u32;
        let frame = [values[2], values[3], values[4], values[5]];
        let p = [frame[0] + 100., frame[1] + 100.];
        let action_start = Instant::now();
        focus_window(pid, Some(id)).unwrap();
        pump(Duration::from_millis(10));
        for kind in [
            CGEventType::MouseMoved,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseDragged,
            CGEventType::LeftMouseUp,
        ] {
            let event = CGEvent::new_mouse_event(
                source().unwrap(),
                kind,
                CGPoint::new(p[0], p[1]),
                CGMouseButton::Left,
            )
            .unwrap();
            event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 1);
            post_pointer(pid, Some(id), Some(frame), &event, p).unwrap();
            pump(Duration::from_millis(10));
        }
        let scroll = CGEvent::new_scroll_event(source().unwrap(), 0, 1, -40, 0, 0).unwrap();
        scroll.set_location(CGPoint::new(p[0], p[1]));
        post_pointer(pid, Some(id), Some(frame), &scroll, p).unwrap();
        for event in text_events(&source().unwrap(), 'z').unwrap() {
            event.post_to_pid(pid);
        }
        drag(
            pid,
            Some(id),
            Some(frame),
            p,
            [p[0] + 30., p[1] + 20.],
            2,
            &["shift".into()],
        )
        .unwrap();
        let editor_point = [frame[0] + 250., frame[1] + 100.];
        for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
            let event = CGEvent::new_mouse_event(
                source().unwrap(),
                kind,
                CGPoint::new(editor_point[0], editor_point[1]),
                CGMouseButton::Left,
            )
            .unwrap();
            event.set_flags(CGEventFlags::empty());
            event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 1);
            post_pointer(pid, Some(id), Some(frame), &event, editor_point).unwrap();
        }
        pump(Duration::from_millis(20));
        for event in text_events(&source().unwrap(), 'q').unwrap() {
            event.post_to_pid(pid);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            let result = std::fs::read_to_string(&log).unwrap();
            assert_eq!(front(), before_front, "background input changed front app");
            let current = cursor();
            assert!(
                (current[0] - p[0]).abs() > 1. || (current[1] - p[1]).abs() > 1.,
                "global cursor followed injected pointer"
            );
            if [
                "down ",
                "up\n",
                "drag\n",
                "scroll\n",
                "key z",
                "middle-up",
                "standard-textview q",
            ]
            .iter()
            .all(|s| result.contains(s))
                || Instant::now() >= deadline
            {
                break result;
            }
            pump(Duration::from_millis(10));
        };
        eprintln!(
            "owned fixture receipt: {result}; cursor before={before_cursor:?}, after={:?}; front={before_front:?}",
            cursor()
        );
        for expected in [
            "down 100.0",
            "up\n",
            "drag\n",
            "scroll\n",
            "key z",
            "middle-down 2 shift=true",
            "middle-drag shift=true",
            "middle-up",
            "standard-textview q",
        ] {
            assert!(result.contains(expected), "missing {expected}: {result}");
        }
        eprintln!("background input elapsed: {:?}", action_start.elapsed());
        let mut desktop = MacDesktop::new();
        let app = desktop.bind(&pid.to_string()).unwrap();
        assert_eq!(app.pid, pid);
        assert!(desktop.validate_app(&app).unwrap());
        desktop.snapshot(&app).unwrap();
        let start = Instant::now();
        let production_cursor = cursor();
        desktop
            .action(
                &app,
                Action::PressKey {
                    key: "super+a".into(),
                },
            )
            .unwrap();
        assert_eq!(
            front(),
            before_front,
            "production chord changed the foreground"
        );
        desktop
            .action(
                &app,
                Action::TypeText {
                    text: "β🧪background".into(),
                },
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if std::fs::read_to_string(&log)
                .unwrap()
                .contains("production-textview β🧪background")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "production native path did not update standard text view: {}",
                std::fs::read_to_string(&log).unwrap()
            );
            pump(Duration::from_millis(10));
        }
        assert_eq!(
            front(),
            before_front,
            "production CUA changed the foreground"
        );
        let after_cursor = cursor();
        assert!(
            (after_cursor[0] - editor_point[0]).abs() > 1.
                || (after_cursor[1] - editor_point[1]).abs() > 1.,
            "production input moved the global cursor to the editor"
        );
        eprintln!(
            "production key+Unicode insertion elapsed: {:?}; cursor before={production_cursor:?}, after={after_cursor:?}; front={:?}; receipt: {}",
            start.elapsed(),
            front(),
            std::fs::read_to_string(&log).unwrap()
        );
        let capture_start = Instant::now();
        let configuration = super::super::super::screenshot::Configuration::default();
        let geometry =
            super::super::super::screenshot::Geometry::new(frame, 1., configuration).unwrap();
        let captured =
            capture_window(pid, frame, Some(id), geometry, configuration.encoding).unwrap();
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(captured.data)
            .unwrap();
        let pixels = image::load_from_memory(&bytes).unwrap().to_rgb8();
        let pixel = pixels.get_pixel(pixels.width() / 4, pixels.height() / 2);
        assert!(
            pixel[2] > 200 && pixel[0] < 50 && pixel[1] < 50,
            "capture must show owned blue content, not covering foreground: {pixel:?}"
        );
        assert_eq!(front(), before_front);
        eprintln!(
            "background window capture elapsed: {:?}",
            capture_start.elapsed()
        );
    }
}
