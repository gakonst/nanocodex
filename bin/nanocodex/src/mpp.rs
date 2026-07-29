use std::path::PathBuf;

mod egress;
mod resource;

use self::egress::{EgressPolicy, MppEgress};
use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Context, Result, eyre};
use mpp::{
    MppError, PaymentChallenge, PaymentCredential,
    client::{PaymentProvider, TempoAccountsProvider, tempo::AutoswapConfig},
    protocol::{
        core::accept_payment::{ACCEPT_PAYMENT_HEADER, from_methods},
        intents::ChargeRequest,
        methods::tempo::{INTENT_CHARGE, METHOD_NAME},
    },
};
use nanousd::{NANOUSD_ADDRESS, TEMPO_MAINNET_CHAIN_ID};

const DEFAULT_MPP_API_BASE_URL: &str = "https://openai.mpp.tempo.xyz/v1";
const DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS: u16 = 100;
const DEFAULT_MAX_EGRESS_CHARGE: u128 = 100_000;

#[derive(Args, Clone)]
pub(crate) struct MppArgs {
    /// Connect directly to `OpenAI`. This is the default provider.
    #[arg(
        long = "provider.openai",
        id = "provider_openai",
        global = true,
        env = "NANOCODEX_PROVIDER_OPENAI",
        default_value_t = false,
        action = ArgAction::SetTrue,
        conflicts_with = "tempo"
    )]
    openai: bool,

    /// Pay for HTTPS Responses and tool requests with `NanoUSD` Charge.
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

    /// Maximum slippage for automatic swaps from `NanoUSD`, in basis points.
    #[arg(
        long = "provider.tempo.swap-slippage-bps",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_SWAP_SLIPPAGE_BPS",
        default_value_t = DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS
    )]
    swap_slippage_bps: u16,

    /// Maximum one-shot Charge payment in `NanoUSD` atomic units.
    #[arg(
        long = "provider.tempo.egress-max-charge",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_EGRESS_MAX_CHARGE",
        default_value_t = DEFAULT_MAX_EGRESS_CHARGE
    )]
    egress_max_charge: u128,

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
        let provider = self.wallet_store.map_or_else(
            TempoAccountsProvider::from_default_store,
            TempoAccountsProvider::from_store,
        )?;
        let provider = provider
            .with_expected_chain_id(TEMPO_MAINNET_CHAIN_ID)
            .with_autoswap(AutoswapConfig::new(NANOUSD_ADDRESS, self.swap_slippage_bps));
        let provider = CappedChargeProvider {
            provider,
            max_charge: self.egress_max_charge,
        };
        let egress = MppEgress::start(provider, EgressPolicy::default())
            .await
            .wrap_err("failed to start the embedded MPP egress proxy")?;

        Ok(Some(MppAdapter {
            api_base_url: normalize_api_base_url(&self.api_base_url)?,
            mpp_api_key: self.mpp_api_key,
            egress: Some(egress),
        }))
    }
}

#[derive(Clone)]
struct CappedChargeProvider<P> {
    provider: P,
    max_charge: u128,
}

