use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use eyre::{Result, WrapErr, eyre};
use nanocodex_browser::{
    BraveSession, BraveSessionError, Browser, BrowserProfileKind, BrowserStorageState, BrowserTool,
    FirefoxCookieSource, SafariCookieSource, VirtualAuthenticator,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum BrowserKind {
    Chromium,
    #[value(alias = "true")]
    Brave,
    #[value(alias = "false", alias = "off")]
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CookieSourceKind {
    #[value(alias = "auto", alias = "true")]
    All,
    Brave,
    Chrome,
    Chromium,
    Edge,
    Firefox,
    Safari,
    #[value(alias = "false", alias = "off")]
    None,
}

enum CookieSource {
    Chromium(BraveSession),
    State(BrowserStorageState),
}

/// Local browser configuration for normal agent sessions.
#[derive(Args)]
pub(crate) struct BrowserArgs {
    /// Select the private browser exposed to Code Mode as `tools.browser`.
    ///
    /// By default, Nanocodex prefers Brave, falls back to another installed
    /// Chromium-family browser, and disables browser tools when none is
    /// available. Pass `brave` to require the standard Brave installation,
    /// `chromium` to skip Brave, or `none` to disable browser tools.
    #[arg(
        long,
        env = "NANOCODEX_BROWSER",
        value_enum,
        num_args = 0..=1,
        default_missing_value = "brave",
        require_equals = true
    )]
    browser: Option<BrowserKind>,

    /// Copy cookies from a standard desktop browser profile into the private session.
    ///
    /// The default `all` copies every cookie from an automatically selected
    /// installed profile. Pass a browser name to select its profile or `none`
    /// to start with an empty cookie jar.
    #[arg(
        long,
        env = "NANOCODEX_BROWSER_COOKIES",
        value_enum,
        num_args = 0..=1,
        default_value = "all",
        default_missing_value = "all",
        require_equals = true
    )]
    cookies: Option<CookieSourceKind>,

    /// Chrome or Chromium executable used by the browser tool.
    #[arg(long, env = "NANOCODEX_BROWSER_EXECUTABLE", value_name = "PATH")]
    browser_executable: Option<PathBuf>,
}

impl Default for BrowserArgs {
    fn default() -> Self {
        Self {
            browser: None,
            cookies: Some(CookieSourceKind::All),
            browser_executable: None,
        }
    }
}

pub(crate) struct ConfiguredBrowser {
    browser: Browser,
}

impl BrowserArgs {
    pub(crate) fn disable(&mut self) {
        self.browser = Some(BrowserKind::None);
        self.cookies = Some(CookieSourceKind::None);
        self.browser_executable = None;
    }

    #[cfg(test)]
    pub(crate) const fn is_enabled(&self) -> bool {
        !matches!(self.browser, Some(BrowserKind::None))
    }

    #[cfg(test)]
    pub(crate) const fn copies_all_cookies(&self) -> bool {
        matches!(self.cookies, Some(CookieSourceKind::All))
    }

    #[cfg(test)]
    pub(crate) const fn uses_brave(&self) -> bool {
        matches!(self.browser, Some(BrowserKind::Brave))
    }

    pub(crate) fn configure(&self, workspace: &Path) -> Result<Option<ConfiguredBrowser>> {
        if self.browser == Some(BrowserKind::None) {
            if self.cookies.is_some_and(|source| {
                !matches!(source, CookieSourceKind::All | CookieSourceKind::None)
            }) {
                return Err(eyre!("--cookies requires an enabled browser"));
            }
            if self.browser_executable.is_some() {
                return Err(eyre!("--browser-executable requires an enabled browser"));
            }
            return Ok(None);
        }
        let Some(launch) = resolve_browser_launch(
            self.browser,
            self.browser_executable.as_deref(),
            BraveSession::standard_for,
        )?
        else {
            return Ok(None);
        };
        let virtual_authenticator = VirtualAuthenticator::platform_passkey()
            .credential_store(default_virtual_credential_store()?);
        let mut builder = Browser::builder()
            .file_root(workspace)
            .virtual_authenticator(virtual_authenticator);
        if let Some(executable) = launch.executable {
            builder = builder.executable(executable);
        }
        if let Some(source) = self
            .cookies
            .filter(|source| *source != CookieSourceKind::None)
        {
            builder = match cookie_source(source, launch.kind)? {
                CookieSource::Chromium(source) => builder.cookie_source(source.copy_all_cookies()),
                CookieSource::State(state) => builder.storage_state(state),
            };
        }
        let browser = builder
            .build()
            .wrap_err("failed to configure the browser tool")?;
        Ok(Some(ConfiguredBrowser { browser }))
    }
}

