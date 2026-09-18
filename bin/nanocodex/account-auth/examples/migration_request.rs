use serde_json::Value;
use std::{error::Error, time::Duration};
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let history_allowed = args.get(1).is_some_and(|p| {
        let Some(rest) = p.strip_prefix("/v1/agents/") else {
            return false;
        };
        let Some((id, suffix)) = rest.split_once('/') else {
            return false;
        };
        id.len() == 36
            && id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
            && [
                "events/history?limit=128&after=0",
                "events/history?limit=128",
            ]
            .contains(&suffix)
            && args[0] == "GET"
    });
    if (args.len() != 3 && args.len() != 4)
        || !["GET", "POST"].contains(&args[0].as_str())
        || !history_allowed
            && ![
                "/v1/agents",
                "/v1/account/conversation-project-migration-20260918",
            ]
            .contains(&args[1].as_str())
        || (args[0] == "POST" && (args[1] == "/v1/agents" || args.len() != 4))
        || (args[0] == "GET" && args.len() != 3)
    {
        return Err(
            "usage: migration_request GET|POST allowed-path output-file [POST-body-json-file]"
                .into(),
        );
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (origin, key) = nanocodex_cli_auth::enrollment_credentials(None)?;
    if origin != "https://nanocodex.gakonst.workers.dev" {
        return Err("unexpected origin".into());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut authorization =
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key.as_str()))?;
    authorization.set_sensitive(true);
    let identity: Value = client
        .get(format!("{origin}/v1/me"))
        .header(reqwest::header::AUTHORIZATION, authorization.clone())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if identity["user"]["id"] != "631f6a83-9e3f-474a-977a-68897d3ee436"
        || identity["team"]["id"] != "e1e0fc10-5e60-433d-b889-09c80dcd7c11"
    {
        return Err("account/team mismatch".into());
    }
    let method = reqwest::Method::from_bytes(args[0].as_bytes())?;
    let mut request = client
        .request(method, format!("{origin}{}", args[1]))
        .header(reqwest::header::AUTHORIZATION, authorization)
        .header("origin", &origin);
    if args[0] == "POST" {
        request = request.json(&serde_json::from_slice::<Value>(&std::fs::read(&args[3])?)?);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("request failed: HTTP {status}").into());
    }
    let value: Value = response.json().await?;
    std::fs::write(&args[2], serde_json::to_vec_pretty(&value)?)?;
    println!(
        "Verified account/team; HTTP {}; response saved to {}",
        status.as_u16(),
        args[2]
    );
    Ok(())
}
