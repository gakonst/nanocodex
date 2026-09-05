use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::{Args, ValueEnum};
use eyre::{Result, WrapErr, eyre};
use nanocodex_browser::{
    BraveSession, BraveSessionError, Browser, BrowserCookieAuthorization, BrowserProfileKind,
    BrowserStorageState, BrowserTool, FirefoxCookieSource, HostPasskeyAuthenticator,
    SafariCookieSource, VirtualAuthenticator,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum BrowserKind {
    Chromium,
    #[value(alias = "true")]
    Brave,
    #[value(alias = "false", alias = "off")]
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CookieAuthorizationKind {
    #[default]
    Background,
    Interactive,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum PasskeyKind {
    #[default]
    Virtual,
    Host,
    #[value(alias = "false", alias = "off")]
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum BrowserProfilePersistence {
    Temporary,
    #[default]
    Persistent,
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
    /// By default, Nanocodex uses a dedicated automation browser on macOS. On
    /// other platforms it prefers Brave and falls back to another installed
    /// Chromium-family browser. Pass `brave` to deliberately use the standard
    /// Brave application with a private profile, `chromium` to use normal
    /// private browser discovery, or `none` to disable browser tools. Bare
    /// `--browser` selects private browser discovery.
    #[arg(
        long,
        env = "NANOCODEX_BROWSER",
        value_enum,
        num_args = 0..=1,
        default_missing_value = "chromium",
        require_equals = true
    )]
    browser: Option<BrowserKind>,

    /// Permit a visible temporary browser when macOS must authorize cookie decryption.
    ///
    /// `interactive` retries a failed background import with a copied temporary
    /// profile. It applies only to `--browser-profile=temporary` and never opens
    /// the ordinary source profile or its tabs.
    #[arg(
        long = "cookie-auth",
        env = "NANOCODEX_BROWSER_COOKIE_AUTH",
        value_enum
    )]
    #[cfg_attr(target_os = "macos", arg(default_value = "interactive"))]
    #[cfg_attr(not(target_os = "macos"), arg(default_value = "background"))]
    cookie_authorization: CookieAuthorizationKind,

    /// Select passkeys owned by Nanocodex or interactively use the macOS host authenticator.
    ///
    /// `virtual` persists automation-only passkeys in the Nanocodex state
    /// directory. `host` exposes explicit start/resume browser actions that use
    /// a temporary visible desktop browser and the normal Touch ID/iPhone UI.
    #[arg(
        long,
        env = "NANOCODEX_BROWSER_PASSKEYS",
        value_enum,
        default_value = "virtual"
    )]
    passkeys: PasskeyKind,

    /// Chrome or Chromium executable used by the browser tool.
    #[arg(long, env = "NANOCODEX_BROWSER_EXECUTABLE", value_name = "PATH")]
    browser_executable: Option<PathBuf>,

    /// Keep the dedicated automation profile across Nanocodex runs.
    ///
    /// `persistent` stores cookies and site data under the Nanocodex state
    /// directory. It never opens or mutates an ordinary desktop-browser
    /// profile. Only one browser tool may actively use the persistent profile
    /// at a time. Use `temporary` for a fresh profile that imports host-browser
    /// cookies and forgets changes at shutdown.
    #[arg(
        long = "browser-profile",
        env = "NANOCODEX_BROWSER_PROFILE",
        value_enum,
        default_value = "persistent"
    )]
    browser_profile: BrowserProfilePersistence,
}

impl Default for BrowserArgs {
    fn default() -> Self {
        Self {
            browser: None,
            cookie_authorization: default_cookie_authorization(),
            passkeys: PasskeyKind::Virtual,
            browser_executable: None,
            browser_profile: BrowserProfilePersistence::Persistent,
        }
    }
}

pub(crate) struct ConfiguredBrowser {
    browser: Browser,
}

impl BrowserArgs {
    pub(crate) fn disable(&mut self) {
        self.browser = Some(BrowserKind::None);
        self.cookie_authorization = CookieAuthorizationKind::Background;
        self.passkeys = PasskeyKind::None;
        self.browser_executable = None;
        self.browser_profile = BrowserProfilePersistence::Temporary;
    }

    #[cfg(test)]
    pub(crate) const fn is_enabled(&self) -> bool {
        !matches!(self.browser, Some(BrowserKind::None))
    }

    #[cfg(test)]
    pub(crate) const fn copies_all_cookies(&self) -> bool {
        !matches!(self.browser, Some(BrowserKind::None))
            && matches!(self.browser_profile, BrowserProfilePersistence::Temporary)
    }

    #[cfg(test)]
    pub(crate) const fn uses_brave(&self) -> bool {
        matches!(self.browser, Some(BrowserKind::Brave))
    }

    #[cfg(test)]
    pub(crate) const fn uses_interactive_cookie_authorization(&self) -> bool {
        matches!(
            self.cookie_authorization,
            CookieAuthorizationKind::Interactive
        )
    }