fn default_virtual_credential_store() -> Result<PathBuf> {
    let root = if let Some(root) = std::env::var_os("NANOCODEX_DIR") {
        PathBuf::from(root)
    } else {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or_else(|| eyre!("HOME is not set; set NANOCODEX_DIR explicitly"))?;
        PathBuf::from(home).join(".nanocodex")
    };
    if root.as_os_str().is_empty() {
        return Err(eyre!("NANOCODEX_DIR cannot be empty"));
    }
    Ok(root.join("browser/passkeys.json"))
}

struct BrowserLaunch {
    kind: BrowserKind,
    executable: Option<PathBuf>,
}

fn resolve_browser_launch(
    requested: Option<BrowserKind>,
    explicit_executable: Option<&Path>,
    mut standard_profile: impl FnMut(BrowserProfileKind) -> Result<BraveSession, BraveSessionError>,
) -> Result<Option<BrowserLaunch>> {
    if requested == Some(BrowserKind::None) {
        unreachable!("disabled browsers return before launch resolution");
    }
    if requested == Some(BrowserKind::Brave) && explicit_executable.is_some() {
        return Err(eyre!(
            "--browser-executable cannot be combined with --browser=brave"
        ));
    }
    if let Some(executable) = explicit_executable {
        return Ok(Some(BrowserLaunch {
            kind: BrowserKind::Chromium,
            executable: Some(executable.to_path_buf()),
        }));
    }
    match requested {
        None => Ok([
            BrowserProfileKind::Brave,
            BrowserProfileKind::Chrome,
            BrowserProfileKind::Chromium,
            BrowserProfileKind::Edge,
        ]
        .into_iter()
        .find_map(|profile| {
            standard_profile(profile).ok().map(|session| BrowserLaunch {
                kind: if profile == BrowserProfileKind::Brave {
                    BrowserKind::Brave
                } else {
                    BrowserKind::Chromium
                },
                executable: Some(session.executable().to_path_buf()),
            })
        })),
        Some(BrowserKind::Chromium) => Ok(Some(BrowserLaunch {
            kind: BrowserKind::Chromium,
            executable: None,
        })),
        Some(BrowserKind::Brave) => {
            let brave = standard_profile(BrowserProfileKind::Brave)
                .wrap_err("failed to locate the standard Brave profile")?;
            Ok(Some(BrowserLaunch {
                kind: BrowserKind::Brave,
                executable: Some(brave.executable().to_path_buf()),
            }))
        }
        Some(BrowserKind::None) => {
            unreachable!("disabled browsers return before launch resolution")
        }
    }
}

fn cookie_source(source: CookieSourceKind, target: BrowserKind) -> Result<CookieSource> {
    match source {
        CookieSourceKind::Firefox => {
            return FirefoxCookieSource::standard()
                .and_then(|source| source.load())
                .map(CookieSource::State)
                .wrap_err("failed to load the standard Firefox cookie profile");
        }
        CookieSourceKind::Safari => {
            return SafariCookieSource::standard()
                .and_then(|source| source.load())
                .map(CookieSource::State)
                .wrap_err("failed to load the standard Safari cookie profile");
        }
        _ => {}
    }
    let explicit = match source {
        CookieSourceKind::All => None,
        CookieSourceKind::Brave => Some(BrowserProfileKind::Brave),
        CookieSourceKind::Chrome => Some(BrowserProfileKind::Chrome),
        CookieSourceKind::Chromium => Some(BrowserProfileKind::Chromium),
        CookieSourceKind::Edge => Some(BrowserProfileKind::Edge),
        CookieSourceKind::Firefox | CookieSourceKind::Safari | CookieSourceKind::None => None,
    };
    if let Some(source) = explicit {
        return BraveSession::standard_for(source)
            .map(CookieSource::Chromium)
            .wrap_err_with(|| format!("failed to locate the standard {} profile", source.name()));
    }

    let preferences = match target {
        BrowserKind::Brave => [
            BrowserProfileKind::Brave,
            BrowserProfileKind::Chrome,
            BrowserProfileKind::Chromium,
            BrowserProfileKind::Edge,
        ],
        BrowserKind::Chromium => [
            BrowserProfileKind::Chrome,
            BrowserProfileKind::Chromium,
            BrowserProfileKind::Brave,
            BrowserProfileKind::Edge,
        ],
        BrowserKind::None => unreachable!("disabled browsers have no cookie source"),
    };
    preferences
        .into_iter()
        .find_map(|source| BraveSession::standard_for(source).ok())
        .map(CookieSource::Chromium)
        .or_else(|| {
            FirefoxCookieSource::standard()
                .and_then(|source| source.load())
                .ok()
                .map(CookieSource::State)
        })
        .or_else(|| {
            SafariCookieSource::standard()
                .and_then(|source| source.load())
                .ok()
                .map(CookieSource::State)
        })
        .ok_or_else(|| eyre!("failed to locate an installed browser cookie profile"))
}

