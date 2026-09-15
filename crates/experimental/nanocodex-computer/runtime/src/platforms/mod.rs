//! Opt-in platform providers. These never discover or launch proprietary helpers.
use crate::{Error, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
pub mod audio;
pub mod linux;
#[cfg(target_os = "linux")]
pub mod linux_audio;
pub mod linux_audio_model;
#[cfg(target_os = "linux")]
pub mod linux_clipboard;
#[cfg(target_os = "linux")]
pub mod linux_x11;
pub mod process;
#[cfg(target_os = "windows")]
pub mod win32;
pub mod windows;
#[cfg(target_os = "windows")]
pub mod windows_audio;
pub mod windows_audio_model;
#[cfg(target_os = "windows")]
pub mod windows_capture;
pub mod windows_capture_model;
#[cfg(target_os = "windows")]
pub mod windows_uia;
pub mod windows_uia_model;

enum Provider {
    Linux(linux::Linux),
    Windows(windows::Windows),
    X11(linux::X11),
    Audio(audio::Audio),
}
pub struct Platforms {
    providers: BTreeMap<String, Provider>,
    registration_enabled: bool,
    turn_context: Value,
}
impl Platforms {
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
            registration_enabled: false,
            turn_context: json!({"session_id":format!("platform-{}",std::process::id()),"turn_id":"initial"}),
        }
    }
    /// Trusted host-only metadata; RPC callers cannot change a physical-Escape turn.
    pub fn set_turn_context(&mut self, session_id: &str, turn_id: &str) -> Result<()> {
        if session_id.trim().is_empty()
            || turn_id.trim().is_empty()
            || session_id.len() > 4096
            || turn_id.len() > 4096
        {
            return Err(Error::invalid("Invalid host turn identity"));
        }
        self.turn_context = json!({"session_id":session_id,"turn_id":turn_id});
        Ok(())
    }
    /// Privileged host configuration entry point; never expose this directly to page JS.
    pub fn configure(&mut self, params: &Value) -> Result<Value> {
        self.registration_enabled = true;
        let result = self.execute("platform.register", params);
        self.registration_enabled = false;
        result
    }
    pub fn has_sky_audio(&self) -> bool {
        self.providers
            .values()
            .any(|provider| matches!(provider, Provider::Audio(_)))
    }
    /// An explicitly configured Linux monitor provider takes precedence over native audio.
    /// Ambiguous configuration fails before any external process is started.
    pub fn sky_audio(&mut self, method: &str, params: &Value) -> Result<Value> {
        if !matches!(method, "start_audio_recording" | "stop_audio_recording") {
            return Err(Error::unsupported("Unsupported Linux Sky audio method"));
        }
        let mut audio = self
            .providers
            .values_mut()
            .filter_map(|provider| match provider {
                Provider::Audio(audio) => Some(audio),
                _ => None,
            });
        let provider = audio.next().ok_or_else(|| {
            Error::unsupported("Linux Sky audio requires one host-configured linux_audio provider")
        })?;
        if audio.next().is_some() {
            return Err(Error::invalid("Linux Sky audio provider is ambiguous"));
        }
        provider.execute(method, params)
    }
    pub fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        if !params.is_object() {
            return Err(Error::invalid("Platform parameters must be an object"));
        }
        match method {
            "platform.capabilities" => Ok(
                json!({"kinds":["linux_helper","windows_helper","linux_x11","linux_audio"],"registration":"explicit executable paths","native_validation":"synthetic process fixtures only on this build","methods":["platform.register","platform.list","platform.unregister","platform.call","platform.events"]}),
            ),
            "platform.list" => Ok(json!(self.providers.keys().collect::<Vec<_>>())),
            "platform.register" => {
                if !self.registration_enabled {
                    return Err(Error::new(
                        -32003,
                        "Platform executable registration is restricted to host configuration",
                    ));
                }
                let id = required(params, "id")?;
                if self.providers.contains_key(id) {
                    return Err(Error::invalid("Platform provider already registered"));
                }
                if self.providers.len() >= 16 {
                    return Err(Error::invalid("Platform provider limit reached"));
                }
                let kind = required(params, "kind")?;
                let provider = match kind {
                    "linux_helper" => Provider::Linux(linux::Linux::new(
                        executable(params, "executable")?,
                        strings(params, "args")?,
                        duration(params, "timeout_ms", 15000)?,
                        unsigned(params, "post_action_sleep_ms", 100)?,
                        params
                            .get("mouse_size_px")
                            .map(|v| {
                                v.as_u64()
                                    .ok_or_else(|| Error::invalid("mouse_size_px must be unsigned"))
                            })
                            .transpose()?,
                    )?),
                    "windows_helper" => Provider::Windows(windows::Windows::new(
                        executable(params, "executable")?,
                        strings(params, "args")?,
                        duration(params, "timeout_ms", 15000)?,
                        params
                            .get("state_directory")
                            .and_then(Value::as_str)
                            .map(PathBuf::from),
                    )?),
                    "linux_audio" => Provider::Audio(audio::Audio::new(
                        executable(params, "pactl")?,
                        executable(params, "ffmpeg")?,
                        PathBuf::from(required(params, "directory")?),
                    )?),
                    "linux_x11" => Provider::X11(linux::X11::new(
                        executable(params, "xdotool")?,
                        executable(params, "screenshot_tool")?,
                        duration(params, "timeout_ms", 15000)?,
                    )),
                    _ => return Err(Error::invalid("Unknown platform provider kind")),
                };
                self.providers.insert(id.into(), provider);
                Ok(json!({"id":id,"kind":kind}))
            }
            "platform.unregister" => {
                let id = required(params, "id")?;
                if self.providers.remove(id).is_none() {
                    return Err(Error::action("Unknown platform provider"));
                }
                Ok(json!({"closed":true}))
            }
            "platform.call" => {
                if params.get("meta").is_some() {
                    return Err(Error::new(
                        -32003,
                        "Platform metadata is controlled by the host",
                    ));
                }
                let id = required(params, "id")?;
                let method = required(params, "method")?;
                let args = params.get("params").cloned().unwrap_or(json!({}));
                if !args.is_object() {
                    return Err(Error::invalid("Provider params must be an object"));
                }
                match self
                    .providers
                    .get_mut(id)
                    .ok_or_else(|| Error::action("Unknown platform provider"))?
                {
                    Provider::Linux(p) => p.execute(method, &args),
                    Provider::Windows(p) => p.execute(method, &args, self.turn_context.clone()),
                    Provider::X11(p) => p.execute(method, &args),
                    Provider::Audio(p) => p.execute(method, &args),
                }
            }
            "platform.events" => {
                let id = required(params, "id")?;
                match self
                    .providers
                    .get_mut(id)
                    .ok_or_else(|| Error::action("Unknown platform provider"))?
                {
                    Provider::Windows(p) => Ok(p.drain_events()),
                    _ => Ok(json!([])),
                }
            }
            _ => Err(Error::unsupported(format!(
                "Unknown platform method: {method}"
            ))),
        }
    }
    /// Discard configured recordings on kernel reset, without closing other providers.
    pub fn cancel_sky_audio(&mut self) -> Result<()> {
        for provider in self.providers.values_mut() {
            if let Provider::Audio(audio) = provider {
                audio.cancel()?;
            }
        }
        Ok(())
    }
    pub fn end_turn(&mut self) -> Result<()> {
        for provider in self.providers.values_mut() {
            match provider {
                Provider::Linux(p) => p.end_turn()?,
                Provider::Windows(p) => {
                    p.end_turn()?;
                }
                Provider::X11(_) => (),
                Provider::Audio(p) => p.end_turn()?,
            }
        }
        Ok(())
    }
}
pub(crate) fn required<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Error::invalid(format!("{key} must be a non-empty string")))
}
pub(crate) fn unsigned(params: &Value, key: &str, default: u64) -> Result<u64> {
    params
        .get(key)
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error::invalid(format!("{key} must be unsigned")))
        })
        .transpose()
        .map(|v| v.unwrap_or(default))
}
fn strings(params: &Value, key: &str) -> Result<Vec<String>> {
    params
        .get(key)
        .map(|v| {
            serde_json::from_value(v.clone())
                .map_err(|_| Error::invalid(format!("{key} must be an array of strings")))
        })
        .transpose()
        .map(|v| v.unwrap_or_default())
}
fn duration(params: &Value, key: &str, default: u64) -> Result<Duration> {
    let ms = unsigned(params, key, default)?;
    if !(1..=300000).contains(&ms) {
        return Err(Error::invalid("Timeout must be 1..300000ms"));
    }
    Ok(Duration::from_millis(ms))
}
fn executable(params: &Value, key: &str) -> Result<PathBuf> {
    let path = Path::new(required(params, key)?);
    if !path.is_absolute() || !path.is_file() {
        return Err(Error::invalid(format!(
            "{key} must be an existing absolute executable path"
        )));
    }
    Ok(path.to_path_buf())
}

impl Default for Platforms {
    fn default() -> Self {
        Self::new()
    }
}
