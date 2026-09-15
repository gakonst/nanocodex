use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde::Serialize;
use url::Url;

use crate::{BrowserHarArtifact, BrowserHttpHeader, BrowserNetworkRequest};

use super::BrowserError;

pub(super) struct HarExchange {
    pub(super) request: BrowserNetworkRequest,
    pub(super) request_body: Option<HarBody>,
    pub(super) response_body: Option<HarBody>,
}

pub(super) struct HarBody {
    pub(super) body: String,
    pub(super) base64_encoded: bool,
}

pub(super) async fn write(
    output_dir: &Path,
    sequence: u64,
    exchanges: Vec<HarExchange>,
) -> Result<BrowserHarArtifact, BrowserError> {
    let body_count = exchanges
        .iter()
        .map(|exchange| {
            usize::from(exchange.request_body.is_some())
                + usize::from(exchange.response_body.is_some())
        })
        .sum();
    let entries = exchanges.into_iter().map(har_entry).collect();
    let artifact = Har {
        log: HarLog {
            version: "1.2",
            creator: HarCreator {
                name: "nanocodex-browser",
                version: env!("CARGO_PKG_VERSION"),
            },
            entries,
        },
    };
    let path = output_dir.join(format!("network-{sequence}.har"));
    tokio::fs::write(&path, serde_json::to_vec(&artifact)?).await?;
    Ok(BrowserHarArtifact {
        path,
        entry_count: artifact.log.entries.len(),
        body_count,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the HAR entry is assembled once from one typed network lifecycle"
)]
fn har_entry(exchange: HarExchange) -> HarEntry {
    let request = &exchange.request;
    let started_date_time = DateTime::<Utc>::from_timestamp_millis(
        i64::try_from(request.started_at_epoch_ms).unwrap_or(i64::MAX),
    )
    .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    .to_rfc3339();
    let query_string = Url::parse(&request.url)
        .ok()
        .map(|url| {
            url.query_pairs()
                .map(|(name, value)| HarNameValue {
                    name: name.into_owned(),
                    value: value.into_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    let request_body_size = exchange.request_body.as_ref().map_or(-1, encoded_len);
    let response_body_size = exchange.response_body.as_ref().map_or(-1, encoded_len);
    let time = request.duration_ms.map_or(0.0, milliseconds_as_f64);
    let (send, wait, receive) = request.timing.map_or((-1.0, -1.0, -1.0), |timing| {
        let send = (timing.send_end - timing.send_start).max(0.0);
        let wait = (timing.receive_headers_end - timing.send_end).max(0.0);
        let receive = request.duration_ms.map_or(-1.0, |total| {
            (milliseconds_as_f64(total) - send - wait).max(0.0)
        });
        (send, wait, receive)
    });
    HarEntry {
        started_date_time,
        time,
        request: HarRequest {
            method: request.method.clone(),
            url: request.url.clone(),
            http_version: request
                .protocol
                .clone()
                .unwrap_or_else(|| "HTTP/1.1".to_owned()),
            headers: har_headers(&request.request_headers),
            query_string,
            cookies: Vec::new(),
            headers_size: -1,
            body_size: request_body_size,
            post_data: exchange.request_body.map(|body| HarPostData {
                mime_type: header_value(&request.request_headers, "content-type")
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                text: body.body,
            }),
        },
        response: HarResponse {
            status: request.status.unwrap_or(0),
            status_text: request.status_text.clone().unwrap_or_default(),
            http_version: request
                .protocol
                .clone()
                .unwrap_or_else(|| "HTTP/1.1".to_owned()),
            headers: har_headers(&request.response_headers),
            cookies: Vec::new(),
            content: HarContent {
                size: request
                    .encoded_data_length
                    .and_then(|value| i64::try_from(value).ok())
                    .unwrap_or(response_body_size),
                mime_type: request
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                encoding: exchange
                    .response_body
                    .as_ref()
                    .and_then(|body| body.base64_encoded.then_some("base64")),
                text: exchange.response_body.map(|body| body.body),
            },
            redirect_url: header_value(&request.response_headers, "location").unwrap_or_default(),
            headers_size: -1,
            body_size: response_body_size,
        },
        cache: HarCache {},
        timings: HarTimings {
            blocked: -1.0,
            dns: request
                .timing
                .map_or(-1.0, |timing| phase(timing.dns_start, timing.dns_end)),
            connect: request.timing.map_or(-1.0, |timing| {
                phase(timing.connect_start, timing.connect_end)
            }),
            send,
            wait,
            receive,
            ssl: request
                .timing
                .map_or(-1.0, |timing| phase(timing.ssl_start, timing.ssl_end)),
        },
    }
}

fn phase(start: f64, end: f64) -> f64 {
    if start < 0.0 || end < 0.0 {
        -1.0
    } else {
        (end - start).max(0.0)
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "HAR timings are defined as floating-point milliseconds"
)]
const fn milliseconds_as_f64(milliseconds: u64) -> f64 {
    milliseconds as f64
}

fn encoded_len(body: &HarBody) -> i64 {
    if body.base64_encoded {
        STANDARD
            .decode(&body.body)
            .ok()
            .and_then(|decoded| i64::try_from(decoded.len()).ok())
            .unwrap_or(-1)
    } else {
        i64::try_from(body.body.len()).unwrap_or(i64::MAX)
    }
}

fn har_headers(headers: &[BrowserHttpHeader]) -> Vec<HarNameValue> {
    headers
        .iter()
        .map(|header| HarNameValue {
            name: header.name.clone(),
            value: header.value.clone().unwrap_or_default(),
        })
        .collect()
}

fn header_value(headers: &[BrowserHttpHeader], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .and_then(|header| header.value.clone())
}

#[derive(Serialize)]
struct Har {
    log: HarLog,
}

#[derive(Serialize)]
struct HarLog {
    version: &'static str,
    creator: HarCreator,
    entries: Vec<HarEntry>,
}

#[derive(Serialize)]
struct HarCreator {
    name: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarEntry {
    started_date_time: String,
    time: f64,
    request: HarRequest,
    response: HarResponse,
    cache: HarCache,
    timings: HarTimings,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarRequest {
    method: String,
    url: String,
    http_version: String,
    headers: Vec<HarNameValue>,
    query_string: Vec<HarNameValue>,
    cookies: Vec<HarNameValue>,
    headers_size: i64,
    body_size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_data: Option<HarPostData>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarResponse {
    status: i64,
    status_text: String,
    http_version: String,
    headers: Vec<HarNameValue>,
    cookies: Vec<HarNameValue>,
    content: HarContent,
    redirect_url: String,
    headers_size: i64,
    body_size: i64,
}

#[derive(Serialize)]
struct HarNameValue {
    name: String,
    value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarPostData {
    mime_type: String,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarContent {
    size: i64,
    mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Serialize)]
struct HarCache {}

#[derive(Serialize)]
struct HarTimings {
    blocked: f64,
    dns: f64,
    connect: f64,
    send: f64,
    wait: f64,
    receive: f64,
    ssl: f64,
}