impl<P> PaymentProvider for CappedChargeProvider<P>
where
    P: PaymentProvider,
{
    fn supports(&self, method: &str, intent: &str) -> bool {
        self.provider.supports(method, intent)
    }

    async fn pay(&self, challenge: &PaymentChallenge) -> Result<PaymentCredential, MppError> {
        challenge
            .request
            .decode::<ChargeRequest>()?
            .validate_max_amount(&self.max_charge.to_string())?;
        self.provider.pay(challenge).await
    }

    async fn commit_payment(
        &self,
        challenge: &PaymentChallenge,
        credential: &PaymentCredential,
    ) -> Result<(), MppError> {
        self.provider.commit_payment(challenge, credential).await
    }

    async fn rollback_payment(
        &self,
        challenge: &PaymentChallenge,
        credential: &PaymentCredential,
    ) -> Result<(), MppError> {
        self.provider.rollback_payment(challenge, credential).await
    }

    fn accept_payment_header(&self) -> Option<String> {
        self.provider.accept_payment_header()
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
    egress: Option<MppEgress>,
}

impl MppAdapter {
    pub(crate) fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    pub(crate) fn tool_environment(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        self.egress
            .as_ref()
            .map_or_else(Vec::new, MppEgress::environment)
    }

    fn http_client_builder(&self) -> Result<reqwest::ClientBuilder> {
        let egress = self
            .egress
            .as_ref()
            .ok_or_else(|| eyre!("MPP egress proxy is not running"))?;
        let certificate = std::fs::read(egress.certificate_path())
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use mpp::{Base64UrlJson, PaymentPayload};

    use super::*;

    #[derive(Clone, Default)]
    struct MockProvider {
        payments: Arc<AtomicUsize>,
        commits: Arc<AtomicUsize>,
        rollbacks: Arc<AtomicUsize>,
    }

    impl PaymentProvider for MockProvider {
        fn supports(&self, method: &str, intent: &str) -> bool {
            method == "tempo" && intent == "charge"
        }

        async fn pay(&self, challenge: &PaymentChallenge) -> Result<PaymentCredential, MppError> {
            self.payments.fetch_add(1, Ordering::SeqCst);
            Ok(PaymentCredential::new(
                challenge.to_echo(),
                PaymentPayload::hash("paid"),
            ))
        }

        async fn commit_payment(
            &self,
            _: &PaymentChallenge,
            _: &PaymentCredential,
        ) -> Result<(), MppError> {
            self.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn rollback_payment(
            &self,
            _: &PaymentChallenge,
            _: &PaymentCredential,
        ) -> Result<(), MppError> {
            self.rollbacks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn test_args() -> MppArgs {
        MppArgs {
            openai: false,
            enabled: false,
            api_base_url: DEFAULT_MPP_API_BASE_URL.to_owned(),
            wallet_store: None,
            swap_slippage_bps: DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS,
            egress_max_charge: DEFAULT_MAX_EGRESS_CHARGE,
            mpp_api_key: None,
        }
    }

    fn challenge(amount: &str) -> PaymentChallenge {
        let request = Base64UrlJson::from_value(&serde_json::json!({
            "amount": amount,
            "currency": NANOUSD_ADDRESS,
            "recipient": "0x1111111111111111111111111111111111111111",
            "methodDetails": {"chainId": TEMPO_MAINNET_CHAIN_ID},
        }))
        .unwrap();
        PaymentChallenge::new("challenge", "api.example.com", "tempo", "charge", request)
    }

    #[tokio::test]
    async fn mpp_is_opt_in() {
        assert!(test_args().start().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn charge_cap_is_checked_before_payment() {
        let inner = MockProvider::default();
        let payments = Arc::clone(&inner.payments);
        let provider = CappedChargeProvider {
            provider: inner,
            max_charge: 100,
        };

        let error = provider.pay(&challenge("101")).await.unwrap_err();

        assert!(matches!(error, MppError::AmountExceedsMax { .. }));
        assert_eq!(payments.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn capped_provider_forwards_payment_lifecycle() {
        let inner = MockProvider::default();
        let payments = Arc::clone(&inner.payments);
        let commits = Arc::clone(&inner.commits);
        let rollbacks = Arc::clone(&inner.rollbacks);
        let provider = CappedChargeProvider {
            provider: inner,
            max_charge: 100,
        };
        let challenge = challenge("100");
        let credential = provider.pay(&challenge).await.unwrap();
        provider
            .commit_payment(&challenge, &credential)
            .await
            .unwrap();
        provider
            .rollback_payment(&challenge, &credential)
            .await
            .unwrap();

        assert_eq!(payments.load(Ordering::SeqCst), 1);
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
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
