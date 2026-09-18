use crate::{Error, Result, ax::Node, selection::TextRange};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
pub mod audio;
#[cfg(any(target_os = "macos", test))]
mod instructions;
pub mod keys;
#[cfg(target_os = "linux")]
mod linux_background;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod render;
pub mod screenshot;
pub mod scroll;
pub mod text_source;
pub mod url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct App {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u32>,
    pub id: String,
    pub name: String,
    pub path: String,
    pub pid: i32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Image {
    pub mime_type: String,
    pub data: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    Element { identity: String },
    Point { point: [f64; 2] },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Click {
        target: Target,
        button: u8,
        count: u32,
    },
    Drag {
        from: [f64; 2],
        to: [f64; 2],
        #[serde(default)]
        button: u8,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    PressKey {
        key: String,
    },
    TypeText {
        text: String,
    },
    SetValue {
        identity: String,
        value: String,
    },
    SelectText {
        identity: String,
        range: TextRange,
    },
    Scroll {
        target: Target,
        direction: String,
        pages: f64,
    },
    Secondary {
        identity: String,
        action: String,
    },
    Paste {
        text: String,
        format: String,
    },
}
pub trait Desktop {
    /// Resolve a native window/app approval target without capture or launch.
    fn sky_policy_target(&mut self, method: &str, params: &serde_json::Value) -> Result<App> {
        let input = if method == "launch_app" || method == "get_window" {
            params
        } else {
            &params["window"]
        };
        let app = input["app"]
            .as_str()
            .ok_or_else(|| Error::invalid("Approval target app is required"))?;
        self.app_policy_target(app)
    }
    fn synthetic(&self) -> bool {
        false
    }
    /// Installed Sky exposes different method families for each desktop target.
    /// The deterministic test fixture follows the macOS window API.
    fn sky_target(&self) -> &'static str {
        "mac"
    }
    /// App-scoped contracts are independent of the operating-system label.
    /// A compositor backend may implement them without exposing global input.
    fn app_interface(&self) -> bool {
        self.sky_target() == "mac"
    }
    fn sky_execute(
        &mut self,
        method: &str,
        _args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(Error::unsupported(format!(
            "Native Sky method unavailable: {method}"
        )))
    }
    fn apps(&mut self) -> Result<Vec<App>>;
    fn app_windows(&mut self, _app: &App) -> Result<serde_json::Value> {
        Err(Error::unsupported(
            "Explicit native windows are unavailable",
        ))
    }
    /// Return an exact native target eligible for an isolated subprocess lane.
    /// Implicit app names and executable paths must not select a lane.
    fn window_lane_binding(&mut self, _identifier: &str) -> Result<Option<App>> {
        Ok(None)
    }
    /// Stable window binding, separate from executable-based policy authorization.
    fn session_key(&self, app: &App) -> String {
        app.path.clone()
    }
    /// Validate a cached handle against its process identity without rediscovery.
    /// Native backends may use a direct PID/window lookup on the input hot path.
    fn validate_app(&mut self, app: &App) -> Result<bool> {
        Ok(self.apps()?.iter().any(|current| {
            current.pid == app.pid && current.path == app.path && current.id == app.id
        }))
    }
    /// Resolve policy metadata without opening or activating an application.
    fn app_policy_target(&mut self, identifier: &str) -> Result<App> {
        let mut matches = self.apps()?.into_iter().filter(|app| {
            app.id == identifier
                || app.path == identifier
                || app.name == identifier
                || app.pid.to_string() == identifier
        });
        let first = matches
            .next()
            .ok_or_else(|| Error::action("Application not found"))?;
        if matches.next().is_some() {
            return Err(Error::action("Ambiguous application identifier"));
        }
        Ok(first)
    }
    fn sky_apps(&mut self) -> Result<serde_json::Value> {
        Ok(
            serde_json::json!(self.apps()?.into_iter().map(|app|serde_json::json!({
            "bundleIdentifier":app.id,"displayName":app.name,"appPath":app.path,"isRunning":true,
        })).collect::<Vec<_>>()),
        )
    }
    fn bind(&mut self, identifier: &str) -> Result<App> {
        let mut matches: Vec<_> = self
            .apps()?
            .into_iter()
            .filter(|a| {
                a.id == identifier
                    || a.path == identifier
                    || a.name == identifier
                    || a.pid.to_string() == identifier
            })
            .collect();
        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => Err(Error::action(format!(
                "Application not found: {identifier}"
            ))),
            _ => Err(Error::action(format!(
                "Application identifier is ambiguous: {identifier}"
            ))),
        }
    }
    /// Optional native guidance for the already bound app, separate from the
    /// per-client formatter cache. The default backend provides no guidance.
    fn app_specific_instructions(&mut self, _app: &App) -> Option<String> {
        None
    }
    fn snapshot(&mut self, app: &App) -> Result<Node>;
    /// Refresh the target window for a visual observation without constructing
    /// the accessibility tree. Backends without a separate path may snapshot.
    fn prepare_screenshot(&mut self, app: &App) -> Result<()> {
        self.snapshot(app).map(|_| ())
    }
    fn action(&mut self, app: &App, action: Action) -> Result<()>;
    /// Visual ownership follows the JavaScript kernel without changing app state.
    fn action_in_scope(&mut self, app: &App, action: Action, _scope: &str) -> Result<()> {
        self.action(app, action)
    }
    fn reset_visual_scope(&mut self, _scope: &str) -> Result<()> {
        Ok(())
    }
    fn screenshot(&mut self, _app: &App) -> Result<Image> {
        Err(Error::unsupported("Screenshot backend unavailable"))
    }
    /// Explicit read-only capture of the main display. Never binds an app or
    /// publishes coordinates for native actions; app captures must not fall back here.
    fn desktop_screenshot(&mut self) -> Result<Image> {
        Err(Error::unsupported("Desktop screenshot backend unavailable"))
    }
    /// Only an explicit model observation may publish new screenshot coordinates.
    /// Preview captures and exports use `screenshot` without rebinding that state.
    fn screenshot_for_observation(&mut self, app: &App) -> Result<Image> {
        self.screenshot(app)
    }
    /// Revoke coordinate authority after failed observation or media publication.
    fn invalidate_screenshot(&mut self, _app: &App) {}
    fn capabilities(&self) -> Vec<&'static str>;
    fn audio(
        &mut self,
        _method: &str,
        _owner: &str,
        _params: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(Error::unsupported("Native audio backend unavailable"))
    }
    /// Cancel and discard the owner's recording without invalidating app handles.
    fn cancel_audio(&mut self, _owner: &str) -> Result<()> {
        Ok(())
    }
    fn end_session(&mut self, _owner: &str) -> Result<()> {
        Ok(())
    }
    fn diagnostics(&self, _app: &App) -> serde_json::Value {
        serde_json::json!({})
    }
    fn control_fixture(
        &mut self,
        _method: &str,
        _args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(Error::unsupported(
            "Fixture controls unavailable on this backend",
        ))
    }
}
pub fn create() -> Result<Box<dyn Desktop>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::MacDesktop::new()))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(crate::platforms::win32::Win32::new()))
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(mode) = std::env::var_os("NANOCODEX_COMPUTER_BACKGROUND") {
            if mode != "hyprland" {
                return Err(Error::invalid("Unsupported background CUA mode"));
            }
            return Ok(Box::new(linux_background::Hyprland::from_environment()?));
        }
        Ok(Box::new(
            crate::platforms::linux::LinuxDesktop::from_environment()?,
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::unsupported(
            "No native backend for this platform; select --fixture",
        ))
    }
}

