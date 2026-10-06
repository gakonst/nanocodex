//! A bounded, read-only projection for the standalone menu. Never serialize API
//! responses, credential selections, paths, leases, or transport errors directly.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eyre::Result;
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const BODY_LIMIT: usize = 1024 * 1024;
const HAND_LIMIT: usize = 2048;

#[derive(Clone, Copy)]
struct Failure {
    state: &'static str,
    message: &'static str,
}

const UNKNOWN: Failure = Failure {
    state: "unknown",
    message: "Status response is unavailable or invalid.",
};
const NETWORK: Failure = Failure {
    state: "network_error",
    message: "Cannot reach Nanocodex; check your connection.",
};

pub(crate) async fn run() -> Result<()> {
    let (local, (account, inventory)) = tokio::join!(local_status(), account_status());
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    println!(
        "{}",
        json!({"schema_version": 1, "observed_at": observed_at,
        "local": local, "account": account, "inventory": inventory})
    );
    Ok(())
}

async fn local_status() -> Value {
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let state = crate::hand_service::status().await?;
        let pending = crate::hand_service::is_pending().await?;
        Ok::<_, eyre::Report>(json!({"installed": state.installed, "loaded": state.loaded,
            "pid": state.pid, "pending_login": pending, "error": null}))
    })
    .await;
    match result {
        Ok(Ok(value)) => value,
        _ => json!({"installed": null, "loaded": null, "pid": null,
            "pending_login": null, "error": "Cannot determine the local Hand service state."}),
    }
}

fn failed_account(failure: Failure) -> (Value, Value) {
    (
        json!({"state": failure.state, "display_name": null, "error": failure.message}),
        failed_inventory(failure),
    )
}

fn failed_inventory(failure: Failure) -> Value {
    json!({"state": failure.state, "hands": [], "error": failure.message,
        "coverage": "currently_advertised", "probe_performed": false})
}

async fn account_status() -> (Value, Value) {
    let (origin, key) = match nanocodex_cli_auth::optional_enrollment_credentials(None) {
        Ok(Some(selected)) => selected,
        Ok(None) => {
            return (
                json!({"state": "signed_out", "display_name": null, "error": null}),
                json!({"state": "signed_out", "hands": [], "error": null,
                "coverage": "currently_advertised", "probe_performed": false}),
            );
        }
        Err(_) => {
            return failed_account(Failure {
                state: "unknown",
                message: "Cannot read the selected account login.",
            });
        }
    };
    let mut authorization = match HeaderValue::from_str(&format!("Bearer {}", key.as_str())) {
        Ok(value) => value,
        Err(_) => return failed_account(UNKNOWN),
    };
    authorization.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(AUTHORIZATION, authorization);
    let client = match Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .timeout(REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return failed_account(UNKNOWN),
    };
    // /v1/me is the public account identity endpoint (also used by account
    // login/status). There is no /v1/account identity route on this server.
    let identity = match get(&client, &origin, "/v1/me").await {
        Ok(value) => value,
        Err(failure) => return failed_account(failure),
    };
    let user = identity["user"]["id"].as_str().and_then(safe_text);
    if identity["authentication"] != "api_key"
        || user.is_none()
        || identity["organization"]["id"]
            .as_str()
            .and_then(safe_text)
            .is_none()
        || identity["team"]["id"]
            .as_str()
            .and_then(safe_text)
            .is_none()
        || !matches!(
            identity["role"].as_str(),
            Some("owner" | "writer" | "reader")
        )
    {
        return failed_account(UNKNOWN);
    }
    let display_name = identity["user"]["name"]
        .as_str()
        .and_then(safe_text)
        .or_else(|| identity["user"]["email"].as_str().and_then(safe_text));
    let mut account = json!({"state": "verified", "display_name": display_name, "error": null});
    let (machines, screens) = tokio::join!(
        get(&client, &origin, "/v1/account/hands"),
        get(&client, &origin, "/v1/account/hands/screens")
    );
    let mut hands = Vec::new();
    let mut failure = None;
    match machines {
        Ok(value) => match project_machines(&value) {
            Ok(found) => hands = found,
            Err(error) => failure = Some(error),
        },
        Err(error) => failure = Some(error),
    }
    match screens {
        Ok(value) => {
            if let Err(error) = project_screens(&value, &mut hands) {
                retain_failure(&mut failure, error);
            }
        }
        Err(error) => retain_failure(&mut failure, error),
    }

    if failure.is_some_and(|failure| failure.state == "expired") {
        account = json!({"state": "expired", "display_name": null,
            "error": "Your account sign-in expired; sign in again."});
        hands.clear();
    }
    hands.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let inventory = json!({"state": failure.map_or("ready", |error| error.state),
        "hands": hands, "error": failure.map(|error| error.message),
        "coverage": "currently_advertised", "probe_performed": false});
    (account, inventory)
}