impl ConfiguredBrowser {
    pub(crate) fn tool(&self) -> BrowserTool {
        BrowserTool::from_browser(self.browser.clone())
    }

    pub(crate) async fn shutdown(self) -> Result<()> {
        self.browser
            .close()
            .await
            .wrap_err("failed to shut down the browser tool")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use nanocodex::Tools;
    use nanocodex_browser::{BraveSession, BraveSessionError, BrowserProfileKind};
    use nanocodex_tools::runtime::ToolRuntime;

    use super::{BrowserArgs, BrowserKind, resolve_browser_launch};

    #[test]
    fn automatic_browser_falls_back_when_brave_is_not_installed() {
        let launch = resolve_browser_launch(None, None, |profile| {
            if profile == BrowserProfileKind::Chrome {
                return Ok(BraveSession::new("/installed/chrome", "/chrome/profile"));
            }
            Err(BraveSessionError::StandardInstallationUnavailable {
                browser: profile.name(),
            })
        })
        .unwrap()
        .unwrap();

        assert_eq!(launch.kind, BrowserKind::Chromium);
        assert_eq!(
            launch.executable.as_deref(),
            Some(Path::new("/installed/chrome"))
        );
    }

    #[test]
    fn automatic_browser_is_disabled_when_none_is_installed() {
        let launch = resolve_browser_launch(None, None, |profile| {
            Err(BraveSessionError::StandardInstallationUnavailable {
                browser: profile.name(),
            })
        })
        .unwrap();

        assert!(launch.is_none());
    }

    #[test]
    fn automatic_browser_prefers_an_installed_brave() {
        let launch = resolve_browser_launch(None, None, |_| {
            Ok(BraveSession::new("/installed/brave", "/brave/profile"))
        })
        .unwrap()
        .unwrap();

        assert_eq!(launch.kind, BrowserKind::Brave);
        assert_eq!(
            launch.executable.as_deref(),
            Some(Path::new("/installed/brave"))
        );
    }

    #[test]
    fn explicit_brave_still_requires_the_standard_installation() {
        let error = resolve_browser_launch(Some(BrowserKind::Brave), None, |_| {
            Err(BraveSessionError::ExecutableUnavailable {
                path: "/missing/brave".into(),
            })
        })
        .err()
        .unwrap();

        assert_eq!(
            error.to_string(),
            "failed to locate the standard Brave profile"
        );
    }

    #[tokio::test]
    async fn configured_browser_adds_no_model_facing_schema() {
        let workspace = tempfile::tempdir().unwrap();
        let baseline_tools = Tools::builder().build().unwrap();
        let baseline = ToolRuntime::new_with_tools(workspace.path(), None, None, &baseline_tools)
            .model_specs("browser-tui-test");
        let browser = BrowserArgs {
            browser: Some(super::BrowserKind::Chromium),
            cookies: None,
            browser_executable: None,
        }
        .configure(workspace.path())
        .unwrap()
        .unwrap();
        let tools = Tools::builder().provider(browser.tool()).build().unwrap();
        let runtime = ToolRuntime::new_with_tools(workspace.path(), None, None, &tools);
        let definitions = runtime.model_specs("browser-tui-test");
        let serialized = serde_json::to_string(&definitions).unwrap();

        let baseline_bytes = serde_json::to_vec(&baseline).unwrap();
        let definition_bytes = serde_json::to_vec(&definitions).unwrap();
        assert_ne!(definition_bytes, baseline_bytes);
        assert!(serialized.contains("tools.browser"));
        assert!(serialized.contains("host-managed browser session"));
        assert!(!serialized.contains("detect_gate"));
        assert!(definition_bytes.len() - baseline_bytes.len() < 512);
        assert!(runtime.contains("browser"));
        browser.shutdown().await.unwrap();
    }

    #[test]
    fn disabled_browser_rejects_nondefault_browser_configuration() {
        let workspace = tempfile::tempdir().unwrap();
        let disabled = BrowserArgs {
            browser: Some(super::BrowserKind::None),
            cookies: Some(super::CookieSourceKind::None),
            browser_executable: None,
        }
        .configure(workspace.path())
        .unwrap();
        assert!(disabled.is_none());

        let cookies = BrowserArgs {
            browser: Some(super::BrowserKind::None),
            cookies: Some(super::CookieSourceKind::Chrome),
            browser_executable: None,
        }
        .configure(workspace.path())
        .err()
        .unwrap();
        assert_eq!(cookies.to_string(), "--cookies requires an enabled browser");

        let executable = BrowserArgs {
            browser: Some(super::BrowserKind::None),
            cookies: Some(super::CookieSourceKind::All),
            browser_executable: Some("/tmp/chromium".into()),
        }
        .configure(workspace.path())
        .err()
        .unwrap();
        assert_eq!(
            executable.to_string(),
            "--browser-executable requires an enabled browser"
        );
    }
}
