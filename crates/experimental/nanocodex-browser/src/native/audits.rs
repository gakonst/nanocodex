use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    process::Stdio,
    time::Duration,
};

use chromiumoxide::{
    Page,
    cdp::{browser_protocol::page::PrintToPdfParams, js_protocol::runtime::ExecutionContextId},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{process::Command, time::timeout};
use url::Url;

use crate::{
    BrowserAccessibilityImpact, BrowserAxeAudit, BrowserAxeFinding, BrowserAxeNode,
    BrowserCruxClient, BrowserCruxCollectionPeriod, BrowserCruxFormFactor, BrowserCruxFraction,
    BrowserCruxHistogramBin, BrowserCruxMetric, BrowserCruxReport, BrowserCruxScope,
    BrowserLighthouseCategory, BrowserLighthouseCategoryScore, BrowserLighthouseFinding,
    BrowserLighthouseFormFactor, BrowserLighthouseReport, BrowserPdfArtifact,
};

use super::{BrowserError, evaluate_typed_in_context};

const AXE_BUNDLE: &str = include_str!("../../assets/axe.js");
const MAX_AXE_FINDINGS: usize = 200;
const MAX_AXE_NODES_PER_FINDING: usize = 50;
const MAX_LIGHTHOUSE_FINDINGS: usize = 100;
const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;
const MAX_LIGHTHOUSE_REPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CRUX_RESPONSE_BYTES: usize = 1024 * 1024;
const LIGHTHOUSE_TIMEOUT: Duration = Duration::from_mins(2);
const DEFAULT_CRUX_ENDPOINT: &str = "https://chromeuxreport.googleapis.com/v1/records:queryRecord";

pub(super) async fn pdf(
    page: &Page,
    output_dir: &Path,
    sequence: u64,
    options: PdfOptions,
) -> Result<BrowserPdfArtifact, BrowserError> {
    let path = output_dir.join(format!("page-{sequence}.pdf"));
    let bytes = page
        .pdf(PrintToPdfParams {
            landscape: Some(options.landscape),
            print_background: Some(options.print_background),
            prefer_css_page_size: Some(options.prefer_css_page_size),
            generate_tagged_pdf: Some(options.tagged),
            generate_document_outline: Some(options.document_outline),
            ..PrintToPdfParams::default()
        })
        .await?;
    if bytes.len() > MAX_PDF_BYTES {
        return Err(BrowserError::PdfTooLarge {
            bytes: bytes.len(),
            maximum: MAX_PDF_BYTES,
        });
    }
    tokio::fs::write(&path, &bytes).await?;
    Ok(BrowserPdfArtifact {
        path,
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        tagged: options.tagged,
        document_outline: options.document_outline,
    })
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent flags map directly to Chromium print-to-PDF options"
)]
pub(super) struct PdfOptions {
    pub(super) landscape: bool,
    pub(super) print_background: bool,
    pub(super) prefer_css_page_size: bool,
    pub(super) tagged: bool,
    pub(super) document_outline: bool,
}

pub(super) async fn install_axe(page: &Page) -> Result<(), BrowserError> {
    let mut params =
        chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams::new(
            AXE_BUNDLE,
        );
    params.run_immediately = Some(true);
    page.add_init_script(params).await?;
    Ok(())
}