// Both discovery requests run concurrently. Keep authorization rejection even
// when the other request fails differently, so revocation never looks signed in.
fn retain_failure(current: &mut Option<Failure>, next: Failure) {
    let priority = |failure: Failure| match failure.state {
        "expired" => 3,
        "permission_denied" => 2,
        "network_error" => 1,
        _ => 0,
    };
    if current.is_none_or(|previous| priority(next) > priority(previous)) {
        *current = Some(next);
    }
}

async fn get(client: &Client, origin: &str, path: &str) -> std::result::Result<Value, Failure> {
    // The outer deadline bounds DNS, headers and streaming the whole body.
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut response = client
            .get(format!("{origin}{path}"))
            .header("origin", origin)
            .header("cache-control", "no-cache")
            .send()
            .await
            .map_err(|_| NETWORK)?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                401 => Failure {
                    state: "expired",
                    message: "Your account sign-in expired; sign in again.",
                },
                403 => Failure {
                    state: "permission_denied",
                    message: "This login does not have permission to read this status.",
                },
                408 | 429 | 500..=599 => NETWORK,
                _ => UNKNOWN,
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| NETWORK)? {
            if bytes.len() + chunk.len() > BODY_LIMIT {
                return Err(UNKNOWN);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| UNKNOWN)
    })
    .await
    .map_err(|_| NETWORK)?
}

fn safe_text(text: &str) -> Option<&str> {
    (!text.trim().is_empty()
        && text.len() <= 256
        && !text.chars().any(char::is_control)
        && !text.contains("ncx_live_")
        && !text.contains("Bearer ")
        && !text.contains("nanocodex_account=")
        && !text.starts_with("s_"))
    .then_some(text)
}

fn project_machines(value: &Value) -> std::result::Result<Vec<Value>, Failure> {
    let machines = value["data"]
        .as_array()
        .filter(|items| items.len() <= HAND_LIMIT)
        .ok_or(UNKNOWN)?;
    let mut result = Vec::new();
    for machine in machines {
        let id = machine["id"].as_str().and_then(safe_text).ok_or(UNKNOWN)?;
        let name = machine["name"]
            .as_str()
            .and_then(safe_text)
            .ok_or(UNKNOWN)?;
        if result.iter().any(|item: &Value| item["id"] == id) {
            return Err(UNKNOWN);
        }
        let capabilities = machine["capabilities"].as_array().ok_or(UNKNOWN)?;
        let background = capabilities
            .iter()
            .any(|capability| capability == "background_limited");
        let screen_only = !capabilities.is_empty()
            && capabilities
                .iter()
                .all(|capability| capability == "screen" || capability == "computer");
        let kind = if screen_only {
            "screen_only"
        } else if id.starts_with("vm:") {
            "vm"
        } else {
            "hand"
        };
        let name = if name == id && id.starts_with("vm:") {
            format!(
                "VM {}",
                id.trim_start_matches("vm:")
                    .chars()
                    .take(8)
                    .collect::<String>()
            )
        } else {
            name.to_owned()
        };
        result.push(json!({"id": id, "name": name, "kind": kind,
            "online": if screen_only { Value::Null } else { json!(true) },
            "transport": if screen_only { "screen_only" } else { "tool_host" },
            "availability": "advertised",
            "health": if screen_only { "screen_advertised" } else { "connected" },
            "detail": if background { Some("Background availability limited") } else { None }}));
    }
    Ok(result)
}

fn project_screens(value: &Value, hands: &mut Vec<Value>) -> std::result::Result<(), Failure> {
    let screens = value["surfaces"]
        .as_array()
        .filter(|items| items.len() <= HAND_LIMIT)
        .ok_or(UNKNOWN)?;
    // Build a separate projection first so a malformed response cannot leave
    // half of its screen inventory looking complete.
    let mut projected = hands.clone();
    for screen in screens {
        let id = screen["machine_id"]
            .as_str()
            .and_then(safe_text)
            .ok_or(UNKNOWN)?;
        let name = screen["machine_name"]
            .as_str()
            .and_then(safe_text)
            .ok_or(UNKNOWN)?;
        let name = if name == id && id.starts_with("vm:") {
            format!(
                "VM {}",
                id.trim_start_matches("vm:")
                    .chars()
                    .take(8)
                    .collect::<String>()
            )
        } else {
            name.to_owned()
        };
        let transport = match screen["transport"].as_str() {
            Some("frames-v1") => "screen_frames",
            None => "screen_webrtc",
            _ => "screen_unknown",
        };
        if let Some(hand) = projected.iter_mut().find(|hand| hand["id"] == id) {
            if hand["kind"] != "screen_only" {
                hand["transport"] = json!("tool_host_and_screen");
                hand["detail"] = json!("Screen also advertised");
            }
        } else {
            projected.push(json!({"id": id, "name": name, "kind": "screen_only", "online": null,
                "transport": transport, "availability": "screen_advertised", "health": "screen_advertised",
                "detail": "No connected tool Hand"}));
        }
    }
    *hands = projected;
    Ok(())
}
