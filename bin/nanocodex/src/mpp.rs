use std::{path::PathBuf, sync::Arc};

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use std::path::Path;

mod egress;
mod resource;

use self::egress::TempoEgress;
use alloy_primitives::Address;
use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Context, Result, eyre};
use mpp::{
    client::{
        PaymentContext, TempoAccountsProvider,
        tempo::{
            AutoswapConfig,
            session::store::{SqliteChannelStore, SqliteChannelStoreOptions},
        },
    },
    mcp::{
        client::{McpPayment, PendingPayment},
        credential_value, extract_challenges_from_data,
    },
    protocol::{
        core::{
            PaymentChallenge,
            accept_payment::{ACCEPT_PAYMENT_HEADER, from_methods},
        },
        methods::tempo::{INTENT_CHARGE, METHOD_NAME},
    },
};
use nanocodex::tools::mcp::{McpPaymentProvider, McpPendingPayment};
use nanocodex_egress::EgressProxy;
use nanousd::{NANOUSD_ADDRESS, TEMPO_MAINNET_CHAIN_ID};
use serde_json::Value;

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use nanocodex_vm::host::{EgressFile, GUEST_EGRESS_ROOT};

use crate::vm::EgressLease;

const DEFAULT_MPP_API_BASE_URL: &str = "https://openai.mpp.tempo.xyz/v1";
const DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS: u16 = 100;
const DEFAULT_MCP_SESSION_MAX_DEPOSIT: u128 = 50_000;
const DEFAULT_MCP_SESSION_TOP_UP: u128 = 50_000;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
const GUEST_EGRESS_DIRECTORY: &str = "tempo";
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
const GUEST_EGRESS_CA_FILENAME: &str = "egress-ca.pem";

#[derive(Args, Clone)]
pub(crate) struct MppArgs {
    /// Connect directly to `OpenAI`. This is the default provider.
    #[arg(
        long = "provider.openai",
        global = true,
        env = "NANOCODEX_PROVIDER_OPENAI",
        default_value_t = false,
        action = ArgAction::SetTrue,
        conflicts_with = "tempo"
    )]
    openai: bool,

    /// Pay for HTTPS Responses and tool requests with Tempo Charge.
    #[arg(
        long = "provider.tempo",
        id = "tempo",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO",
        default_value_t = false,
        action = ArgAction::SetTrue
    )]
    enabled: bool,

    /// Paid MPP API base used for HTTPS Responses.
    #[arg(
        long = "provider.tempo.api-base-url",
        id = "tempo_api_base_url",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_API_BASE_URL",
        default_value = DEFAULT_MPP_API_BASE_URL,
        value_parser = NonEmptyStringValueParser::new()
    )]
    api_base_url: String,

    /// Tempo Accounts state containing the logged-in account and access keys.
    #[arg(
        long = "provider.tempo.wallet-store",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_WALLET_STORE"
    )]
    wallet_store: Option<PathBuf>,

    /// Preferred payment token and automatic-swap input currency.
    #[arg(
        long = "provider.tempo.payment-token",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_PAYMENT_TOKEN",
        default_value_t = NANOUSD_ADDRESS
    )]
    payment_token: Address,

    /// Maximum slippage for automatic swaps from the payment token, in basis points.
    #[arg(
        long = "provider.tempo.swap-slippage-bps",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_SWAP_SLIPPAGE_BPS",
        default_value_t = DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS
    )]
    swap_slippage_bps: u16,

    /// Optional access key for gated MPP deployments.
    #[arg(
        long = "provider.tempo.api-key",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_API_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    mpp_api_key: Option<String>,
}

