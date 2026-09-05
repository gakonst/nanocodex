//! Shared wire contract and client for the Tempo-facing binaries.

use std::time::Duration;

use alloy_primitives::Address;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};

/// `NanoUSD` on Tempo mainnet.
pub const NANOUSD_ADDRESS: Address =
    alloy_primitives::address!("20C0000000000000000000008B4c619d2eedEc7A");
/// Tempo mainnet chain ID.
pub const TEMPO_MAINNET_CHAIN_ID: u64 = 4217;
/// `NanoUSD` uses six decimal places.
pub const NANOUSD_DECIMALS: u32 = 6;
/// Default local credits API used by the CLI and development server.
pub const DEFAULT_API_URL: &str = "http://127.0.0.1:8789";

/// Installs the workspace's ring-backed Rustls provider if the host has not
/// already selected a process-level provider.
#[doc(hidden)]
pub fn install_default_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        drop(rustls::crypto::ring::default_provider().install_default());
    }
}

/// A server-controlled credit package. One cent buys 10,000 atomic `NanoUSD` units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditPackage {
    pub usd_cents: u64,
    pub nanousd_units: u64,
}

impl CreditPackage {
    #[must_use]
    pub const fn from_cents(usd_cents: u64) -> Self {
        Self {
            usd_cents,
            nanousd_units: usd_cents * 10_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    pub wallet: Address,
    pub package_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrderResponse {
    pub order: Order,
    /// Capability used to read this order. It is returned only at creation.
    pub order_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub wallet: Address,
    pub package: CreditPackage,
    pub status: OrderStatus,
    pub checkout_url: Option<String>,
    pub transaction_hash: Option<String>,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Created,
    AwaitingPayment,
    Paid,
    Fulfilling,
    Fulfilled,
    Failed,
    Expired,
}

impl OrderStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Fulfilled | Self::Expired)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub wallet: Address,
    pub token: Address,
    pub chain_id: u64,
    pub nanousd_units: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub payment_mode: String,
    pub issuer_mode: String,
    pub token: Address,
    pub chain_id: u64,
    pub packages: Vec<CreditPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct CreditsClient {
    base_url: Url,
    http: reqwest::Client,
}

impl CreditsClient {
    /// Creates a client for a credits API endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid URL or the HTTP client
    /// cannot be constructed.
    pub fn new(base_url: &str) -> Result<Self, ClientError> {
        install_default_rustls_crypto_provider();
        let mut base_url = Url::parse(base_url).map_err(ClientError::Url)?;
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(ClientError::Http)?;
        Ok(Self { base_url, http })
    }

    /// Returns the service's public token and package configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the API request fails or its response is invalid.
    pub async fn info(&self) -> Result<ServiceInfo, ClientError> {
        self.send(self.http.get(self.url("v1/credits")?)).await
    }

    /// Returns one wallet's current credits balance.
    ///
    /// # Errors
    ///
    /// Returns an error when the API request fails or its response is invalid.
    pub async fn balance(&self, wallet: Address) -> Result<BalanceResponse, ClientError> {
        self.send(
            self.http
                .get(self.url(&format!("v1/credits/balance/{wallet}"))?),
        )
        .await
    }

    /// Creates a server-priced credits order.
    ///
    /// # Errors
    ///
    /// Returns an error when the API rejects the request or its response is invalid.
    pub async fn create_order(
        &self,
        request: &CreateOrderRequest,
    ) -> Result<CreateOrderResponse, ClientError> {
        self.send(self.http.post(self.url("v1/credits/orders")?).json(request))
            .await
    }

    /// Reads an order using its creation-time capability.
    ///
    /// # Errors
    ///
    /// Returns an error when the capability is rejected, the request fails, or
    /// the response is invalid.
    pub async fn order(&self, id: &str, token: &str) -> Result<Order, ClientError> {
        self.send(
            self.http
                .get(self.url(&format!("v1/credits/orders/{id}"))?)
                .bearer_auth(token),
        )
        .await
    }

    fn url(&self, path: &str) -> Result<Url, ClientError> {
        self.base_url.join(path).map_err(ClientError::Url)
    }

    async fn send<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ClientError> {
        let response = request.send().await.map_err(ClientError::Http)?;
        let status = response.status();
        let body = response.bytes().await.map_err(ClientError::Http)?;
        if status.is_success() {
            serde_json::from_slice(&body).map_err(ClientError::Decode)
        } else {
            let error = serde_json::from_slice::<ApiErrorBody>(&body).unwrap_or(ApiErrorBody {
                code: "http_error".to_owned(),
                message: String::from_utf8_lossy(&body).into_owned(),
            });
            Err(ClientError::Api { status, error })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid credits API URL: {0}")]
    Url(#[source] url::ParseError),
    #[error("credits API request failed: {0}")]
    Http(#[source] reqwest::Error),
    #[error("credits API returned invalid JSON: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("credits API returned {status}: {}", error.message)]
    Api {
        status: StatusCode,
        error: ApiErrorBody,
    },
}
