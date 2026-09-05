use std::{str::FromStr, sync::Arc};

use alloy_primitives::Address;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
#[cfg(test)]
use clap::Parser;
use getrandom::fill;
use nanousd::{
    ApiErrorBody, BalanceResponse, CreateOrderRequest, CreateOrderResponse, CreditPackage, Order,
    OrderStatus, ServiceInfo, TEMPO_MAINNET_CHAIN_ID,
};
use serde::Deserialize;
use sha2_11::{Digest, Sha256};

use crate::{
    config::{Config, IssuerMode, PaymentMode},
    db::{Database, DbError},
    issuer::{AlloyIssuer, Issuer, IssuerError, MockIssuer},
    stripe::{StripeClient, StripeError, verify_webhook},
};

const PACKAGES: [u64; 4] = [500, 1000, 2500, 5000];
const WEBHOOK_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct AppState {
    config: Arc<Config>,
    database: Database,
    issuer: Arc<dyn Issuer>,
    stripe: Option<StripeClient>,
}

impl AppState {
    pub fn new(config: Config) -> Result<Self, AppError> {
        config.validate().map_err(AppError::Configuration)?;
        let database = Database::open(&config.database)?;
        let issuer: Arc<dyn Issuer> = match config.issuer_mode {
            IssuerMode::Mock => Arc::new(MockIssuer::default()),
            IssuerMode::Alloy => Arc::new(AlloyIssuer::new(
                &config.rpc_url,
                config.token,
                config.fee_token.parse().map_err(|error| {
                    AppError::Configuration(eyre::eyre!("invalid NANOUSD_FEE_TOKEN: {error}"))
                })?,
                config.wallet_store.as_deref(),
            )?),
        };
        let stripe = match config.payment_mode {
            PaymentMode::Mock => None,
            PaymentMode::Stripe => Some(StripeClient::new(
                &config.stripe_api_url,
                config
                    .stripe_secret_key
                    .clone()
                    .ok_or(AppError::MissingStripeConfiguration)?,
                config.stripe_api_version.clone(),
            )?),
        };
        Ok(Self {
            config: Arc::new(config),
            database,
            issuer,
            stripe,
        })
    }

    #[cfg(test)]
    fn test() -> Self {
        let mut config = Config::parse_from(["nanousd-api"]);
        config.database = ":memory:".into();
        let database = Database::memory().unwrap();
        Self {
            config: Arc::new(config),
            database,
            issuer: Arc::new(MockIssuer::default()),
            stripe: None,
        }
    }
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/credits", get(info))
        .route("/v1/credits/balance/{wallet}", get(balance))
        .route("/v1/credits/orders", post(create_order))
        .route("/v1/credits/orders/{id}", get(get_order))
        .route("/v1/stripe/webhook", post(stripe_webhook))
        .route("/v1/credits/checkout/complete", get(checkout_complete))
        .route("/v1/credits/checkout/cancelled", get(checkout_cancelled))
        .layer(DefaultBodyLimit::max(WEBHOOK_BODY_LIMIT))
        .with_state(state)
}

