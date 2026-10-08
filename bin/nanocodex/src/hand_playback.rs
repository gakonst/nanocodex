//! Owner commands for short-lived, view-only screen playback links.
use super::{Account, account};
use clap::{Args, Subcommand, ValueEnum};
use eyre::{Result, eyre};
use reqwest::Method;
use serde_json::{Value, json};

const PATH: &str = "/v1/account/hands/playback-links";

#[derive(Args)]
pub(crate) struct Playback {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a view-only HLS link for VLC or another HLS player.
    Create {
        /// Machine ID from `hand list`.
        machine_id: String,
        /// Surface ID; required only when the Hand publishes multiple screens.
        #[arg(long)]
        surface: Option<String>,
        /// Link lifetime in seconds; stopping the link also ends playback.
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u32).range(60..=28800))]
        expires_in: u32,
        #[arg(long, value_enum, default_value = "720p")]
        preset: Preset,
    },
    /// List playback links without revealing their bearer URLs.
    List,
    /// Revoke a playback link and stop its stream.
    Stop { id: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Preset {
    #[value(name = "720p")]
    Hd,
    #[value(name = "1080p")]
    FullHd,
}

impl Playback {
    pub(crate) async fn run(self) -> Result<()> {
        let account = account()?;
        let result = match self.command {
            Command::List => request(&account, Method::GET, PATH, None).await?,
            Command::Stop { id } => {
                if !id.strip_prefix("sp_").is_some_and(|suffix| {
                    suffix.len() == 32
                        && suffix
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                }) {
                    return Err(eyre!("Use a playback link ID from `hand stream list`."));
                }
                request(&account, Method::DELETE, &format!("{PATH}/{id}"), None).await?
            }
            Command::Create {
                machine_id,
                surface,
                expires_in,
                preset,
            } => {
                let catalog =
                    request(&account, Method::GET, "/v1/account/hands/screens", None).await?;
                let selected: Vec<_> = catalog["surfaces"]
                    .as_array()
                    .ok_or_else(|| eyre!("Invalid screen catalog"))?
                    .iter()
                    .filter(|hand| {
                        hand["machine_id"].as_str() == Some(&machine_id)
                            && surface
                                .as_ref()
                                .is_none_or(|id| hand["id"].as_str() == Some(id))
                    })
                    .collect();
                let hand = match selected.as_slice() {
                    [hand] => *hand,
                    [] => {
                        return Err(eyre!(
                            "No matching online screen. Check `hand list` and the surface ID."
                        ));
                    }
                    _ => {
                        return Err(eyre!(
                            "This Hand has multiple screens. Select one with --surface ID."
                        ));
                    }
                };
                if hand["playback"] != true {
                    return Err(eyre!(
                        "This Hand does not support playback links. Update the Hand first."
                    ));
                }
                let body = json!({"operation_id": uuid::Uuid::new_v4().to_string(), "machine_id":machine_id,"surface_id":hand["id"],
                    "generation":hand["generation"],"expires_in_seconds":expires_in,
                    "preset":match preset { Preset::Hd => "720p", Preset::FullHd => "1080p" }});
                let result = request(&account, Method::POST, PATH, Some(body)).await?;
                let origin = reqwest::Url::parse(&account.origin)?;
                let url = result["url"].as_str().and_then(|value| reqwest::Url::parse(value).ok())
                    .ok_or_else(|| eyre!("The server returned no playback URL. Check `hand stream list` before creating another."))?;
                if url.origin() != origin.origin()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                    || url.path()
                        != format!(
                            "/v1/screen-playback/{}/index.m3u8",
                            result["id"].as_str().unwrap_or("")
                        )
                    || !valid_view_query(&url)
                    || !url.path().ends_with("/index.m3u8")
                {
                    return Err(eyre!(
                        "The server returned an invalid playback URL. Check `hand stream list`."
                    ));
                }
                result
            }
        };
        println!("{}", serde_json::to_string(&result)?);
        Ok(())
    }
}

async fn request(
    account: &Account,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let writes = method != Method::GET;
    let mut request = account
        .client
        .request(method, format!("{}{path}", account.origin))
        .header("origin", &account.origin)
        .header("cache-control", "no-store");
    if let Some(body) = body {
        request = request.json(&body);
    }
    // Do not retry a write: the Hand may already have started its encoder.
    let response = request.send().await.map_err(|_| eyre!(if writes {
        "Playback request did not return a receipt. Check `hand stream list` before trying again."
    } else { "Could not reach the playback service." }))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|_| eyre!("Invalid playback response; check `hand stream list`."))?;
    if !status.is_success() {
        let description = match body["error"].as_str().unwrap_or("") {
            "unsupported" => "This Hand needs an update to support playback links.",
            "busy" | "too_many_streams" => {
                "A stream is already running. Stop it before creating another."
            }
            "not_found" | "stale_generation" | "host_unavailable" => {
                "The screen changed or disconnected. Refresh and try again."
            }
            "forbidden" | "unauthorized" => "Sign in with an account that can manage this Hand.",
            _ => "The playback request failed.",
        };
        return Err(eyre!("{description} ({status})"));
    }
    Ok(body)
}

fn valid_view_query(url: &reqwest::Url) -> bool {
    let pairs: Vec<_> = url.query_pairs().collect();
    pairs.len() == 1
        && pairs[0].0 == "token"
        && pairs[0].1.strip_prefix("nsv_").is_some_and(|token| {
            token.len() == 43
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}