/// Translate the point-resolution window screenshot coordinate into desktop space.
pub fn window_point(frame: [f64; 4], point: [f64; 2]) -> Result<[f64; 2]> {
    let [x, y, w, h] = frame;
    if !frame.into_iter().chain(point).all(f64::is_finite)
        || w <= 0.
        || h <= 0.
        || point[0] < 0.
        || point[1] < 0.
        || point[0] >= w
        || point[1] >= h
    {
        return Err(Error::invalid("Point outside valid window bounds"));
    }
    Ok([x + point[0], y + point[1]])
}

/// Permission status is read-only by default. Prompting is an explicit UI flow;
/// this never edits the TCC database, profiles or another app's authorization.
pub fn permissions(request: bool) -> Result<serde_json::Value> {
    #[cfg(target_os = "macos")]
    {
        macos::permissions(request)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = request;
        Err(Error::unsupported(
            "This permission flow is specific to macOS",
        ))
    }
}

#[cfg(test)]
mod desktop_screenshot_tests {
    use super::*;

    struct UnsupportedDesktop;
    impl Desktop for UnsupportedDesktop {
        fn apps(&mut self) -> Result<Vec<App>> {
            panic!("Desktop capture must not enumerate or bind applications")
        }
        fn snapshot(&mut self, _app: &App) -> Result<Node> {
            panic!("Desktop capture must not read AX or publish app coordinates")
        }
        fn action(&mut self, _app: &App, _action: Action) -> Result<()> {
            panic!("Desktop capture must not perform app actions")
        }
        fn screenshot(&mut self, _app: &App) -> Result<Image> {
            panic!("Desktop capture must not fall back to app screenshots")
        }
        fn capabilities(&self) -> Vec<&'static str> {
            vec![]
        }
    }

    #[test]
    fn unsupported_desktop_capture_has_no_app_fallback() {
        let error = UnsupportedDesktop.desktop_screenshot().unwrap_err();
        let expected = Error::unsupported("Desktop screenshot backend unavailable");
        assert_eq!(error.code, expected.code);
        assert_eq!(error.message, expected.message);
    }
}