pub(super) async fn axe(
    page: &Page,
    context_id: Option<ExecutionContextId>,
    frame_id: Option<String>,
    frame_url: Option<String>,
) -> Result<BrowserAxeAudit, BrowserError> {
    let expression = format!(
        r#"(
async () => {{
  const maxFindings = {MAX_AXE_FINDINGS};
  const maxNodes = {MAX_AXE_NODES_PER_FINDING};
  const result = await globalThis.__nanocodexAxe.run(document, {{
    iframes: false,
    resultTypes: ["violations", "incomplete", "passes", "inapplicable"]
  }});
  const mapFinding = finding => ({{
    id: finding.id,
    impact: finding.impact,
    description: finding.description,
    help: finding.help,
    helpUrl: finding.helpUrl,
    tags: finding.tags,
    nodeCount: finding.nodes.length,
    nodes: finding.nodes.slice(0, maxNodes).map(node => ({{
      selector: node.target
        .map(part => Array.isArray(part) ? part.join(" >>> ") : String(part))
        .join(" -> "),
      html: node.html,
      failureSummary: node.failureSummary ?? null
    }}))
  }});
  return {{
    url: result.url,
    engineVersion: result.testEngine.version,
    violations: result.violations.slice(0, maxFindings).map(mapFinding),
    incomplete: result.incomplete.slice(0, maxFindings).map(mapFinding),
    passCount: result.passes.length,
    inapplicableCount: result.inapplicable.length,
    truncated:
      result.violations.length > maxFindings ||
      result.incomplete.length > maxFindings ||
      [...result.violations, ...result.incomplete]
        .some(finding => finding.nodes.length > maxNodes)
  }};
}}
)()"#
    );
    let wire: AxeAuditWire = evaluate_typed_in_context(page, expression, context_id).await?;
    Ok(BrowserAxeAudit {
        url: wire.url,
        engine_version: wire.engine_version,
        violations: axe_findings(wire.violations, frame_id.as_deref(), frame_url.as_deref()),
        incomplete: axe_findings(wire.incomplete, frame_id.as_deref(), frame_url.as_deref()),
        pass_count: wire.pass_count,
        inapplicable_count: wire.inapplicable_count,
        truncated: wire.truncated,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the explicit CLI lifecycle and bounded report conversion form one audit boundary"
)]
pub(super) async fn lighthouse(
    executable: &Path,
    page_url: &str,
    websocket_address: &str,
    output_dir: &Path,
    sequence: u64,
    categories: Vec<BrowserLighthouseCategory>,
    form_factor: Option<BrowserLighthouseFormFactor>,
) -> Result<BrowserLighthouseReport, BrowserError> {
    let websocket = Url::parse(websocket_address)?;
    if websocket.scheme() != "ws" {
        return Err(BrowserError::LighthouseEndpoint {
            endpoint: websocket_address.to_owned(),
        });
    }
    let hostname = websocket
        .host_str()
        .ok_or_else(|| BrowserError::LighthouseEndpoint {
            endpoint: websocket_address.to_owned(),
        })?;
    let port =
        websocket
            .port_or_known_default()
            .ok_or_else(|| BrowserError::LighthouseEndpoint {
                endpoint: websocket_address.to_owned(),
            })?;
    let selected = lighthouse_categories(categories);
    let category_argument = selected
        .iter()
        .map(|category| lighthouse_category_name(*category))
        .collect::<Vec<_>>()
        .join(",");
    let path = output_dir.join(format!("lighthouse-{sequence}.json"));
    let stderr_path = output_dir.join(format!("lighthouse-{sequence}.stderr.log"));
    let stderr = tokio::fs::File::create(&stderr_path)
        .await?
        .into_std()
        .await;
    let mut command = Command::new(executable);
    command
        .arg(page_url)
        .arg("--quiet")
        .arg("--output=json")
        .arg(format!("--output-path={}", path.display()))
        .arg(format!("--hostname={hostname}"))
        .arg(format!("--port={port}"))
        .arg("--disable-storage-reset")
        .arg(format!("--only-categories={category_argument}"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    if matches!(form_factor, Some(BrowserLighthouseFormFactor::Desktop)) {
        command.arg("--preset=desktop");
    }
    let mut child = command.spawn()?;
    let status = if let Ok(status) = timeout(LIGHTHOUSE_TIMEOUT, child.wait()).await {
        status?
    } else {
        child.kill().await?;
        return Err(BrowserError::LighthouseTimeout);
    };
    if !status.success() {
        return Err(BrowserError::LighthouseFailed {
            status: status.code(),
            stderr_path,
        });
    }

    let report_bytes = tokio::fs::metadata(&path).await?.len();
    if report_bytes > MAX_LIGHTHOUSE_REPORT_BYTES {
        return Err(BrowserError::LighthouseReportTooLarge {
            bytes: report_bytes,
            maximum: MAX_LIGHTHOUSE_REPORT_BYTES,
        });
    }
    let report_path = path.clone();
    let encoded = tokio::task::spawn_blocking(move || {
        let mut encoded = Vec::new();
        std::fs::File::open(report_path)?
            .take(MAX_LIGHTHOUSE_REPORT_BYTES.saturating_add(1))
            .read_to_end(&mut encoded)?;
        Ok::<_, std::io::Error>(encoded)
    })
    .await
    .map_err(BrowserError::LighthouseReadTask)??;
    if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > MAX_LIGHTHOUSE_REPORT_BYTES {
        return Err(BrowserError::LighthouseReportTooLarge {
            bytes: u64::try_from(encoded.len()).unwrap_or(u64::MAX),
            maximum: MAX_LIGHTHOUSE_REPORT_BYTES,
        });
    }
    let wire: LighthouseWire = serde_json::from_slice(&encoded)?;
    let categories = selected
        .into_iter()
        .map(|category| BrowserLighthouseCategoryScore {
            category,
            score: wire
                .categories
                .get(lighthouse_category_name(category))
                .and_then(|category| category.score),
        })
        .collect();
    let mut findings = wire
        .audits
        .into_values()
        .filter(|audit| audit.score.is_some_and(|score| score < 1.0))
        .map(|audit| BrowserLighthouseFinding {
            id: audit.id,
            title: audit.title,
            description: audit.description,
            score: audit.score,
            score_display_mode: audit.score_display_mode,
            display_value: audit.display_value,
            numeric_value: audit.numeric_value,
            numeric_unit: audit.numeric_unit,
        })
        .collect::<Vec<_>>();
    findings.sort_by(|left, right| {
        left.score
            .unwrap_or(f64::INFINITY)
            .total_cmp(&right.score.unwrap_or(f64::INFINITY))
            .then_with(|| left.id.cmp(&right.id))
    });
    let omitted_finding_count = findings.len().saturating_sub(MAX_LIGHTHOUSE_FINDINGS);
    findings.truncate(MAX_LIGHTHOUSE_FINDINGS);
    Ok(BrowserLighthouseReport {
        path,
        lighthouse_version: wire.lighthouse_version,
        requested_url: wire.requested_url,
        final_url: wire.final_url,
        fetch_time: wire.fetch_time,
        categories,
        findings,
        omitted_finding_count,
    })
}

pub(super) async fn crux(
    client: &BrowserCruxClient,
    page_url: &str,
    scope: BrowserCruxScope,
    form_factor: Option<BrowserCruxFormFactor>,
) -> Result<BrowserCruxReport, BrowserError> {
    let mut requested = Url::parse(page_url)?;
    if !matches!(requested.scheme(), "http" | "https") {
        return Err(BrowserError::CruxUrl {
            url: requested.to_string(),
        });
    }
    requested.set_fragment(None);
    let request = match scope {
        BrowserCruxScope::Url => CruxRequest {
            url: Some(requested.to_string()),
            origin: None,
            form_factor: form_factor.map(crux_form_factor_name),
        },
        BrowserCruxScope::Origin => {
            let origin = requested.origin().ascii_serialization();
            if origin == "null" {
                return Err(BrowserError::CruxUrl {
                    url: requested.to_string(),
                });
            }
            CruxRequest {
                url: None,
                origin: Some(origin),
                form_factor: form_factor.map(crux_form_factor_name),
            }
        }
    };
    let endpoint = match &client.endpoint {
        Some(endpoint) => endpoint.clone(),
        None => Url::parse(DEFAULT_CRUX_ENDPOINT)?,
    };
    nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
    let response = reqwest::Client::new()
        .post(endpoint)
        .query(&[("key", client.api_key.as_str())])
        .json(&request)
        .send()
        .await
        .map_err(crux_request_error)?
        .error_for_status()
        .map_err(crux_request_error)?;
    let mut encoded = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(crux_request_error)?;
        if encoded.len().saturating_add(chunk.len()) > MAX_CRUX_RESPONSE_BYTES {
            return Err(BrowserError::CruxResponseTooLarge {
                maximum: MAX_CRUX_RESPONSE_BYTES,
            });
        }
        encoded.extend_from_slice(&chunk);
    }
    let response: CruxWire = serde_json::from_slice(&encoded)?;
    let metrics = response
        .record
        .metrics
        .into_iter()
        .map(|(name, metric)| BrowserCruxMetric {
            name,
            p75: metric.percentiles.and_then(|percentiles| percentiles.p75),
            histogram: metric
                .histogram
                .into_iter()
                .map(|bin| BrowserCruxHistogramBin {
                    start: bin.start,
                    end: bin.end,
                    density: bin.density,
                })
                .collect(),
            fractions: metric
                .fractions
                .into_iter()
                .map(|(name, density)| BrowserCruxFraction { name, density })
                .collect(),
        })
        .collect();
    Ok(BrowserCruxReport {
        requested_url: page_url.to_owned(),
        record_url: response.record.key.url,
        record_origin: response.record.key.origin,
        form_factor: response
            .record
            .key
            .form_factor
            .as_deref()
            .and_then(parse_crux_form_factor),
        collection_period: response
            .record
            .collection_period
            .map(crux_collection_period),
        metrics,
        normalized_url: response
            .url_normalization_details
            .map(|details| details.normalized_url),
    })
}

fn lighthouse_categories(
    categories: Vec<BrowserLighthouseCategory>,
) -> Vec<BrowserLighthouseCategory> {
    let categories = if categories.is_empty() {
        vec![
            BrowserLighthouseCategory::Performance,
            BrowserLighthouseCategory::Accessibility,
            BrowserLighthouseCategory::BestPractices,
            BrowserLighthouseCategory::Seo,
        ]
    } else {
        categories
    };
    categories
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

const fn lighthouse_category_name(category: BrowserLighthouseCategory) -> &'static str {
    match category {
        BrowserLighthouseCategory::Performance => "performance",
        BrowserLighthouseCategory::Accessibility => "accessibility",
        BrowserLighthouseCategory::BestPractices => "best-practices",
        BrowserLighthouseCategory::Seo => "seo",
    }
}

const fn crux_form_factor_name(form_factor: BrowserCruxFormFactor) -> &'static str {
    match form_factor {
        BrowserCruxFormFactor::Desktop => "DESKTOP",
        BrowserCruxFormFactor::Phone => "PHONE",
        BrowserCruxFormFactor::Tablet => "TABLET",
    }
}