pub(crate) async fn fulfillment_worker(state: AppState) {
    let mut interval = tokio::time::interval(state.config.worker_interval());
    loop {
        interval.tick().await;
        let order = match state.database.claim_fulfillment() {
            Ok(Some(order)) => order,
            Ok(None) => continue,
            Err(error) => {
                tracing::error!(%error, "failed to claim NanoUSD fulfillment");
                continue;
            }
        };
        let result = async {
            let mint = state.issuer.prepare(&order).await?;
            state.database.save_prepared_transaction(
                &order.id,
                &mint.signed_transaction,
                &mint.transaction_hash,
                mint.valid_before,
            )?;
            state.issuer.publish(&order, &mint).await?;
            state
                .database
                .mark_fulfilled(&order.id, &mint.transaction_hash)?;
            Result::<_, AppError>::Ok(mint.transaction_hash)
        }
        .await;
        match result {
            Ok(transaction_hash) => tracing::info!(
                order_id = %order.id,
                %transaction_hash,
                "fulfilled NanoUSD order"
            ),
            Err(error) => {
                tracing::error!(order_id = %order.id, %error, "NanoUSD fulfillment failed");
                if let Err(database_error) =
                    state.database.mark_failed(&order.id, &error.to_string(), 5)
                {
                    tracing::error!(%database_error, "failed to persist fulfillment failure");
                }
            }
        }
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn info(State(state): State<AppState>) -> Json<ServiceInfo> {
    Json(ServiceInfo {
        payment_mode: format!("{:?}", state.config.payment_mode).to_lowercase(),
        issuer_mode: format!("{:?}", state.config.issuer_mode).to_lowercase(),
        token: state.config.token,
        chain_id: TEMPO_MAINNET_CHAIN_ID,
        packages: PACKAGES
            .into_iter()
            .map(CreditPackage::from_cents)
            .collect(),
    })
}

async fn balance(
    State(state): State<AppState>,
    Path(wallet): Path<String>,
) -> Result<Json<BalanceResponse>, AppError> {
    let wallet = Address::from_str(&wallet).map_err(|_| AppError::InvalidWallet)?;
    let nanousd_units = state.issuer.balance(wallet).await?;
    Ok(Json(BalanceResponse {
        wallet,
        token: state.config.token,
        chain_id: TEMPO_MAINNET_CHAIN_ID,
        nanousd_units,
    }))
}

async fn create_order(
    State(state): State<AppState>,
    Json(request): Json<CreateOrderRequest>,
) -> Result<(StatusCode, Json<CreateOrderResponse>), AppError> {
    if !PACKAGES.contains(&request.package_cents) {
        return Err(AppError::InvalidPackage);
    }
    let package = CreditPackage::from_cents(request.package_cents);
    let id = format!("ord_{}", uuid::Uuid::now_v7().simple());
    let mut token = [0_u8; 32];
    fill(&mut token).map_err(AppError::Random)?;
    let order_token = hex::encode(token);
    let token_hash = hash_token(&order_token);
    state.database.create_order(
        &id,
        &token_hash,
        request.wallet,
        package,
        OrderStatus::Created,
    )?;
    let order = match state.config.payment_mode {
        PaymentMode::Mock => {
            state.database.mark_mock_paid(&id)?;
            state.database.order(&id)?.ok_or(AppError::OrderNotFound)?
        }
        PaymentMode::Stripe => {
            let stripe = state
                .stripe
                .as_ref()
                .ok_or(AppError::MissingStripeConfiguration)?;
            let checkout = stripe
                .create_checkout(&id, package, &state.config.public_url)
                .await?;
            let checkout_url = checkout.url.ok_or(AppError::StripeCheckoutUrl)?;
            state
                .database
                .set_checkout(&id, &checkout.id, &checkout_url)?;
            state.database.order(&id)?.ok_or(AppError::OrderNotFound)?
        }
    };
    Ok((
        StatusCode::CREATED,
        Json(CreateOrderResponse { order, order_token }),
    ))
}

async fn get_order(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Order>, AppError> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    let order = state
        .database
        .order_authorized(&id, &hash_token(authorization))?
        .ok_or(AppError::OrderNotFound)?;
    Ok(Json(order))
}

async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    if state.config.payment_mode != PaymentMode::Stripe {
        return Err(AppError::StripeDisabled);
    }
    let signature = headers
        .get("stripe-signature")
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::InvalidStripeSignature)?;
    let event = verify_webhook(
        &body,
        signature,
        state
            .config
            .stripe_webhook_secret
            .as_deref()
            .ok_or(AppError::MissingStripeConfiguration)?,
        state.config.webhook_tolerance_seconds,
    )?;
    if !matches!(
        event.event_type.as_str(),
        "checkout.session.completed" | "checkout.session.async_payment_succeeded"
    ) {
        return Ok(StatusCode::OK);
    }
    let stripe = state
        .stripe
        .as_ref()
        .ok_or(AppError::MissingStripeConfiguration)?;
    let checkout = stripe.checkout(&event.data.object.id).await?;
    let order_id = checkout
        .metadata
        .get("order_id")
        .or(checkout.client_reference_id.as_ref())
        .ok_or(AppError::StripeOrderMismatch)?;
    let order = state
        .database
        .order(order_id)?
        .ok_or(AppError::StripeOrderMismatch)?;
    if checkout.payment_status != "paid"
        || checkout.amount_total != Some(order.package.usd_cents)
        || checkout.currency.as_deref() != Some("usd")
    {
        return Err(AppError::StripeOrderMismatch);
    }
    let _inserted = state.database.record_stripe_payment(
        &event.id,
        &event.event_type,
        &order.id,
        &checkout.id,
        checkout.payment_intent.as_deref(),
    )?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct CheckoutResult {
    order_id: String,
}

async fn checkout_complete(Query(query): Query<CheckoutResult>) -> Html<String> {
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>Nanocodex credits</title>\
         <main><h1>Payment received</h1><p>Order <code>{}</code> is being fulfilled.\
         You can close this window and return to Nanocodex.</p></main>",
        escape_html(&query.order_id)
    ))
}

