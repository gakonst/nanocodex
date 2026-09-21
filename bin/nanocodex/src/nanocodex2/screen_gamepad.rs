//! Linux Xbox-layout uinput device. Full snapshots expire after 500 ms.
use nix::libc;
use serde::Deserialize;
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GamepadState {
    left_x: f64,
    left_y: f64,
    right_x: f64,
    right_y: f64,
    left_trigger: f64,
    right_trigger: f64,
    buttons: Vec<String>,
}
const BUTTONS: [(&str, u16); 10] = [
    ("a", 0x130),
    ("b", 0x131),
    ("x", 0x133),
    ("y", 0x134),
    ("leftShoulder", 0x136),
    ("rightShoulder", 0x137),
    ("leftStick", 0x13d),
    ("rightStick", 0x13e),
    ("back", 0x13a),
    ("start", 0x13b),
];
const HATS: [&str; 4] = ["dpadUp", "dpadDown", "dpadLeft", "dpadRight"];
const AXES: [u16; 8] = [0, 1, 2, 3, 4, 5, 16, 17];
impl GamepadState {
    pub(crate) fn validate(&self) -> Result<()> {
        for (i, v) in [
            self.left_x,
            self.left_y,
            self.right_x,
            self.right_y,
            self.left_trigger,
            self.right_trigger,
        ]
        .iter()
        .enumerate()
        {
            if !v.is_finite() || *v > 1.0 || *v < if i < 4 { -1.0 } else { 0.0 } {
                return Err("invalid gamepad axis".into());
            }
        }
        for (i, b) in self.buttons.iter().enumerate() {
            if self.buttons[..i].contains(b)
                || !(BUTTONS.iter().any(|(name, _)| b == name) || HATS.contains(&b.as_str()))
            {
                return Err("invalid gamepad button".into());
            }
        }
        Ok(())
    }
    fn events(&self) -> Vec<(u16, u16, i32)> {
        let held = |b: &str| i32::from(self.buttons.iter().any(|v| v == b));
        let stick = |v: f64| (v * if v < 0.0 { 32768.0 } else { 32767.0 }).round() as i32;
        let mut events: Vec<_> = BUTTONS.iter().map(|(b, c)| (1, *c, held(b))).collect();
        let values = [
            stick(self.left_x),
            stick(self.left_y),
            (self.left_trigger * 255.0).round() as i32,
            stick(self.right_x),
            stick(self.right_y),
            (self.right_trigger * 255.0).round() as i32,
            held("dpadRight") - held("dpadLeft"),
            held("dpadDown") - held("dpadUp"),
        ];
        events.extend(AXES.into_iter().zip(values).map(|(c, v)| (3, c, v)));
        events.push((0, 0, 0));
        events
    }
}
trait Device: Send {
    fn write_state(&mut self, state: &GamepadState) -> io::Result<()>;
}
struct Uinput(File);
// Keep the native boundary private to this module's fixed scalar uinput calls.
#[allow(unsafe_code)]
fn ioctl(file: &File, request: libc::c_ulong, value: i32) -> io::Result<()> {
    // SAFETY: all callers use Linux uinput scalar/no-argument ioctl constants,
    // never pointer-bearing requests. The borrowed File keeps the fd alive.
    if unsafe { libc::ioctl(file.as_raw_fd(), request, value) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
impl Uinput {
    fn open() -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open("/dev/uinput")?;
        for value in [1, 3] {
            ioctl(&file, 0x40045564, value)?;
        }
        for (_, code) in BUTTONS {
            ioctl(&file, 0x40045565, i32::from(code))?;
        }
        for code in AXES {
            ioctl(&file, 0x40045567, i32::from(code))?;
        }
        // uinput_user_dev's stable write ABI: name, input_id, effects, four ABS arrays.
        let mut setup = vec![0u8; 1116];
        let name = b"Nanocodex Virtual Xbox Controller";
        setup[..name.len()].copy_from_slice(name);
        for (i, value) in [3u16, 0x045e, 0x028e, 1].into_iter().enumerate() {
            setup[80 + i * 2..82 + i * 2].copy_from_slice(&value.to_ne_bytes());
        }
        for axis in AXES {
            let (min, max, flat): (i32, i32, i32) = match axis {
                2 | 5 => (0, 255, 0),
                16 | 17 => (-1, 1, 0),
                _ => (-32768, 32767, 2048),
            };
            for (array, value) in [(0, max), (1, min), (3, flat)] {
                let at = 92 + array * 256 + usize::from(axis) * 4;
                setup[at..at + 4].copy_from_slice(&value.to_ne_bytes());
            }
        }
        (&file).write_all(&setup)?;
        ioctl(&file, 0x5501, 0)?;
        let mut device = Self(file);
        device.write_state(&GamepadState::default())?;
        Ok(device)
    }
}
impl Device for Uinput {
    fn write_state(&mut self, state: &GamepadState) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(19 * std::mem::size_of::<libc::input_event>());
        for (kind, code, value) in state.events() {
            bytes.resize(bytes.len() + std::mem::size_of::<libc::timeval>(), 0);
            bytes.extend_from_slice(&kind.to_ne_bytes());
            bytes.extend_from_slice(&code.to_ne_bytes());
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        if self.0.write(&bytes)? != bytes.len() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        Ok(())
    }
}
impl Drop for Uinput {
    fn drop(&mut self) {
        let _ = self.write_state(&GamepadState::default());
        let _ = ioctl(&self.0, 0x5502, 0);
    }
}
struct State {
    device: Option<Box<dyn Device>>,
    deadline: Option<Instant>,
}
impl State {
    fn write(&mut self, s: &GamepadState) -> Result<()> {
        let result = self
            .device
            .as_mut()
            .ok_or("native gamepad unavailable")?
            .write_state(s);
        if let Err(e) = result {
            self.device.take();
            self.deadline = None;
            return Err(e.into());
        }
        Ok(())
    }
    fn expire(&mut self, now: Instant) {
        if self.deadline.is_some_and(|d| now >= d) {
            self.deadline = None;
            let _ = self.write(&GamepadState::default());
        }
    }
}
#[derive(Clone)]
pub(crate) struct Controller(Arc<Mutex<State>>);
impl Controller {
    pub(crate) fn configured() -> Self {
        let device = if std::env::var("NANOCODEX_VIRTUAL_GAMEPAD").as_deref() == Ok("1") {
            match Uinput::open() {
                Ok(d) => Some(Box::new(d) as Box<dyn Device>),
                Err(e) => {
                    eprintln!("Native gamepad unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        let this = Self(Arc::new(Mutex::new(State {
            device,
            deadline: None,
        })));
        let weak = Arc::downgrade(&this.0);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(25));
            loop {
                tick.tick().await;
                let Some(state) = weak.upgrade() else { break };
                state.lock().unwrap().expire(Instant::now());
            }
        });
        this
    }
    pub(crate) fn available(&self) -> bool {
        self.0.lock().unwrap().device.is_some()
    }
    pub(crate) fn apply(&self, s: &GamepadState) -> Result<()> {
        s.validate()?;
        let mut state = self.0.lock().unwrap();
        state.write(s)?;
        state.deadline = Some(Instant::now() + Duration::from_millis(500));
        Ok(())
    }
    pub(crate) fn release(&self) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        state.deadline = None;
        if state.device.is_none() {
            return Ok(());
        }
        state.write(&GamepadState::default())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn xbox_snapshot_axes_hats_and_syn() {
        let s:GamepadState=serde_json::from_value(json!({"leftX":-1,"leftY":1,"rightX":1,"rightY":-1,"leftTrigger":1,"rightTrigger":0.5,"buttons":["a","start","dpadUp","dpadRight"]})).unwrap();
        s.validate().unwrap();
        let e = s.events();
        assert_eq!(e.len(), 19);
        assert_eq!(e[0], (1, 0x130, 1));
        assert_eq!(
            &e[10..],
            &[
                (3, 0, -32768),
                (3, 1, 32767),
                (3, 2, 255),
                (3, 3, 32767),
                (3, 4, -32768),
                (3, 5, 128),
                (3, 16, 1),
                (3, 17, -1),
                (0, 0, 0)
            ]
        );
    }
    #[test]
    fn missing_snapshot_fields_and_duplicate_buttons_rejected() {
        assert!(serde_json::from_value::<GamepadState>(json!({"buttons":[]})).is_err());
        let s = GamepadState {
            buttons: vec!["a".into(), "a".into()],
            ..Default::default()
        };
        assert!(s.validate().is_err());
    }
    type EventLog = Arc<Mutex<Vec<Vec<(u16, u16, i32)>>>>;
    struct Fake(EventLog);
    impl Device for Fake {
        fn write_state(&mut self, s: &GamepadState) -> io::Result<()> {
            self.0.lock().unwrap().push(s.events());
            Ok(())
        }
    }
    #[test]
    fn watchdog_uses_latest_snapshot_deadline() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let controller = Controller(Arc::new(Mutex::new(State {
            device: Some(Box::new(Fake(log.clone()))),
            deadline: None,
        })));
        controller
            .apply(&GamepadState {
                left_x: 1.0,
                ..Default::default()
            })
            .unwrap();
        let first = controller.0.lock().unwrap().deadline.unwrap();
        controller
            .apply(&GamepadState {
                left_y: 1.0,
                ..Default::default()
            })
            .unwrap();
        let latest = controller.0.lock().unwrap().deadline.unwrap();
        controller
            .0
            .lock()
            .unwrap()
            .expire(first - Duration::from_millis(1));
        assert_eq!(log.lock().unwrap().len(), 2);
        controller.0.lock().unwrap().expire(latest);
        assert_eq!(
            log.lock().unwrap().last().unwrap(),
            &GamepadState::default().events()
        );
    }
}