/// Explicit native window binding, retaining executable identity for policy.
pub fn window_binding(identifier: &str) -> Result<Option<(&str, u32)>> {
    let Some((app, id)) = identifier.rsplit_once("#window=") else {
        return Ok(None);
    };
    let id = id
        .parse::<u32>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| Error::invalid("window ID must be a positive u32"))?;
    if app.is_empty() {
        return Err(Error::invalid("Window binding requires an app"));
    }
    Ok(Some((app, id)))
}

#[cfg(test)]
mod window_binding_tests {
    use super::*;
    #[test]
    fn window_binding_is_exact_and_rejects_invalid_ids() {
        assert_eq!(
            window_binding("/Applications/Owned.app#window=42").unwrap(),
            Some(("/Applications/Owned.app", 42))
        );
        assert_eq!(window_binding("org.owned").unwrap(), None);
        for id in ["", "0", "-1", "4294967296", "2.5", "42junk"] {
            assert!(window_binding(&format!("org.owned#window={id}")).is_err());
        }
        assert!(window_binding("#window=42").is_err());
    }
}

// Worker cancellation is installed only on the native owning thread. A private
// pipe reader can revoke it without ever touching AppKit or native state.
thread_local! {
    static NATIVE_DEADLINE: std::cell::RefCell<Option<(std::time::Instant, std::sync::Arc<std::sync::atomic::AtomicU64>)>> = const { std::cell::RefCell::new(None) };
    static NATIVE_CANCELLATION: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = const { std::cell::RefCell::new(None) };
}
pub fn set_native_cancellation(cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) {
    NATIVE_CANCELLATION.with(|slot| *slot.borrow_mut() = cancelled);
}
pub fn set_native_execution_deadline(
    start: std::time::Instant,
    deadline: std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    NATIVE_DEADLINE.with(|slot| *slot.borrow_mut() = Some((start, deadline)));
}
pub fn check_native_cancellation() -> Result<()> {
    if NATIVE_DEADLINE.with(|slot| {
        slot.borrow().as_ref().is_some_and(|(start, deadline)| {
            start.elapsed().as_millis()
                >= u128::from(deadline.load(std::sync::atomic::Ordering::Acquire))
        })
    }) {
        return Err(Error::new(-32800, "Native window admission expired"));
    }
    if NATIVE_CANCELLATION.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(std::sync::atomic::Ordering::Acquire))
    }) {
        Err(Error::new(-32800, "Native window call revoked"))
    } else {
        Ok(())
    }
}

/// Minimal backend context inherited by native subprocess lanes.
pub fn window_lane_environment() -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    #[cfg(target_os = "linux")]
    if std::env::var_os("NANOCODEX_COMPUTER_BACKGROUND").is_some() {
        return linux_background::window_lane_environment();
    }
    Ok(Vec::new())
}

mod window_capture;
pub use window_capture::{WindowCapture, set_window_capture_delegate, start_window_capture};
