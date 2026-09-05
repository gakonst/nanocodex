use std::path::PathBuf;

use eyre::{Result, WrapErr, eyre};
use nanocodex_browser::{
    Browser, BrowserAction, BrowserActionResult, BrowserDevicePreset, BrowserOrientation,
    BrowserTarget,
};

const INPUT_SELECTOR: &str = "textarea[aria-label='Message Nanocodex']";
const PROMPT: &str = "Reply with exactly MOBILE_OK. Do not call tools.";

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let executable = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("usage: mobile_terminal_smoke <chrome> <screenshot> <url>"))?;
    let screenshot = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("usage: mobile_terminal_smoke <chrome> <screenshot> <url>"))?;
    let url = arguments
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| eyre!("usage: mobile_terminal_smoke <chrome> <screenshot> <url>"))?;

    let browser = Browser::with_executable(executable)?;
    browser
        .execute(BrowserAction::SetDevice {
            device: BrowserDevicePreset::Iphone15Pro,
            orientation: BrowserOrientation::Portrait,
        })
        .await?;
    browser.execute(BrowserAction::Open { url }).await?;
    browser
        .execute(BrowserAction::WaitForFunction {
            expression: format!(
                "(() => {{ const input = document.querySelector({INPUT_SELECTOR:?}); return input instanceof HTMLTextAreaElement && !input.disabled; }})()"
            ),
        })
        .await
        .wrap_err("wait for an enabled mobile terminal composer")?;

    let input = BrowserTarget::css(INPUT_SELECTOR);
    browser
        .execute(BrowserAction::Fill {
            target: input.clone(),
            text: PROMPT.to_owned(),
        })
        .await
        .wrap_err("type into the mobile terminal composer")?;
    let value = browser
        .execute(BrowserAction::GetValue {
            target: input.clone(),
        })
        .await?;
    if !matches!(
        value,
        BrowserActionResult::Value {
            value: Some(ref value),
            ..
        } if value == PROMPT
    ) {
        return Err(eyre!("mobile composer did not retain the typed prompt"));
    }

    browser
        .execute(BrowserAction::Press {
            target: input,
            key: "Enter".to_owned(),
            modifiers: Vec::new(),
        })
        .await
        .wrap_err("submit the mobile terminal prompt")?;
    browser
        .execute(BrowserAction::WaitForFunction {
            expression: format!(
                "(() => {{ const input = document.querySelector({INPUT_SELECTOR:?}); const submit = document.querySelector('.agent-touch-actions button[type=submit]'); return input instanceof HTMLTextAreaElement && (input.value === '' || submit?.textContent?.trim() === 'Queued'); }})()"
            ),
        })
        .await
        .wrap_err("wait for the mobile prompt to send or queue for authentication")?;

    let final_value = browser
        .execute(BrowserAction::GetValue {
            target: BrowserTarget::css(INPUT_SELECTOR),
        })
        .await?;
    let submit_label = browser
        .execute(BrowserAction::GetText {
            target: BrowserTarget::css(".agent-touch-actions button[type=submit]"),
        })
        .await?;

    let errors = browser
        .execute(BrowserAction::Errors { limit: Some(100) })
        .await?;
    if let BrowserActionResult::Errors { total, .. } = &errors
        && *total != 0
    {
        return Err(eyre!("mobile terminal emitted {total} page errors"));
    }
    let captured = browser
        .execute(BrowserAction::Screenshot {
            full_page: true,
            annotate: false,
            target: None,
        })
        .await?;
    let BrowserActionResult::Screenshot { path, .. } = captured else {
        return Err(eyre!("browser did not return a terminal screenshot"));
    };
    tokio::fs::copy(path, &screenshot)
        .await
        .wrap_err_with(|| format!("copy screenshot to {}", screenshot.display()))?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "errors": errors,
            "finalValue": final_value,
            "submitLabel": submit_label,
        }))?
    );
    browser.close().await?;
    Ok(())
}
