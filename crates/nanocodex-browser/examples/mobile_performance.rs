use std::path::PathBuf;

use eyre::{Result, WrapErr, eyre};
use nanocodex_browser::{
    Browser, BrowserAction, BrowserActionResult, BrowserDevicePreset, BrowserNetworkRequest,
    BrowserOrientation, BrowserTarget,
};

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let executable = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("usage: mobile_performance <chrome> <url>..."))?;
    let urls = arguments
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Err(eyre!("mobile_performance requires at least one URL"));
    }

    let mut failed = false;
    for url in urls {
        // One fresh browser profile per URL makes every route measurement cold.
        let browser = Browser::with_executable(executable.clone())?;
        browser
            .execute(BrowserAction::SetDevice {
                device: BrowserDevicePreset::Pixel8,
                orientation: BrowserOrientation::Portrait,
            })
            .await?;
        browser
            .execute(BrowserAction::PerformanceTraceStart)
            .await?;
        browser
            .execute(BrowserAction::Open { url: url.clone() })
            .await
            .wrap_err_with(|| format!("open {url}"))?;
        browser
            .execute(BrowserAction::WaitForSelector {
                target: BrowserTarget::css("#root > *"),
                state: None,
            })
            .await
            .wrap_err_with(|| format!("wait for rendered app at {url}"))?;
        browser
            .execute(BrowserAction::WaitForTimeout {
                milliseconds: 1_000,
            })
            .await?;

        let title = browser.execute(BrowserAction::GetTitle).await?;
        let vitals = browser.execute(BrowserAction::WebVitals).await?;
        let trace = browser.execute(BrowserAction::PerformanceTraceStop).await?;
        let network = browser
            .execute(BrowserAction::NetworkRequests {
                filter: None,
                after: None,
                limit: Some(1_000),
            })
            .await?;
        let errors = browser
            .execute(BrowserAction::Errors { limit: Some(100) })
            .await?;
        if let BrowserActionResult::Errors { total, .. } = &errors {
            failed |= *total != 0;
        }

        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "url": url,
                "title": title,
                "vitals": vitals,
                "trace": trace,
                "network": summarize_network(&network),
                "errors": errors,
            }))?
        );
        browser.close().await?;
    }
    if failed {
        return Err(eyre!("one or more routes emitted page errors"));
    }
    Ok(())
}

fn summarize_network(result: &BrowserActionResult) -> serde_json::Value {
    let BrowserActionResult::NetworkRequests {
        requests,
        total,
        dropped,
        has_more,
        ..
    } = result
    else {
        return serde_json::json!({ "error": "missing network result" });
    };
    let mut largest = requests.iter().collect::<Vec<_>>();
    largest.sort_by_key(|request| std::cmp::Reverse(request.encoded_data_length.unwrap_or(0)));
    serde_json::json!({
        "total": total,
        "dropped": dropped,
        "hasMore": has_more,
        "failed": requests.iter().filter(|request| request.failure.is_some()).count(),
        "cached": requests.iter().filter(|request| request.from_disk_cache || request.from_service_worker).count(),
        "encodedBytes": requests.iter().filter_map(|request| request.encoded_data_length).sum::<u64>(),
        "largest": largest.into_iter().take(10).map(network_row).collect::<Vec<_>>(),
    })
}

fn network_row(request: &BrowserNetworkRequest) -> serde_json::Value {
    serde_json::json!({
        "url": request.url,
        "type": request.resource_type,
        "status": request.status,
        "durationMs": request.duration_ms,
        "encodedBytes": request.encoded_data_length,
        "cached": request.from_disk_cache || request.from_service_worker,
        "failure": request.failure,
    })
}