    #[cfg(test)]
    pub(crate) const fn uses_host_passkeys(&self) -> bool {
        matches!(self.passkeys, PasskeyKind::Host)
    }

    #[cfg(test)]
    pub(crate) const fn uses_persistent_profile(&self) -> bool {
        matches!(self.browser_profile, BrowserProfilePersistence::Persistent)
    }

    pub(crate) fn configure(&self, workspace: &Path) -> Result<Option<ConfiguredBrowser>> {
        if self.browser == Some(BrowserKind::None) {
            if self.browser_executable.is_some() {
                return Err(eyre!("--browser-executable requires an enabled browser"));
            }
            if self.passkeys == PasskeyKind::Host {
                return Err(eyre!("--passkeys=host requires an enabled browser"));
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
        let persistent_profile =
            match self.browser_profile {
                BrowserProfilePersistence::Temporary => None,
                BrowserProfilePersistence::Persistent => {
                    let profile = default_persistent_browser_profile()?;
                    create_private_directory(profile.parent().ok_or_else(|| {
                        eyre!("persistent browser profile has no parent directory")
                    })?)?;
                    create_private_directory(&profile)?;
                    Some(profile)
                }
            };
        let mut builder = Browser::builder().file_root(workspace);
        if let Some(profile) = persistent_profile {
            builder = builder.persistent_profile(profile);
        }
        builder = match self.passkeys {
            PasskeyKind::Virtual => builder.virtual_authenticator(
                VirtualAuthenticator::platform_passkey()
                    .credential_store(default_virtual_credential_store()?),
            ),
            PasskeyKind::Host => {
                builder.host_passkey_authenticator(standard_host_passkey_authenticator()?)
            }
            PasskeyKind::None => builder,
        };
        if let Some(executable) = launch.executable {
            builder = builder.executable(executable);
        }
        // A persistent automation profile is its own browser identity. Host
        // cookie import remains the bootstrap policy for temporary profiles;
        // mixing it into a durable profile could overwrite a login created by
        // an earlier Nanocodex run.
        if self.browser_profile == BrowserProfilePersistence::Temporary
            && let Some(source) = cookie_source()
        {
            builder = match source {
                CookieSource::Chromium(source) => {
                    builder.cookie_source(source.copy_all_cookies().cookie_authorization(
                        match self.cookie_authorization {
                            CookieAuthorizationKind::Background => {
                                BrowserCookieAuthorization::Background
                            }
                            CookieAuthorizationKind::Interactive => {
                                BrowserCookieAuthorization::Interactive
                            }
                        },
                    ))
                }
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
    Ok(default_browser_state_root()?.join("passkeys.json"))
}

fn default_persistent_browser_profile() -> Result<PathBuf> {
    Ok(default_browser_state_root()?.join("profile"))
}

fn default_browser_state_root() -> Result<PathBuf> {
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
    Ok(root.join("browser"))
}

fn create_private_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(eyre!(
            "persistent browser profile directory must not be a symbolic link: {}",
            path.display()
        ));
    }
    fs::create_dir_all(path).wrap_err_with(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .wrap_err_with(|| format!("failed to protect {}", path.display()))?;
    }
    Ok(())
}

fn standard_host_passkey_authenticator() -> Result<HostPasskeyAuthenticator> {
    #[cfg(not(target_os = "macos"))]
    {
        Err(eyre!(
            "--passkeys=host is currently supported only on macOS"
        ))
    }

    #[cfg(target_os = "macos")]
    {
        for kind in [
            BrowserProfileKind::Brave,
            BrowserProfileKind::Chrome,
            BrowserProfileKind::Edge,
            BrowserProfileKind::Chromium,
        ] {
            if let Ok(session) = BraveSession::standard_for(kind) {
                return Ok(HostPasskeyAuthenticator::new(session.executable()));
            }
        }
        Err(eyre!(
            "--passkeys=host requires an installed Brave, Chrome, Edge, or Chromium application"
        ))
    }
}

struct BrowserLaunch {
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
            executable: Some(executable.to_path_buf()),
        }));
    }
    match requested {
        None => {
            #[cfg(target_os = "macos")]
            {
                Ok(Some(BrowserLaunch { executable: None }))
            }
            #[cfg(not(target_os = "macos"))]
            {
                Ok([
                    BrowserProfileKind::Brave,
                    BrowserProfileKind::Chrome,
                    BrowserProfileKind::Chromium,
                    BrowserProfileKind::Edge,
                ]
                .into_iter()
                .find_map(|profile| {
                    standard_profile(profile).ok().map(|session| BrowserLaunch {
                        executable: Some(session.executable().to_path_buf()),
                    })
                }))
            }
        }
        Some(BrowserKind::Chromium) => Ok(Some(BrowserLaunch { executable: None })),
        Some(BrowserKind::Brave) => {
            let brave = standard_profile(BrowserProfileKind::Brave)
                .wrap_err("failed to locate the standard Brave profile")?;
            Ok(Some(BrowserLaunch {
                executable: Some(brave.executable().to_path_buf()),
            }))
        }
        Some(BrowserKind::None) => {
            unreachable!("disabled browsers return before launch resolution")
        }
    }
}

