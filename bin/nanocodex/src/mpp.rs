use std::{
    net::TcpListener as StdTcpListener,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_transport_mpp::{
    CloseProvider, CloseRequest, MppApplicationWs, MppApplicationWsConnect, VoucherProvider,
    VoucherRequest,
};
use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Context, Result, eyre};
use futures_util::{SinkExt, StreamExt};
use mpp::{
    MppError, PaymentChallenge, PaymentCredential,
    client::{
        MultiProvider, PaymentContext, PaymentProvider, TempoProvider, TempoSessionProvider,
        tempo::{
            AutoswapConfig,
            session::store::{
                SqliteChannelStore, SqliteChannelStoreOptions, default_channel_database_path,
            },
            signing::{KeychainVersion, TempoSigningMode},
            wallet::TempoWallet,
        },
    },
    protocol::intents::ChargeRequest,
};
use mpp_egress::{EgressPolicy, MppEgress};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        http::{HeaderName, HeaderValue},
    },
};

const DEFAULT_MPP_WEBSOCKET_URL: &str = "wss://openai.mpp.tempo.xyz/v1/responses";
const DEFAULT_TEMPO_RPC_URL: &str = "https://rpc.mainnet.tempo.xyz";
const DEFAULT_TEMPO_PAY_WITH: &str = "0x20c0000000000000000000000000000000000000";
const DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS: u16 = 100;
const DEFAULT_SESSION_DEPOSIT: u128 = 5_000_000;
const DEFAULT_TOP_UP_AMOUNT: u128 = 5_000_000;
// Five $5 refill quanta while retaining a finite client-side authorization cap.
const DEFAULT_MAX_DEPOSIT: u128 = 25_000_000;
const DEFAULT_MAX_EGRESS_CHARGE: u128 = 100_000;

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

    /// Pay for the Responses WebSocket through MPP.
    #[arg(
        long = "provider.tempo",
        id = "tempo",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO",
        default_value_t = false,
        action = ArgAction::SetTrue
    )]
    enabled: bool,

    /// Paid MPP WebSocket endpoint.
    #[arg(
        long = "provider.tempo.responses-websocket-url",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_RESPONSES_WEBSOCKET_URL",
        default_value = DEFAULT_MPP_WEBSOCKET_URL,
        value_parser = NonEmptyStringValueParser::new()
    )]
    mpp_websocket_url: String,

    /// Tempo Wallet state containing the logged-in account and access key.
    #[arg(
        long = "provider.tempo.wallet-store",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_WALLET_STORE"
    )]
    wallet_store: Option<PathBuf>,

    /// `SQLite` channel store shared with Tempo Wallet and `MPPx` CLIs.
    #[arg(
        long = "provider.tempo.channel-store",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_CHANNEL_STORE"
    )]
    channel_store: Option<PathBuf>,

    /// Tempo RPC used for native TIP-1034 channel operations.
    #[arg(
        long = "provider.tempo.rpc-url",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_RPC_URL",
        default_value = DEFAULT_TEMPO_RPC_URL,
        value_parser = NonEmptyStringValueParser::new()
    )]
    rpc_url: String,

    /// Stablecoin used to acquire the MPP service's requested currency.
    #[arg(
        long = "provider.tempo.pay-with",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_PAY_WITH",
        default_value = DEFAULT_TEMPO_PAY_WITH,
        value_parser = NonEmptyStringValueParser::new()
    )]
    pay_with: String,

    /// Maximum slippage for automatic stablecoin swaps, in basis points.
    #[arg(
        long = "provider.tempo.swap-slippage-bps",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_SWAP_SLIPPAGE_BPS",
        default_value_t = DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS
    )]
    swap_slippage_bps: u16,

    /// Maximum total native session deposit in token atomic units.
    #[arg(
        long = "provider.tempo.max-deposit",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_MAX_DEPOSIT",
        default_value_t = DEFAULT_MAX_DEPOSIT
    )]
    max_deposit: u128,

    /// Preferred automatic session top-up in token atomic units.
    #[arg(
        long = "provider.tempo.top-up-amount",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_TOP_UP_AMOUNT",
        default_value_t = DEFAULT_TOP_UP_AMOUNT
    )]
    top_up_amount: u128,

    /// Maximum one-shot egress charge in token atomic units.
    #[arg(
        long = "provider.tempo.egress-max-charge",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_EGRESS_MAX_CHARGE",
        default_value_t = DEFAULT_MAX_EGRESS_CHARGE
    )]
    egress_max_charge: u128,

    /// Optional access key for gated MPP deployments such as Moderato staging.
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

    pub(crate) async fn start(
        self,
        direct_websocket_url: String,
    ) -> Result<(String, Option<MppAdapter>)> {
        if self.openai || !self.enabled {
            return Ok((direct_websocket_url, None));
        }
        let wallet_path = self.wallet_store.unwrap_or(
            default_channel_database_path()
                .map_err(|error| eyre!(error))?
                .with_file_name("store.json"),
        );
        let wallet = TempoWallet::load(&wallet_path)?;
        let endpoint = payment_http_url(&self.mpp_websocket_url)?;
        let api_base_url = openai_api_base_url(&self.mpp_websocket_url)?;
        let namespace = websocket_origin(&self.mpp_websocket_url)?;
        let store = SqliteChannelStore::open(SqliteChannelStoreOptions {
            namespace,
            path: self.channel_store,
            request_url: Some(self.mpp_websocket_url.clone()),
        })
        .map_err(|error| eyre!(error))
        .wrap_err("failed to open the Tempo session channel store")?;
        let autoswap = AutoswapConfig::new(
            self.pay_with
                .parse()
                .wrap_err("invalid provider.tempo.pay-with token address")?,
            self.swap_slippage_bps,
        );
        let charge = TempoProvider::new(wallet.signer.clone(), &self.rpc_url)
            .wrap_err("failed to configure the native Tempo charge provider")?
            .with_signing_mode(TempoSigningMode::Keychain {
                wallet: wallet.account,
                key_authorization: wallet.key_authorization.clone(),
                version: KeychainVersion::V2,
            })
            .with_expected_chain_id(wallet.chain_id)
            .with_autoswap(autoswap.clone());
        let session = TempoSessionProvider::new(wallet.signer, &self.rpc_url)
            .wrap_err("failed to configure the native Tempo session provider")?
            .with_signing_mode(TempoSigningMode::Keychain {
                wallet: wallet.account,
                key_authorization: wallet.key_authorization,
                version: KeychainVersion::V2,
            })
            .with_authorized_signer(wallet.access_key)
            .with_channel_store(Arc::new(store))
            .with_default_deposit(DEFAULT_SESSION_DEPOSIT)
            .with_max_deposit(self.max_deposit)
            .with_top_up_amount(self.top_up_amount)
            .with_autoswap(autoswap);
        let mut management_headers = reqwest::header::HeaderMap::new();
        if let Some(api_key) = &self.mpp_api_key {
            management_headers.insert(
                reqwest::header::HeaderName::from_static("x-api-key"),
                reqwest::header::HeaderValue::from_str(api_key)?,
            );
        }
        let payment = NativeSession {
            session,
            client: reqwest::Client::new(),
            management_url: endpoint.to_string(),
            management_headers,
        };
        let provider = MultiProvider::new()
            .with(CappedChargeProvider {
                provider: charge,
                max_charge: self.egress_max_charge,
            })
            .with(payment.session.clone());
        let egress = MppEgress::start(provider, EgressPolicy::default())
            .await
            .wrap_err("failed to start the embedded MPP egress proxy")?;

        let listener = StdTcpListener::bind("127.0.0.1:0")
            .wrap_err("failed to bind the local MPP WebSocket adapter")?;
        listener
            .set_nonblocking(true)
            .wrap_err("failed to configure the local MPP WebSocket adapter")?;
        let address = listener
            .local_addr()
            .wrap_err("failed to read the local MPP WebSocket adapter address")?;
        let listener = TcpListener::from_std(listener)
            .wrap_err("failed to start the local MPP WebSocket adapter")?;
        let config = Arc::new(BridgeConfig {
            endpoint: self.mpp_websocket_url,
            api_key: self.mpp_api_key,
            payment,
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(serve(listener, config, shutdown_rx));
        Ok((
            format!("ws://{address}/v1/responses"),
            Some(MppAdapter {
                api_base_url,
                shutdown_tx: Some(shutdown_tx),
                task: Some(task),
                egress: Some(egress),
            }),
        ))
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

    fn accept_payment_header(&self) -> Option<String> {
        self.provider.accept_payment_header()
    }
}

fn payment_http_url(websocket_url: &str) -> Result<reqwest::Url> {
    let mut url =
        reqwest::Url::parse(websocket_url).wrap_err("Tempo Responses WebSocket URL is invalid")?;
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        scheme => return Err(eyre!("unsupported Tempo WebSocket URL scheme {scheme}")),
    };
    url.set_scheme(scheme)
        .map_err(|()| eyre!("failed to derive the Tempo payment bootstrap URL"))?;
    Ok(url)
}

fn openai_api_base_url(websocket_url: &str) -> Result<String> {
    let mut url = payment_http_url(websocket_url)?;
    let api_path = url
        .path()
        .strip_suffix("/responses")
        .filter(|path| !path.is_empty())
        .ok_or_else(|| eyre!("Tempo Responses WebSocket URL must end in /responses"))?
        .to_owned();
    url.set_path(&api_path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

fn websocket_origin(websocket_url: &str) -> Result<String> {
    let url =
        reqwest::Url::parse(websocket_url).wrap_err("Tempo Responses WebSocket URL is invalid")?;
    match url.scheme() {
        "ws" | "wss" => Ok(url.origin().ascii_serialization()),
        scheme => Err(eyre!("unsupported Tempo WebSocket URL scheme {scheme}")),
    }
}

pub(crate) struct MppAdapter {
    api_base_url: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
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

    pub(crate) fn tool_http_client(&self) -> Result<reqwest::Client> {
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
        reqwest::Client::builder()
            .proxy(proxy)
            .add_root_certificate(certificate)
            .build()
            .wrap_err("failed to configure the MPP-aware tool HTTP client")
    }

    pub(crate) async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        let websocket_result = match self.task.take() {
            Some(mut task) => match timeout(Duration::from_secs(30), &mut task).await {
                Ok(Ok(completed)) => completed.wrap_err("MPP WebSocket adapter failed"),
                Ok(Err(error)) => Err(error).wrap_err("MPP WebSocket adapter task failed"),
                Err(error) => {
                    task.abort();
                    Err(error).wrap_err("timed out closing the paid MPP session")
                }
            },
            None => Err(eyre!("MPP WebSocket adapter task is missing")),
        };
        let egress_result = if let Some(egress) = self.egress.take() {
            egress
                .shutdown()
                .await
                .wrap_err("failed to stop the embedded MPP egress proxy")
        } else {
            Ok(())
        };
        websocket_result?;
        egress_result
    }
}

impl Drop for MppAdapter {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        drop(self.egress.take());
    }
}

#[derive(Clone)]
struct NativeSession {
    session: TempoSessionProvider,
    client: reqwest::Client,
    management_url: String,
    management_headers: reqwest::header::HeaderMap,
}

impl PaymentProvider for NativeSession {
    fn supports(&self, method: &str, intent: &str) -> bool {
        self.session.supports(method, intent)
    }

    async fn pay(&self, challenge: &PaymentChallenge) -> Result<PaymentCredential, MppError> {
        self.session
            .application_websocket_credential_with_top_up(
                &self.client,
                &self.management_url,
                self.management_headers.clone(),
                challenge,
            )
            .await
    }

    async fn pay_with_context(
        &self,
        challenge: &PaymentChallenge,
        context: PaymentContext,
    ) -> Result<PaymentCredential, MppError> {
        self.session
            .application_websocket_credential_with_top_up(
                &self.client,
                context.url.as_str(),
                context.headers,
                challenge,
            )
            .await
    }

    async fn prepare_application_websocket_challenge(
        &self,
        challenge: &PaymentChallenge,
        context: PaymentContext,
    ) -> Result<PaymentChallenge, MppError> {
        self.session
            .recover_application_websocket_challenge_with_headers(
                &self.client,
                context.url.as_str(),
                context.headers,
                challenge,
            )
            .await
    }

    fn accept_payment_header(&self) -> Option<String> {
        self.session.accept_payment_header()
    }
}

impl VoucherProvider for NativeSession {
    async fn next_voucher(&self, request: &VoucherRequest) -> Result<PaymentCredential, MppError> {
        let cumulative = request.required_cumulative.parse().map_err(|error| {
            MppError::InvalidConfig(format!(
                "invalid required cumulative voucher amount: {error}"
            ))
        })?;
        let deposit = request.deposit.parse().map_err(|error| {
            MppError::InvalidConfig(format!("invalid channel deposit amount: {error}"))
        })?;
        self.session
            .voucher_credential_with_top_up(
                &self.client,
                &self.management_url,
                self.management_headers.clone(),
                &request.channel_id,
                cumulative,
                deposit,
            )
            .await
    }

    async fn next_voucher_for_challenge(
        &self,
        challenge: &PaymentChallenge,
        request: &VoucherRequest,
    ) -> Result<PaymentCredential, MppError> {
        let cumulative = request.required_cumulative.parse().map_err(|error| {
            MppError::InvalidConfig(format!(
                "invalid required cumulative voucher amount: {error}"
            ))
        })?;
        let deposit = request.deposit.parse().map_err(|error| {
            MppError::InvalidConfig(format!("invalid channel deposit amount: {error}"))
        })?;
        self.session
            .voucher_credential_with_top_up_for_challenge(
                &self.client,
                &self.management_url,
                self.management_headers.clone(),
                challenge,
                &request.channel_id,
                cumulative,
                deposit,
            )
            .await
    }
}

impl CloseProvider for NativeSession {
    async fn close_credential(
        &self,
        request: &CloseRequest,
    ) -> Result<PaymentCredential, MppError> {
        let cumulative = request.cumulative_amount.parse().map_err(|error| {
            MppError::InvalidConfig(format!("invalid close-ready cumulative amount: {error}"))
        })?;
        self.session
            .close_credential_at(&request.channel_id, cumulative)
            .await
    }

    async fn close_credential_for_challenge(
        &self,
        challenge: &PaymentChallenge,
        request: &CloseRequest,
    ) -> Result<PaymentCredential, MppError> {
        let cumulative = request.cumulative_amount.parse().map_err(|error| {
            MppError::InvalidConfig(format!("invalid close-ready cumulative amount: {error}"))
        })?;
        self.session
            .close_credential_at_for_challenge(challenge, &request.channel_id, cumulative)
            .await
    }
}

struct BridgeConfig {
    endpoint: String,
    api_key: Option<String>,
    payment: NativeSession,
}

async fn serve(
    listener: TcpListener,
    config: Arc<BridgeConfig>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let mut bridges = JoinSet::new();
    let (bridge_shutdown_tx, _) = watch::channel(false);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let config = Arc::clone(&config);
                let bridge_shutdown = bridge_shutdown_tx.subscribe();
                bridges.spawn(async move { bridge(stream, &config, bridge_shutdown).await });
            }
            completed = bridges.join_next(), if !bridges.is_empty() => {
                record_bridge_result(completed);
            }
        }
    }
    let _ = bridge_shutdown_tx.send(true);
    while let Some(completed) = bridges.join_next().await {
        record_bridge_result(Some(completed));
    }
    Ok(())
}

