use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use eyre::{Result, WrapErr, eyre};
use nanocodex_browser::{
    BraveSession, Browser, BrowserProfileKind, BrowserStorageState, BrowserTool,
    FirefoxCookieSource, SafariCookieSource,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum BrowserKind {
    #[value(alias = "true")]
    Chromium,
    Brave,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CookieSourceKind {
    #[value(alias = "true")]
    Auto,
    Brave,
    Chrome,
    Chromium,
    Edge,
    Firefox,
    Safari,
}

enum CookieSource {
    Chromium(BraveSession),
    State(BrowserStorageState),
}

/// Opt-in local browser configuration for normal agent sessions.
#[derive(Args, Clone, Default)]
pub(crate) struct BrowserArgs {
    /// Expose one private browser session to Code Mode as `tools.browser`.
    ///
    /// Pass `brave` to use the standard Brave installation. A bare `--browser`
    /// preserves the private Chromium default.
    #[arg(
        long,
        env = "NANOCODEX_BROWSER",
        value_enum,
        num_args = 0..=1,
        default_missing_value = "chromium",
        require_equals = true
    )]
    browser: Option<BrowserKind>,

    /// Copy cookies from a standard desktop browser profile.
    ///
    /// Pass a browser name to select the source profile. A bare flag or `true`
    /// automatically selects an installed profile.
    #[arg(
        long,
        env = "NANOCODEX_BROWSER_COOKIES",
        value_enum,
        num_args = 0..=1,
        default_missing_value = "auto",
        require_equals = true,
        requires = "browser"
    )]
    cookies: Option<CookieSourceKind>,

    /// Chrome or Chromium executable used by the browser tool.
    #[arg(
        long,
        env = "NANOCODEX_BROWSER_EXECUTABLE",
        value_name = "PATH",
        requires = "browser"
    )]
    browser_executable: Option<PathBuf>,
}

pub(crate) struct ConfiguredBrowser {
    browser: Browser,
}

impl BrowserArgs {
    #[cfg(test)]
    pub(crate) const fn is_enabled(&self) -> bool {
        self.browser.is_some()
    }

    pub(crate) fn configure(&self, workspace: &Path) -> Result<Option<ConfiguredBrowser>> {
        let Some(kind) = self.browser else {
            return Ok(None);
        };
        let mut builder = Browser::builder().file_root(workspace);
        match kind {
            BrowserKind::Chromium => {
                if let Some(executable) = &self.browser_executable {
                    builder = builder.executable(executable);
                }
            }
            BrowserKind::Brave => {
                if self.browser_executable.is_some() {
                    return Err(eyre!(
                        "--browser-executable cannot be combined with --browser=brave"
                    ));
                }
                let brave = BraveSession::standard()
                    .wrap_err("failed to locate the standard Brave profile")?;
                builder = builder.executable(brave.executable().to_path_buf());
            }
        }
        if let Some(source) = self.cookies {
            builder = match cookie_source(source, kind)? {
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
        CookieSourceKind::Auto => None,
        CookieSourceKind::Brave => Some(BrowserProfileKind::Brave),
        CookieSourceKind::Chrome => Some(BrowserProfileKind::Chrome),
        CookieSourceKind::Chromium => Some(BrowserProfileKind::Chromium),
        CookieSourceKind::Edge => Some(BrowserProfileKind::Edge),
        CookieSourceKind::Firefox | CookieSourceKind::Safari => None,
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
    use nanocodex::Tools;
    use nanocodex_tools::runtime::ToolRuntime;

    use super::BrowserArgs;

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

        assert_eq!(
            serde_json::to_vec(&definitions).unwrap(),
            serde_json::to_vec(&baseline).unwrap(),
            "enabling browser must not add any model-input schema bytes"
        );
        assert!(!serialized.contains("browser"));
        assert!(runtime.contains("browser"));
        browser.shutdown().await.unwrap();
    }
}