impl MppArgs {
    pub(crate) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) async fn start(self) -> Result<Option<MppAdapter>> {
        if self.openai || !self.enabled {
            return Ok(None);
        }

        resource::ensure_mpp_file_descriptor_capacity()?;
        let api_base_url = normalize_api_base_url(&self.api_base_url)?;
        let allow_loopback = reqwest::Url::parse(&api_base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            });
        let provider = self.wallet_store.map_or_else(
            TempoAccountsProvider::from_default_store,
            TempoAccountsProvider::from_store,
        )?;
        let provider = provider
            .with_expected_chain_id(TEMPO_MAINNET_CHAIN_ID)
            .with_preferred_currency(self.payment_token)
            .with_autoswap(AutoswapConfig::new(
                self.payment_token,
                self.swap_slippage_bps,
            ));
        let egress = EgressProxy::builder()
            .allow_loopback_upstreams(allow_loopback)
            .layer(TempoEgress::new(provider.clone()))
            .spawn()
            .await
            .wrap_err("failed to start the embedded MPP egress proxy")?;

        Ok(Some(MppAdapter {
            api_base_url,
            mpp_api_key: self.mpp_api_key,
            provider,
            egress: Some(egress),
        }))
    }
}

fn normalize_api_base_url(value: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(value).wrap_err("Tempo MPP API base URL is invalid")?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => return Err(eyre!("unsupported Tempo MPP API URL scheme {scheme}")),
    }
    if url.cannot_be_a_base() {
        return Err(eyre!("Tempo MPP API URL must be an absolute HTTP URL"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| eyre!("Tempo MPP API URL must be an absolute HTTP URL"))?;
    if url.scheme() == "http" {
        let is_loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if !is_loopback {
            return Err(eyre!(
                "Tempo MPP API URL must use HTTPS unless it is loopback"
            ));
        }
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(eyre!(
            "Tempo MPP API URL must not include a query or fragment"
        ));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

pub(crate) struct MppAdapter {
    api_base_url: String,
    mpp_api_key: Option<String>,
    provider: TempoAccountsProvider,
    egress: Option<EgressProxy>,
}

impl MppAdapter {
    pub(crate) fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    pub(crate) fn tool_environment(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        self.egress.as_ref().map_or_else(Vec::new, |egress| {
            egress.environment().into_iter().collect()
        })
    }

    #[cfg(any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    pub(crate) fn vm_egress_lease(&self) -> Result<EgressLease> {
        let egress = self
            .egress
            .as_ref()
            .ok_or_else(|| eyre!("MPP egress proxy is not running"))?;
        let route = egress.route();
        let guest_certificate = Path::new(GUEST_EGRESS_ROOT)
            .join(GUEST_EGRESS_DIRECTORY)
            .join(GUEST_EGRESS_CA_FILENAME);
        let mut lease = EgressLease::internet();
        lease
            .insert_file(EgressFile::new(
                &guest_certificate,
                route.ca_certificate_pem(),
                0o444,
            ))
            .wrap_err("failed to provision the MPP egress CA in the VM")?;
        for (name, value) in route.environment(&guest_certificate) {
            let name = name
                .into_string()
                .map_err(|_| eyre!("MPP egress environment name is not valid UTF-8"))?;
            let value = value
                .into_string()
                .map_err(|_| eyre!("MPP egress environment value for {name} is not valid UTF-8"))?;
            lease
                .insert_environment(name, value)
                .wrap_err("failed to configure MPP egress environment in the VM")?;
        }
        Ok(lease)
    }

    #[cfg(not(any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    pub(crate) fn vm_egress_lease(&self) -> Result<EgressLease> {
        Err(eyre!(
            "provider-backed VM egress is unsupported on {}",
            std::env::consts::ARCH
        ))
    }

    fn http_client_builder(&self) -> Result<reqwest::ClientBuilder> {
        let egress = self
            .egress
            .as_ref()
            .ok_or_else(|| eyre!("MPP egress proxy is not running"))?;
        let certificate = std::fs::read(egress.ca_certificate_path())
            .wrap_err("failed to read the MPP egress CA certificate")?;
        let certificate = reqwest::Certificate::from_pem(&certificate)
            .wrap_err("failed to parse the MPP egress CA certificate")?;
        let proxy = reqwest::Proxy::all(egress.proxy_url())
            .wrap_err("failed to configure the MPP egress proxy")?;
        Ok(reqwest::Client::builder()
            .proxy(proxy)
            .add_root_certificate(certificate))
    }

    pub(crate) fn responses_http_client(&self) -> Result<reqwest::Client> {
        self.http_client_builder()?
            .default_headers(responses_payment_headers(self.mpp_api_key.as_deref())?)
            .build()
            .wrap_err("failed to configure the MPP-aware Responses HTTP client")
    }

    pub(crate) fn tool_http_client(&self) -> Result<reqwest::Client> {
        self.http_client_builder()?
            .build()
            .wrap_err("failed to configure the MPP-aware tool HTTP client")
    }

    pub(crate) fn mcp_payment_provider(&self, url: &str) -> Result<Arc<dyn McpPaymentProvider>> {
        let store = SqliteChannelStore::open(SqliteChannelStoreOptions {
            namespace: url.to_owned(),
            request_url: Some(url.to_owned()),
            ..Default::default()
        })
        .wrap_err("failed to open the durable Tempo MCP channel store")?;
        let provider = self
            .provider
            .clone()
            .with_session_store(Arc::new(store))
            .with_session_max_deposit(DEFAULT_MCP_SESSION_MAX_DEPOSIT)
            .with_session_top_up_amount(DEFAULT_MCP_SESSION_TOP_UP);
        Ok(Arc::new(TempoMcpPayment::new(
            provider,
            url.parse().wrap_err("Tempo MCP URL is invalid")?,
        )))
    }

    pub(crate) async fn shutdown(mut self) -> Result<()> {
        if let Some(egress) = self.egress.take() {
            egress
                .shutdown()
                .await
                .wrap_err("failed to stop the embedded MPP egress proxy")?;
        }
        Ok(())
    }
}

struct TempoMcpPayment {
    payment: McpPayment<TempoAccountsProvider>,
}

impl TempoMcpPayment {
    fn new(provider: TempoAccountsProvider, resource_url: reqwest::Url) -> Self {
        Self {
            payment: McpPayment::new(
                provider,
                PaymentContext {
                    url: resource_url,
                    headers: reqwest::header::HeaderMap::new(),
                },
            ),
        }
    }
}

#[async_trait::async_trait]
impl McpPaymentProvider for TempoMcpPayment {
    async fn prepare(
        &self,
        payment_required: &Value,
    ) -> Result<Option<Box<dyn McpPendingPayment>>, String> {
        let challenges: Vec<PaymentChallenge> = extract_challenges_from_data(payment_required)
            .ok_or_else(|| "MCP payment-required payload has no valid challenges".to_owned())?;
        let Some(pending) = self
            .payment
            .prepare(&challenges)
            .await
            .map_err(|error| format!("failed to prepare Tempo MCP payment: {error}"))?
        else {
            return Ok(None);
        };
        let credential = match credential_value(pending.credential()) {
            Ok(credential) => credential,
            Err(error) => {
                let rollback = pending.rollback().await;
                let mut message = format!("failed to encode Tempo MCP credential: {error}");
                if let Err(rollback) = rollback {
                    message.push_str(&format!("; rollback failed: {rollback}"));
                }
                return Err(message);
            }
        };
        Ok(Some(Box::new(TempoPendingMcpPayment {
            pending,
            credential,
        })))
    }
}

struct TempoPendingMcpPayment {
    pending: PendingPayment<TempoAccountsProvider>,
    credential: Value,
}

#[async_trait::async_trait]
impl McpPendingPayment for TempoPendingMcpPayment {
    fn credential(&self) -> &Value {
        &self.credential
    }

    async fn commit(self: Box<Self>) -> Result<(), String> {
        let Self { pending, .. } = *self;
        pending
            .commit()
            .await
            .map_err(|error| format!("failed to commit Tempo MCP payment: {error}"))
    }

    async fn rollback(self: Box<Self>) -> Result<(), String> {
        let Self { pending, .. } = *self;
        pending
            .rollback()
            .await
            .map_err(|error| format!("failed to roll back Tempo MCP payment: {error}"))
    }
}

fn responses_payment_headers(api_key: Option<&str>) -> Result<reqwest::header::HeaderMap> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        ACCEPT_PAYMENT_HEADER,
        reqwest::header::HeaderValue::from_str(&from_methods(&[(METHOD_NAME, INTENT_CHARGE)]))
            .wrap_err("failed to encode the Tempo Charge payment preference")?,
    );
    if let Some(api_key) = api_key {
        headers.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            reqwest::header::HeaderValue::from_str(api_key)
                .wrap_err("invalid provider.tempo.api-key header value")?,
        );
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        mpp: MppArgs,
    }

    fn test_args() -> MppArgs {
        MppArgs {
            openai: false,
            enabled: false,
            api_base_url: DEFAULT_MPP_API_BASE_URL.to_owned(),
            wallet_store: None,
            payment_token: NANOUSD_ADDRESS,
            swap_slippage_bps: DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS,
            mpp_api_key: None,
        }
    }

    #[tokio::test]
    async fn mpp_is_opt_in() {
        assert!(test_args().start().await.unwrap().is_none());
    }

    #[test]
    fn payment_token_defaults_to_nanousd_and_accepts_pathusd() {
        let default = TestCli::try_parse_from(["nanocodex"]).unwrap();
        assert_eq!(default.mpp.payment_token, NANOUSD_ADDRESS);

        let pathusd = alloy_primitives::address!("20c0000000000000000000000000000000000000");
        let configured = TestCli::try_parse_from([
            "nanocodex",
            "--provider.tempo.payment-token",
            "0x20c0000000000000000000000000000000000000",
        ])
        .unwrap();
        assert_eq!(configured.mpp.payment_token, pathusd);
    }

    #[cfg(any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    #[tokio::test]
    async fn vm_lease_contains_only_the_proxy_route_and_public_ca() {
        let egress = EgressProxy::builder().spawn().await.unwrap();
        let proxy_url = egress.route().proxy_url().to_owned();
        let adapter = MppAdapter {
            api_base_url: DEFAULT_MPP_API_BASE_URL.to_owned(),
            mpp_api_key: None,
            egress: Some(egress),
        };

        let lease = adapter.vm_egress_lease().unwrap();
        let guest_certificate = Path::new(GUEST_EGRESS_ROOT)
            .join(GUEST_EGRESS_DIRECTORY)
            .join(GUEST_EGRESS_CA_FILENAME);

        assert_eq!(
            lease.guest_environment().get("HTTPS_PROXY"),
            Some(&proxy_url)
        );
        assert_eq!(
            lease.guest_environment().get("SSL_CERT_FILE"),
            Some(&guest_certificate.to_string_lossy().into_owned())
        );
        let file = lease.guest_files().next().unwrap();
        assert_eq!(file.guest_path(), guest_certificate);
        assert!(file.contents().starts_with(b"-----BEGIN CERTIFICATE-----"));
        assert!(lease.guest_environment().keys().all(|name| {
            !name.contains("WALLET") && !name.contains("PRIVATE") && !name.contains("SECRET")
        }));
        adapter.shutdown().await.unwrap();
    }

    #[test]
    fn responses_request_charge_only() {
        let headers = responses_payment_headers(Some("secret")).unwrap();

        assert_eq!(headers.get("accept-payment").unwrap(), "tempo/charge");
        assert_eq!(headers.get("x-api-key").unwrap(), "secret");
    }

    #[test]
    fn accepts_https_and_loopback_http_api_bases() {
        assert_eq!(
            normalize_api_base_url("https://openai.mpp.tempo.xyz/v1/").unwrap(),
            "https://openai.mpp.tempo.xyz/v1"
        );
        assert_eq!(
            normalize_api_base_url("http://127.0.0.1:8080/v1").unwrap(),
            "http://127.0.0.1:8080/v1"
        );
    }

    #[test]
    fn rejects_websocket_api_base() {
        let error = normalize_api_base_url("wss://openai.mpp.tempo.xyz/v1/responses").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported Tempo MPP API URL scheme")
        );
    }

    #[test]
    fn rejects_plaintext_remote_api_base() {
        let error = normalize_api_base_url("http://openai.mpp.tempo.xyz/v1").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must use HTTPS unless it is loopback")
        );
    }
}