fn record_bridge_result(
    completed: Option<std::result::Result<Result<()>, tokio::task::JoinError>>,
) {
    match completed {
        Some(Ok(Err(error))) => {
            tracing::warn!(error = ?error, "MPP WebSocket adapter closed");
        }
        Some(Err(error)) => {
            tracing::warn!(%error, "MPP WebSocket adapter task failed");
        }
        Some(Ok(Ok(()))) | None => {}
    }
}

#[expect(
    clippy::result_large_err,
    reason = "tungstenite fixes the handshake callback's rejection response type"
)]
async fn bridge(
    stream: TcpStream,
    config: &BridgeConfig,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let downstream_headers = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&downstream_headers);
    let downstream = accept_hdr_async(stream, move |request: &Request, response: Response| {
        if let Ok(mut headers) = captured.lock() {
            *headers = Some(request.headers().clone());
        }
        Ok(response)
    })
    .await
    .wrap_err("local Responses WebSocket handshake failed")?;
    let headers = downstream_headers
        .lock()
        .map_err(|_| eyre!("local Responses WebSocket header capture was poisoned"))?
        .take()
        .ok_or_else(|| eyre!("local Responses WebSocket headers were not captured"))?;

    let mut connector = MppApplicationWsConnect::new(
        &config.endpoint,
        config.payment.clone(),
        config.payment.clone(),
    );
    for name in [
        "openai-beta",
        "x-openai-internal-codex-responses-lite",
        "session-id",
        "thread-id",
        "x-client-request-id",
        "x-responsesapi-include-timing-metrics",
        "user-agent",
    ] {
        if let Some(value) = headers.get(name) {
            connector = connector.with_header(
                HeaderName::from_static(name),
                HeaderValue::from_bytes(value.as_bytes())?,
            );
        }
    }
    if let Some(api_key) = &config.api_key {
        connector = connector.with_header(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_str(api_key)?,
        );
    }
    let upstream = connector
        .connect()
        .await
        .wrap_err("failed to open the paid MPP WebSocket")?;
    relay(downstream, upstream, shutdown).await
}

