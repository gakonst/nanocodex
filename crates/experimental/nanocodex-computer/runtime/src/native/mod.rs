use crate::{Error, Result, ax::Node, selection::TextRange};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
pub mod audio;
#[cfg(any(target_os = "macos", test))]
mod instructions;
pub mod keys;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod render;
pub mod screenshot;
pub mod scroll;
pub mod text_source;
pub mod url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct App {
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
    fn action(&mut self, app: &App, action: Action) -> Result<()>;
    fn screenshot(&mut self, _app: &App) -> Result<Image> {
        Err(Error::unsupported("Screenshot backend unavailable"))
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
