use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr, eyre};
use nanocodex_browser::{
    Browser, BrowserAction, BrowserActionResult, BrowserDevicePreset, BrowserOrientation,
    BrowserTarget,
};

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let executable = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("usage: mobile_audit <chrome> <output-dir> <url>..."))?;
    let output_dir = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("usage: mobile_audit <chrome> <output-dir> <url>..."))?;
    let urls = arguments
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Err(eyre!("mobile_audit requires at least one URL"));
    }
    tokio::fs::create_dir_all(&output_dir)
        .await
        .wrap_err("create audit output directory")?;

    let browser = Browser::with_executable(executable)?;
    let mut failed = false;
    for (index, url) in urls.iter().enumerate() {
        browser
            .execute(BrowserAction::SetDevice {
                device: BrowserDevicePreset::IphoneSe,
                orientation: BrowserOrientation::Portrait,
            })
            .await
            .wrap_err_with(|| format!("configure mobile browser for {url}"))?;
        browser
            .execute(BrowserAction::Open { url: url.clone() })
            .await
            .wrap_err_with(|| format!("open {url}"))?;

        let mobile = browser
            .execute(BrowserAction::MobileAudit {
                devices: vec![
                    BrowserDevicePreset::IphoneSe,
                    BrowserDevicePreset::Iphone15Pro,
                    BrowserDevicePreset::Pixel8,
                    BrowserDevicePreset::GalaxyS24,
                ],
                // End in portrait so follow-up screenshots remain phone-shaped.
                orientations: vec![BrowserOrientation::Landscape, BrowserOrientation::Portrait],
                ready: Some(BrowserTarget::css("#root > *")),
            })
            .await
            .wrap_err_with(|| format!("mobile audit {url}"))?;
        if let BrowserActionResult::MobileAudit { audit, .. } = &mobile {
            failed |= !audit.passed;
        }
        let accessibility = browser.execute(BrowserAction::AccessibilityAudit).await?;
        if let BrowserActionResult::Accessibility { audit, .. } = &accessibility {
            failed |= !audit.violations.is_empty();
        }
        let errors = browser
            .execute(BrowserAction::Errors { limit: Some(100) })
            .await?;
        if let BrowserActionResult::Errors { total, .. } = &errors {
            failed |= *total != 0;
        }
        let console = browser
            .execute(BrowserAction::Console { limit: Some(100) })
            .await?;
        let title = browser.execute(BrowserAction::GetTitle).await?;
        let vitals = browser.execute(BrowserAction::WebVitals).await?;
        let screenshot = browser
            .execute(BrowserAction::Screenshot {
                full_page: true,
                annotate: false,
                target: None,
            })
            .await?;
        let screenshot_path = copy_screenshot(&screenshot, &output_dir, index).await?;

        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "url": url,
                "mobile": mobile,
                "accessibility": accessibility,
                "errors": errors,
                "console": console,
                "title": title,
                "vitals": vitals,
                "screenshot": screenshot_path,
            }))?
        );
    }
    browser.close().await?;
    if failed {
        return Err(eyre!(
            "one or more routes had mobile, accessibility, or page errors"
        ));
    }
    Ok(())
}

async fn copy_screenshot(
    result: &BrowserActionResult,
    output_dir: &Path,
    index: usize,
) -> Result<PathBuf> {
    let BrowserActionResult::Screenshot { path, .. } = result else {
        return Err(eyre!("browser did not return a screenshot"));
    };
    let destination = output_dir.join(format!("route-{index}.png"));
    tokio::fs::copy(path, &destination)
        .await
        .wrap_err_with(|| format!("copy screenshot to {}", destination.display()))?;
    Ok(destination)
}