async fn relay(
    mut downstream: WebSocketStream<TcpStream>,
    mut upstream: MppApplicationWs<NativeSession>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let relay_result = loop {
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            inbound = downstream.next() => match inbound {
                Some(Ok(Message::Text(text))) => {
                    if let Err(error) = upstream.send(text.to_string()).await {
                        return Err(error.into());
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if let Err(error) = downstream.send(Message::Pong(payload)).await {
                        if *shutdown.borrow() {
                            break Ok(());
                        }
                        break Err(error.into());
                    }
                }
                Some(Ok(Message::Close(_))) | None => break Ok(()),
                Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Binary(_))) => {
                    break Err(eyre!("Responses WebSocket sent a binary frame"));
                }
                Some(Err(error)) => {
                    if *shutdown.borrow() {
                        break Ok(());
                    }
                    break Err(error.into());
                }
            },
            outbound = upstream.next() => {
                let text = outbound.wrap_err("paid MPP WebSocket receive failed")?;
                if let Err(error) = downstream.send(Message::Text(text.into())).await {
                    if *shutdown.borrow() {
                        break Ok(());
                    }
                    break Err(error.into());
                }
            }
        }
    };
    upstream
        .disconnect()
        .await
        .wrap_err("failed to disconnect the paid MPP WebSocket")?;
    relay_result
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mpp::{Base64UrlJson, PaymentPayload};

    use super::*;

    #[derive(Clone, Default)]
    struct MockChargeProvider {
        payments: Arc<AtomicUsize>,
    }

    impl PaymentProvider for MockChargeProvider {
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
    }

    fn charge_challenge(amount: &str) -> PaymentChallenge {
        PaymentChallenge::new(
            "charge-id",
            "service.example",
            "tempo",
            "charge",
            Base64UrlJson::from_value(&serde_json::json!({
                "amount": amount,
                "currency": "0x20c000000000000000000000b9537d11c60e8b50"
            }))
            .unwrap(),
        )
    }

    fn args(enabled: bool) -> MppArgs {
        MppArgs {
            openai: false,
            enabled,
            mpp_websocket_url: DEFAULT_MPP_WEBSOCKET_URL.to_owned(),
            wallet_store: None,
            channel_store: None,
            rpc_url: DEFAULT_TEMPO_RPC_URL.to_owned(),
            pay_with: DEFAULT_TEMPO_PAY_WITH.to_owned(),
            swap_slippage_bps: DEFAULT_TEMPO_SWAP_SLIPPAGE_BPS,
            max_deposit: DEFAULT_MAX_DEPOSIT,
            top_up_amount: DEFAULT_TOP_UP_AMOUNT,
            egress_max_charge: DEFAULT_MAX_EGRESS_CHARGE,
            mpp_api_key: None,
        }
    }

    #[tokio::test]
    async fn mpp_is_opt_in() {
        let (url, adapter) = args(false)
            .start("wss://api.openai.com/v1/responses".to_owned())
            .await
            .unwrap();
        assert_eq!(url, "wss://api.openai.com/v1/responses");
        assert!(adapter.is_none());
    }

    #[tokio::test]
    async fn egress_charge_cap_is_checked_before_payment() {
        let inner = MockChargeProvider::default();
        let payments = Arc::clone(&inner.payments);
        let provider = CappedChargeProvider {
            provider: inner,
            max_charge: 100,
        };

        provider.pay(&charge_challenge("100")).await.unwrap();
        let error = provider.pay(&charge_challenge("101")).await.unwrap_err();

        assert!(matches!(error, MppError::AmountExceedsMax { .. }));
        assert_eq!(payments.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn derives_payment_bootstrap_url() {
        let url = payment_http_url("wss://openai.mpp.tempo.xyz/v1/responses").unwrap();
        assert_eq!(url.as_str(), "https://openai.mpp.tempo.xyz/v1/responses");
    }

    #[test]
    fn derives_openai_api_base_url() {
        let url = openai_api_base_url("wss://openai.mpp.tempo.xyz/v1/responses").unwrap();
        assert_eq!(url, "https://openai.mpp.tempo.xyz/v1");
    }

    #[test]
    fn rejects_non_responses_mpp_endpoint() {
        let error =
            openai_api_base_url("wss://openai.mpp.tempo.xyz/v1/chat/completions").unwrap_err();
        assert!(error.to_string().contains("must end in /responses"));
    }

    #[test]
    fn preserves_mppx_websocket_namespace() {
        let namespace = websocket_origin("wss://openai.mpp.tempo.xyz/v1/responses").unwrap();
        assert_eq!(namespace, "wss://openai.mpp.tempo.xyz");
    }
}