#[cfg(target_os = "macos")]
const fn default_cookie_authorization() -> CookieAuthorizationKind {
    CookieAuthorizationKind::Interactive
}

#[cfg(not(target_os = "macos"))]
const fn default_cookie_authorization() -> CookieAuthorizationKind {
    CookieAuthorizationKind::Background
}

fn cookie_source() -> Option<CookieSource> {
    // Cookie selection is independent from the disposable automation binary.
    // Prefer Chromium-family profiles in the same order as `nanocodex cookies`,
    // followed by non-Chromium fallbacks.
    chromium_cookie_source(BraveSession::standard_cookie_for)
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
}

fn chromium_cookie_source(
    mut standard_cookie: impl FnMut(BrowserProfileKind) -> Result<BraveSession, BraveSessionError>,
) -> Option<BraveSession> {
    chromium_cookie_source_preferences()
        .into_iter()
        .find_map(|source| standard_cookie(source).ok())
}

const fn chromium_cookie_source_preferences() -> [BrowserProfileKind; 4] {
    [
        BrowserProfileKind::Brave,
        BrowserProfileKind::Chrome,
        BrowserProfileKind::Chromium,
        BrowserProfileKind::Edge,
    ]
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
    use nanocodex_browser::BraveSession;
    use nanocodex_browser::{BraveSessionError, BrowserProfileKind};
    use nanocodex_tools::runtime::ToolRuntime;

    use super::{BrowserArgs, BrowserKind, chromium_cookie_source, resolve_browser_launch};

    #[test]
    fn automatic_cookie_source_prefers_the_host_brave_profile() {
        let mut visited = Vec::new();
        let source = chromium_cookie_source(|profile| {
            visited.push(profile);
            Ok(BraveSession::new("/installed/brave", "/brave/profile"))
        })
        .unwrap();

        assert_eq!(source.executable(), Path::new("/installed/brave"));
        assert_eq!(visited, [BrowserProfileKind::Brave]);
    }

    #[test]
    fn automatic_cookie_source_falls_back_from_brave_to_chrome() {
        let mut visited = Vec::new();
        let source = chromium_cookie_source(|profile| {
            visited.push(profile);
            if profile == BrowserProfileKind::Chrome {
                return Ok(BraveSession::new("/installed/chrome", "/chrome/profile"));
            }
            Err(BraveSessionError::StandardInstallationUnavailable {
                browser: profile.name(),
            })
        })
        .unwrap();

        assert_eq!(source.executable(), Path::new("/installed/chrome"));
        assert_eq!(
            visited,
            [BrowserProfileKind::Brave, BrowserProfileKind::Chrome]
        );
    }

    #[cfg(not(target_os = "macos"))]
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

        assert_eq!(
            launch.executable.as_deref(),
            Some(Path::new("/installed/chrome"))
        );
    }

    #[cfg(not(target_os = "macos"))]
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

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn automatic_browser_prefers_an_installed_brave() {
        let launch = resolve_browser_launch(None, None, |_| {
            Ok(BraveSession::new("/installed/brave", "/brave/profile"))
        })
        .unwrap()
        .unwrap();

        assert_eq!(
            launch.executable.as_deref(),
            Some(Path::new("/installed/brave"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn automatic_browser_leaves_private_runtime_selection_to_the_library() {
        let launch = resolve_browser_launch(None, None, |_| {
            panic!("automatic macOS browser selection must not inspect desktop profiles")
        })
        .unwrap()
        .unwrap();

        assert!(launch.executable.is_none());
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
            cookie_authorization: super::CookieAuthorizationKind::Background,
            passkeys: super::PasskeyKind::Virtual,
            browser_executable: None,
            browser_profile: super::BrowserProfilePersistence::Temporary,
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
            cookie_authorization: super::CookieAuthorizationKind::Interactive,
            passkeys: super::PasskeyKind::None,
            browser_executable: None,
            browser_profile: super::BrowserProfilePersistence::Temporary,
        }
        .configure(workspace.path())
        .unwrap();
        assert!(disabled.is_none());

        let executable = BrowserArgs {
            browser: Some(super::BrowserKind::None),
            cookie_authorization: super::CookieAuthorizationKind::Background,
            passkeys: super::PasskeyKind::None,
            browser_executable: Some("/tmp/chromium".into()),
            browser_profile: super::BrowserProfilePersistence::Temporary,
        }
        .configure(workspace.path())
        .err()
        .unwrap();
        assert_eq!(
            executable.to_string(),
            "--browser-executable requires an enabled browser"
        );
    }

    #[test]
    fn persistent_browser_profile_directory_is_private() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("browser/profile");
        super::create_private_directory(profile.parent().unwrap()).unwrap();
        super::create_private_directory(&profile).unwrap();

        assert!(profile.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(profile.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(profile).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
