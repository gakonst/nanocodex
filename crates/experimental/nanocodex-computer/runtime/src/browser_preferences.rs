//! Host-owned browser routing. Looking up the OS default never launches an app.
use crate::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preferences {
    pub default_browser: Option<String>,
    #[serde(default)]
    pub origins: BTreeMap<String, String>,
}

impl Preferences {
    pub(super) fn validate(&self, exists: impl Fn(&str) -> bool) -> Result<()> {
        if self.origins.len() > 128 {
            return Err(Error::invalid("Browser preferences exceed 128 origins"));
        }
        for id in self.default_browser.iter().chain(self.origins.values()) {
            if !exists(id) {
                return Err(Error::invalid(format!(
                    "Browser preference names an unregistered browser: {id}"
                )));
            }
        }
        for origin in self.origins.keys() {
            let url = url::Url::parse(origin)
                .map_err(|_| Error::invalid("Invalid browser preference origin"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.origin().ascii_serialization() != *origin
            {
                return Err(Error::invalid(
                    "Browser preference origins must be canonical HTTP(S) origins without paths",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn select(&self, url: Option<&url::Url>) -> Option<&str> {
        url.and_then(|url| self.origins.get(&url.origin().ascii_serialization()))
            .or(self.default_browser.as_ref())
            .map(String::as_str)
    }
}

pub(super) fn system_browser(url: Option<&url::Url>) -> Option<&'static str> {
    let scheme = url.map_or("https", url::Url::scheme);
    let application = system_application(scheme)?;
    match application
        .to_ascii_lowercase()
        .trim_end_matches(".desktop")
    {
        "com.google.chrome"
        | "google-chrome"
        | "google-chrome-stable"
        | "chromium"
        | "chromium-browser"
        | "org.chromium.chromium" => Some("chrome"),
        "com.microsoft.edgemac" | "microsoft-edge" | "microsoft-edge-stable" => Some("edge"),
        "com.brave.browser" | "brave-browser" => Some("brave"),
        "com.vivaldi.vivaldi" | "vivaldi-stable" | "vivaldi" => Some("vivaldi"),
        "com.operasoftware.opera" | "opera" => Some("opera"),
        "com.apple.safari" => Some("safari"),
        "org.mozilla.firefox" | "firefox" => Some("firefox"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn system_application(scheme: &str) -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSBundle, NSString, NSURL};
    let url = NSURL::URLWithString(&NSString::from_str(&format!("{scheme}://example.invalid/")))?;
    let application = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&url)?;
    NSBundle::bundleWithURL(&application)?
        .bundleIdentifier()
        .map(|id| id.to_string())
}

#[cfg(target_os = "linux")]
fn system_application(scheme: &str) -> Option<String> {
    use std::{
        ffi::{CStr, CString, c_char, c_void},
        ptr::NonNull,
        sync::OnceLock,
    };
    let scheme = CString::new(scheme).ok()?;
    // GIO reads the desktop's registered URI handler. No shell, UI connection,
    // browser launch or optional library is required to initialize the runtime.
    unsafe {
        // GIO registers process-global GTypes. Pin a single library handle even
        // when no application is registered for the requested scheme.
        static GIO: OnceLock<Option<libloading::Library>> = OnceLock::new();
        let gio = GIO
            .get_or_init(|| libloading::Library::new("libgio-2.0.so.0").ok())
            .as_ref()?;
        let get = gio
            .get::<unsafe extern "C" fn(*const c_char) -> *mut c_void>(
                b"g_app_info_get_default_for_uri_scheme\0",
            )
            .ok()?;
        let id = gio
            .get::<unsafe extern "C" fn(*mut c_void) -> *const c_char>(b"g_app_info_get_id\0")
            .ok()?;
        let unref = gio
            .get::<unsafe extern "C" fn(*mut c_void)>(b"g_object_unref\0")
            .ok()?;
        let app = NonNull::new(get(scheme.as_ptr()))?;
        let result = NonNull::new(id(app.as_ptr()).cast_mut())
            .map(|id| CStr::from_ptr(id.as_ptr()).to_string_lossy().into_owned());
        unref(app.as_ptr());
        result
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn system_application(_: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::super::Browsers;
    use serde_json::json;

    fn browsers() -> Browsers {
        let mut browsers = Browsers::default();
        for id in ["alpha", "work", "personal"] {
            browsers
                .register(id, "ws://127.0.0.1:1/never-connected")
                .unwrap();
        }
        browsers
    }

    #[test]
    fn exact_origin_preference_precedes_default_without_opening_a_browser() {
        let mut browsers = browsers();
        browsers
            .configure_preferences(
                serde_json::from_value(
                    json!({"defaultBrowser":"personal","origins":{"https://work.example":"work"}}),
                )
                .unwrap(),
            )
            .unwrap();
        for (method, args, expected) in [
            ("get_default_browser", json!({}), "personal"),
            (
                "get_browser_for_url",
                json!({"url":"https://work.example/path?q=1"}),
                "work",
            ),
            (
                "get_browser_for_url",
                json!({"url":"https://work.example.attacker.invalid/"}),
                "personal",
            ),
            (
                "get_browser_for_url",
                json!({"url":"http://work.example/"}),
                "personal",
            ),
            (
                "get_browser_for_url",
                json!({"url":"https://work.example:444/"}),
                "personal",
            ),
        ] {
            assert_eq!(browsers.execute(method, &args).unwrap()["id"], expected);
        }
        assert!(
            browsers
                .providers
                .values()
                .all(|provider| provider.client.is_none())
        );
    }

    #[test]
    fn invalid_preferences_preserve_the_existing_routing_and_never_expand_host_scope() {
        let mut browsers = browsers();
        browsers
            .configure_preferences(
                serde_json::from_value(json!({"defaultBrowser":"work"})).unwrap(),
            )
            .unwrap();
        for invalid in [
            json!({"defaultBrowser":"missing"}),
            json!({"origins":{"https://work.example/path":"work"}}),
            json!({"origins":{"https://USER@work.example":"work"}}),
        ] {
            assert!(
                browsers
                    .configure_preferences(serde_json::from_value(invalid).unwrap())
                    .is_err()
            );
            assert_eq!(
                browsers.execute("get_default_browser", &json!({})).unwrap()["id"],
                "work"
            );
        }
        browsers.host_managed = true;
        assert!(
            browsers
                .execute("get_default_browser", &json!({}))
                .unwrap_err()
                .message
                .contains("unavailable")
        );
        assert!(
            browsers
                .providers
                .values()
                .all(|provider| provider.client.is_none())
        );
    }
}