async fn checkout_cancelled(Query(query): Query<CheckoutResult>) -> Html<String> {
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>Nanocodex credits</title>\
         <main><h1>Checkout cancelled</h1><p>Order <code>{}</code> was not paid.\
         You can close this window and return to Nanocodex.</p></main>",
        escape_html(&query.order_id)
    ))
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AppError {
    #[error("invalid service configuration: {0}")]
    Configuration(#[source] eyre::Report),
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Issuer(#[from] IssuerError),
    #[error(transparent)]
    Stripe(#[from] StripeError),
    #[error("Stripe mode is not configured")]
    MissingStripeConfiguration,
    #[error("Stripe Checkout Session omitted its hosted URL")]
    StripeCheckoutUrl,
    #[error("wallet must be a valid Tempo address")]
    InvalidWallet,
    #[error("package is not offered by this service")]
    InvalidPackage,
    #[error("order capability is missing or invalid")]
    Unauthorized,
    #[error("order was not found")]
    OrderNotFound,
    #[error("Stripe webhook endpoint is disabled in mock mode")]
    StripeDisabled,
    #[error("Stripe-Signature header is missing or invalid")]
    InvalidStripeSignature,
    #[error("Stripe Checkout Session does not match its NanoUSD order")]
    StripeOrderMismatch,
    #[error("operating system random generator failed: {0}")]
    Random(#[source] getrandom::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, public_message) = match &self {
            Self::InvalidWallet => (StatusCode::BAD_REQUEST, "invalid_wallet", self.to_string()),
            Self::InvalidPackage => (StatusCode::BAD_REQUEST, "invalid_package", self.to_string()),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", self.to_string()),
            Self::OrderNotFound => (StatusCode::NOT_FOUND, "order_not_found", self.to_string()),
            Self::StripeDisabled => (StatusCode::NOT_FOUND, "not_found", "not found".to_owned()),
            Self::InvalidStripeSignature | Self::Stripe(StripeError::InvalidSignature) => (
                StatusCode::BAD_REQUEST,
                "invalid_signature",
                "Stripe signature is invalid".to_owned(),
            ),
            Self::Stripe(StripeError::StaleSignature) => {
                (StatusCode::BAD_REQUEST, "stale_signature", self.to_string())
            }
            Self::StripeOrderMismatch => (StatusCode::CONFLICT, "order_mismatch", self.to_string()),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "internal service error".to_owned(),
            ),
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "NanoUSD API request failed");
        }
        (
            status,
            Json(ApiErrorBody {
                code: code.to_owned(),
                message: public_message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn mock_order_fulfills_once_and_updates_balance() {
        let state = AppState::test();
        let app = router(state.clone());
        let wallet = Address::repeat_byte(0x11);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/credits/orders")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&CreateOrderRequest {
                    wallet,
                    package_cents: 500,
                })
                .unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let created: CreateOrderResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(created.order.status, OrderStatus::Paid);

        let claimed = state.database.claim_fulfillment().unwrap().unwrap();
        let mint = state.issuer.prepare(&claimed).await.unwrap();
        state.issuer.publish(&claimed, &mint).await.unwrap();
        state
            .database
            .mark_fulfilled(&claimed.id, &mint.transaction_hash)
            .unwrap();
        assert_eq!(state.issuer.balance(wallet).await.unwrap(), 5_000_000);
        let order = state
            .database
            .order_authorized(&created.order.id, &hash_token(&created.order_token))
            .unwrap()
            .unwrap();
        assert_eq!(order.status, OrderStatus::Fulfilled);
    }

    #[tokio::test]
    async fn order_capability_is_required() {
        let app = router(AppState::test());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/credits/orders/ord_missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