fn parse_crux_form_factor(value: &str) -> Option<BrowserCruxFormFactor> {
    match value {
        "DESKTOP" => Some(BrowserCruxFormFactor::Desktop),
        "PHONE" => Some(BrowserCruxFormFactor::Phone),
        "TABLET" => Some(BrowserCruxFormFactor::Tablet),
        _ => None,
    }
}

fn axe_findings(
    findings: Vec<AxeFindingWire>,
    frame_id: Option<&str>,
    frame_url: Option<&str>,
) -> Vec<BrowserAxeFinding> {
    findings
        .into_iter()
        .map(|finding| BrowserAxeFinding {
            id: finding.id,
            impact: finding.impact,
            description: finding.description,
            help: finding.help,
            help_url: finding.help_url,
            tags: finding.tags,
            node_count: finding.node_count,
            nodes: finding
                .nodes
                .into_iter()
                .map(|node| BrowserAxeNode {
                    selector: node.selector,
                    html: node.html,
                    failure_summary: node.failure_summary,
                    frame_id: frame_id.map(str::to_owned),
                    frame_url: frame_url.map(str::to_owned),
                })
                .collect(),
        })
        .collect()
}

fn crux_request_error(error: reqwest::Error) -> BrowserError {
    BrowserError::CruxRequest {
        source: error.without_url(),
    }
}

