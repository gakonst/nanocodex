use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use reqwest::Url;
use serde::Deserialize;
use sha2_11::Sha256;

use nanousd::CreditPackage;

#[derive(Clone)]
pub(crate) struct StripeClient {
    http: reqwest::Client,
    api_url: Url,
    secret_key: String,
    api_version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CheckoutSession {
    pub id: String,
    pub url: Option<String>,
    pub client_reference_id: Option<String>,
    pub payment_status: String,
    pub amount_total: Option<u64>,
    pub currency: Option<String>,
    pub payment_intent: Option<String>,
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StripeEvent {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: StripeEventData,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StripeEventData {
    pub object: StripeEventObject,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StripeEventObject {
    pub id: String,
}

impl StripeClient {
    pub fn new(
        api_url: &str,
        secret_key: String,
        api_version: Option<String>,
    ) -> Result<Self, StripeError> {
        nanousd::install_default_rustls_crypto_provider();
        let api_url = Url::parse(api_url)?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            http,
            api_url,
            secret_key,
            api_version,
        })
    }

    pub async fn create_checkout(
        &self,
        order_id: &str,
        package: CreditPackage,
        public_url: &str,
    ) -> Result<CheckoutSession, StripeError> {
        let success_url = format!(
            "{}/v1/credits/checkout/complete?order_id={order_id}&session_id={{CHECKOUT_SESSION_ID}}",
            public_url.trim_end_matches('/')
        );
        let cancel_url = format!(
            "{}/v1/credits/checkout/cancelled?order_id={order_id}",
            public_url.trim_end_matches('/')
        );
        let amount = package.usd_cents.to_string();
        let form = [
            ("mode", "payment"),
            ("payment_method_types[0]", "card"),
            ("line_items[0][price_data][currency]", "usd"),
            ("line_items[0][price_data][unit_amount]", &amount),
            (
                "line_items[0][price_data][product_data][name]",
                "Nanocodex credits",
            ),
            (
                "line_items[0][price_data][product_data][description]",
                "Closed-loop NANOUSD credits for Nanocodex services",
            ),
            ("line_items[0][quantity]", "1"),
            ("client_reference_id", order_id),
            ("metadata[order_id]", order_id),
            ("success_url", &success_url),
            ("cancel_url", &cancel_url),
        ];
        let request = self
            .request(reqwest::Method::POST, "v1/checkout/sessions")?
            .header("Idempotency-Key", format!("nanousd-order-{order_id}"))
            .form(&form);
        self.send(request).await
    }

    pub async fn checkout(&self, session_id: &str) -> Result<CheckoutSession, StripeError> {
        let request = self.request(
            reqwest::Method::GET,
            &format!("v1/checkout/sessions/{session_id}"),
        )?;
        self.send(request).await
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, StripeError> {
        let url = self.api_url.join(path)?;
        let mut request = self.http.request(method, url).bearer_auth(&self.secret_key);
        if let Some(version) = &self.api_version {
            request = request.header("Stripe-Version", version);
        }
        Ok(request)
    }

    async fn send<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, StripeError> {
        let response = request.send().await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            return Err(StripeError::Api {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        serde_json::from_slice(&body).map_err(Into::into)
    }
}

pub(crate) fn verify_webhook(
    body: &[u8],
    signature_header: &str,
    secret: &str,
    tolerance_seconds: u64,
) -> Result<StripeEvent, StripeError> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for field in signature_header.split(',') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key.trim() {
            "t" => timestamp = value.parse::<u64>().ok(),
            "v1" => signatures.push(hex::decode(value)?),
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(StripeError::InvalidSignature)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if now.abs_diff(timestamp) > tolerance_seconds {
        return Err(StripeError::StaleSignature);
    }
    let mut signed = timestamp.to_string().into_bytes();
    signed.push(b'.');
    signed.extend_from_slice(body);
    let valid = signatures.iter().any(|signature| {
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).is_ok_and(|mut mac| {
            mac.update(&signed);
            mac.verify_slice(signature).is_ok()
        })
    });
    if !valid {
        return Err(StripeError::InvalidSignature);
    }
    serde_json::from_slice(body).map_err(Into::into)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StripeError {
    #[error("invalid Stripe URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("Stripe request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Stripe returned invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Stripe returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("Stripe signature contains invalid hexadecimal data")]
    Hex(#[from] hex::FromHexError),
    #[error("Stripe signature is invalid")]
    InvalidSignature,
    #[error("Stripe signature timestamp is outside the allowed tolerance")]
    StaleSignature,
    #[error("system clock is before the Unix epoch")]
    Clock(#[from] std::time::SystemTimeError),
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::{Form, Json, Router, http::HeaderMap, routing::post};

    use super::*;

    async fn fake_checkout(
        headers: HeaderMap,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer sk_test_nanousd"
        );
        assert_eq!(
            headers.get("idempotency-key").unwrap(),
            "nanousd-order-ord_test"
        );
        assert_eq!(form.get("mode").unwrap(), "payment");
        assert_eq!(
            form.get("line_items[0][price_data][unit_amount]").unwrap(),
            "500"
        );
        assert_eq!(form.get("metadata[order_id]").unwrap(), "ord_test");
        assert!(
            form.get("success_url")
                .unwrap()
                .contains("{CHECKOUT_SESSION_ID}")
        );
        Json(serde_json::json!({
            "id": "cs_test",
            "url": "https://checkout.stripe.test/c/pay/cs_test",
            "client_reference_id": "ord_test",
            "payment_status": "unpaid",
            "amount_total": 500,
            "currency": "usd",
            "payment_intent": null,
            "metadata": {"order_id": "ord_test"}
        }))
    }

    #[tokio::test]
    async fn creates_server_priced_hosted_checkout_idempotently() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/checkout/sessions", post(fake_checkout)),
            )
            .await
            .unwrap();
        });
        let stripe = StripeClient::new(
            &format!("http://{address}"),
            "sk_test_nanousd".to_owned(),
            None,
        )
        .unwrap();

        let checkout = stripe
            .create_checkout(
                "ord_test",
                CreditPackage::from_cents(500),
                "https://credits.nanocodex.test",
            )
            .await
            .unwrap();
        assert_eq!(checkout.id, "cs_test");
        assert_eq!(checkout.payment_status, "unpaid");
        server.abort();
    }

    #[test]
    fn verifies_signed_payload_and_rejects_tampering() {
        let body = br#"{"id":"evt_1","type":"checkout.session.completed","data":{"object":{"id":"cs_1"}}}"#;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut signed = format!("{timestamp}.").into_bytes();
        signed.extend_from_slice(body);
        let mut mac = Hmac::<Sha256>::new_from_slice(b"whsec_test").unwrap();
        mac.update(&signed);
        let header = format!(
            "t={timestamp},v1={}",
            hex::encode(mac.finalize().into_bytes())
        );

        let event = verify_webhook(body, &header, "whsec_test", 300).unwrap();
        assert_eq!(event.id, "evt_1");
        assert!(verify_webhook(b"{}", &header, "whsec_test", 300).is_err());
    }
}