fn crux_collection_period(period: CollectionPeriodWire) -> BrowserCruxCollectionPeriod {
    BrowserCruxCollectionPeriod {
        first_date: period.first_date.render(),
        last_date: period.last_date.render(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AxeAuditWire {
    url: String,
    engine_version: String,
    violations: Vec<AxeFindingWire>,
    incomplete: Vec<AxeFindingWire>,
    pass_count: usize,
    inapplicable_count: usize,
    truncated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AxeFindingWire {
    id: String,
    impact: Option<BrowserAccessibilityImpact>,
    description: String,
    help: String,
    help_url: String,
    tags: Vec<String>,
    node_count: usize,
    nodes: Vec<AxeNodeWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AxeNodeWire {
    selector: String,
    html: String,
    failure_summary: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LighthouseWire {
    lighthouse_version: String,
    requested_url: String,
    final_url: String,
    fetch_time: String,
    categories: BTreeMap<String, LighthouseCategoryWire>,
    audits: BTreeMap<String, LighthouseAuditWire>,
}

#[derive(Deserialize)]
struct LighthouseCategoryWire {
    score: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LighthouseAuditWire {
    id: String,
    title: String,
    description: Option<String>,
    score: Option<f64>,
    score_display_mode: String,
    display_value: Option<String>,
    numeric_value: Option<f64>,
    numeric_unit: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CruxRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    form_factor: Option<&'static str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CruxWire {
    record: CruxRecordWire,
    url_normalization_details: Option<UrlNormalizationDetailsWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CruxRecordWire {
    key: CruxKeyWire,
    metrics: BTreeMap<String, CruxMetricWire>,
    collection_period: Option<CollectionPeriodWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CruxKeyWire {
    url: Option<String>,
    origin: Option<String>,
    form_factor: Option<String>,
}

#[derive(Deserialize)]
struct CruxMetricWire {
    #[serde(default)]
    histogram: Vec<CruxHistogramBinWire>,
    percentiles: Option<CruxPercentilesWire>,
    #[serde(default)]
    fractions: BTreeMap<String, f64>,
}

#[derive(Deserialize)]
struct CruxHistogramBinWire {
    start: Option<f64>,
    end: Option<f64>,
    density: f64,
}

#[derive(Deserialize)]
struct CruxPercentilesWire {
    p75: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionPeriodWire {
    first_date: CruxDateWire,
    last_date: CruxDateWire,
}

#[derive(Deserialize)]
struct CruxDateWire {
    year: u16,
    month: u8,
    day: u8,
}

impl CruxDateWire {
    fn render(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UrlNormalizationDetailsWire {
    normalized_url: String,
}

#[cfg(test)]
mod tests {
    use super::crux_request_error;

    #[tokio::test]
    async fn crux_request_errors_do_not_retain_the_api_key_url() {
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind a local test socket");
        let address = listener.local_addr().expect("read the local test address");
        drop(listener);
        let secret = "crux-test-secret";
        let error = reqwest::Client::new()
            .get(format!("http://{address}/query?key={secret}"))
            .send()
            .await
            .expect_err("the dropped local listener must reject the request");
        let error = crux_request_error(error);

        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
    }
}
