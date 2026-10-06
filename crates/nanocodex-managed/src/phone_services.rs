//! Standalone phone intents and private inbox reads. No operation approves an intent.
#![allow(missing_docs)]
use crate::{ManagedClient, ManagedError};
use reqwest::Method;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhoneCountry {
    US,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneNumberStatus {
    Active,
    ReleasePending,
    Released,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneNumberType {
    Local,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneRequestKind {
    Purchase,
    Release,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneRequestStatus {
    PendingApproval,
    Complete,
    Denied,
    Expired,
    Failed,
    OutcomeUnknown,
}
#[derive(Serialize, Deserialize)]
pub struct PhoneNumber {
    pub id: String,
    pub phone_number: String,
    pub country: PhoneCountry,
    pub status: PhoneNumberStatus,
    pub created_at: String,
}
#[derive(Serialize, Deserialize)]
pub struct AvailablePhoneNumber {
    pub phone_number: String,
    pub country: PhoneCountry,
    #[serde(rename = "type")]
    pub number_type: PhoneNumberType,
}
/// Prices remain decimal strings, avoiding floating-point rounding.
#[derive(Serialize, Deserialize)]
pub struct PhoneQuote {
    pub id: String,
    pub currency: String,
    pub monthly_price: String,
    pub inbound_sms_price: String,
    pub recurring: bool,
    pub expires_at: String,
}
#[derive(Serialize, Deserialize)]
pub struct PhoneRequest {
    pub operation_id: String,
    pub approval_request_id: Option<String>,
    pub kind: PhoneRequestKind,
    pub status: PhoneRequestStatus,
    pub phone_number: String,
    pub number_id: Option<String>,
    pub quote: Option<PhoneQuote>,
    pub created_at: String,
    pub error: Option<String>,
}
/// Private, untrusted SMS content; never log or render in agent diagnostics.
/// Deliberately does not implement `Debug`.
#[derive(Serialize, Deserialize)]
pub struct PhoneMessage {
    pub id: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub received_at: String,
    pub expires_at: String,
}
#[derive(Serialize)]
pub struct PhoneProvision {
    pub operation_id: String,
    pub phone_number: String,
    pub country: PhoneCountry,
}
#[derive(Serialize)]
pub struct PhoneRelease {
    pub operation_id: String,
}
#[derive(Default)]
pub struct PhoneAvailableQuery<'a> {
    pub country: Option<PhoneCountry>,
    pub area_code: Option<&'a str>,
    pub limit: Option<u8>,
}
#[derive(Default)]
pub struct PhoneMessagesQuery<'a> {
    pub cursor: Option<&'a str>,
    pub limit: Option<u8>,
}
#[derive(Serialize, Deserialize)]
pub struct PhoneAvailable {
    pub numbers: Vec<AvailablePhoneNumber>,
}
#[derive(Serialize, Deserialize)]
pub struct PhoneNumbers {
    pub numbers: Vec<PhoneNumber>,
}
#[derive(Serialize, Deserialize)]
pub struct PhoneNumberReceipt {
    pub number: PhoneNumber,
}
#[derive(Serialize, Deserialize)]
pub struct PhoneRequestReceipt {
    pub request: PhoneRequest,
}
/// Private inbox page, intentionally without `Debug`.
#[derive(Serialize, Deserialize)]
pub struct PhoneMessages {
    pub messages: Vec<PhoneMessage>,
    pub next_cursor: Option<String>,
}
fn invalid() -> ManagedError {
    ManagedError::InvalidResponse("invalid phone service request")
}
// Match the service's canonical lowercase, RFC variant, version 1–8 UUID contract.
fn validate_uuid(value: &str) -> Result<(), ManagedError> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| invalid())?;
    if id.hyphenated().to_string() != value
        || !(1..=8).contains(&id.get_version_num())
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(invalid());
    }
    Ok(())
}
fn query_path(path: &str, mut query: url::form_urlencoded::Serializer<'_, String>) -> String {
    let query = query.finish();
    if query.is_empty() {
        path.into()
    } else {
        format!("{path}?{query}")
    }
}
impl ManagedClient {
    /// Search US local numbers. This does not reserve or purchase a number.
    pub async fn services_phone_available(
        &self,
        options: PhoneAvailableQuery<'_>,
    ) -> Result<PhoneAvailable, ManagedError> {
        if options.limit.is_some_and(|n| !(1..=20).contains(&n))
            || options.area_code.is_some_and(|s| {
                s.len() != 3
                    || !s.bytes().all(|b| b.is_ascii_digit())
                    || !(b'2'..=b'9').contains(&s.as_bytes()[0])
            })
        {
            return Err(invalid());
        }
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        if options.country.is_some() {
            q.append_pair("country", "US");
        }
        if let Some(area) = options.area_code {
            q.append_pair("area_code", area);
        }
        if let Some(limit) = options.limit {
            q.append_pair("limit", &limit.to_string());
        }
        self.connector_request(
            Method::GET,
            &query_path("v1/services/phone/numbers/available", q),
            None,
        )
        .await
    }
    pub async fn services_phone_list(&self) -> Result<PhoneNumbers, ManagedError> {
        self.connector_request(Method::GET, "v1/services/phone/numbers", None)
            .await
    }
    /// Submit one purchase intent, without retries. Retain the UUID before calling;
    /// reconcile an uncertain response with `services_phone_request_get`.
    /// Obtain human approval through `services_link` with service `phone`.
    pub async fn services_phone_provision(
        &self,
        input: &PhoneProvision,
    ) -> Result<PhoneRequestReceipt, ManagedError> {
        validate_uuid(&input.operation_id)?;
        let n = input.phone_number.as_bytes();
        if n.len() != 12
            || !n.starts_with(b"+1")
            || !(b'2'..=b'9').contains(&n[2])
            || !n[3..].iter().all(u8::is_ascii_digit)
        {
            return Err(invalid());
        }
        self.connector_request(
            Method::POST,
            "v1/services/phone/numbers",
            Some(serde_json::to_value(input).map_err(|_| invalid())?),
        )
        .await
    }
    pub async fn services_phone_get(&self, id: &str) -> Result<PhoneNumberReceipt, ManagedError> {
        validate_uuid(id)?;
        self.connector_request(
            Method::GET,
            &format!("v1/services/phone/numbers/{id}"),
            None,
        )
        .await
    }
    /// Submit one release intent. Release still requires hosted human approval.
    /// Never automatically retry an uncertain mutation with a fresh UUID.
    pub async fn services_phone_release(
        &self,
        id: &str,
        input: &PhoneRelease,
    ) -> Result<PhoneRequestReceipt, ManagedError> {
        validate_uuid(id)?;
        validate_uuid(&input.operation_id)?;
        self.connector_request(
            Method::DELETE,
            &format!("v1/services/phone/numbers/{id}"),
            Some(serde_json::to_value(input).map_err(|_| invalid())?),
        )
        .await
    }
    /// Read one private inbox page. Pass `next_cursor` unchanged to continue.
    pub async fn services_phone_messages(
        &self,
        id: &str,
        options: PhoneMessagesQuery<'_>,
    ) -> Result<PhoneMessages, ManagedError> {
        validate_uuid(id)?;
        if options.limit.is_some_and(|n| !(1..=50).contains(&n)) {
            return Err(invalid());
        }
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        if let Some(cursor) = options.cursor {
            q.append_pair("cursor", cursor);
        }
        if let Some(limit) = options.limit {
            q.append_pair("limit", &limit.to_string());
        }
        self.connector_request(
            Method::GET,
            &query_path(&format!("v1/services/phone/numbers/{id}/messages"), q),
            None,
        )
        .await
    }
    /// Poll the original caller UUID once; no automatic polling or approval.
    pub async fn services_phone_request_get(
        &self,
        operation_id: &str,
    ) -> Result<PhoneRequestReceipt, ManagedError> {
        validate_uuid(operation_id)?;
        self.connector_request(
            Method::GET,
            &format!("v1/services/phone/requests/{operation_id}"),
            None,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        extract::{Path, State},
        http::{HeaderMap, StatusCode, Uri},
        response::{IntoResponse, Response},
        routing::get,
    };
    use serde_json::{Value, json};
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    const OP: &str = "11111111-1111-4111-8111-111111111111";
    const LOST: &str = "22222222-2222-4222-8222-222222222222";
    const RELEASE: &str = "33333333-3333-4333-8333-333333333333";
    const NUMBER: &str = "44444444-4444-4444-8444-444444444444";
    const CURSOR: &str = "opaque+/cursor==";
    #[derive(Default)]
    struct Backend {
        writes: usize,
        requests: HashMap<String, Value>,
    }
    type Shared = Arc<Mutex<Backend>>;
    fn receipt(id: &str, kind: &str) -> Value {
        json!({"operation_id": id, "approval_request_id": OP, "kind":kind, "status":"pending_approval", "phone_number":"+14155550123", "created_at":"2026-10-06T00:00:00Z", "quote":{"id":"quote", "currency":"usd", "monthly_price":"1.1500", "inbound_sms_price":"0.00830", "recurring":true, "expires_at":"2026-10-06T00:10:00Z"}})
    }
    fn number() -> Value {
        json!({"id":NUMBER,"phone_number":"+14155550123","country":"US","status":"active","created_at":"2026-10-06T00:00:00Z","provider_sid":"discard-provider-secret"})
    }
    async fn provision(
        State(state): State<Shared>,
        headers: HeaderMap,
        Json(input): Json<Value>,
    ) -> Response {
        assert!(headers.contains_key("authorization"));
        assert!(!headers.contains_key("x-nanocodex-phone-human-approval"));
        assert_eq!(input["country"], "US");
        let id = input["operation_id"].as_str().unwrap();
        let request = receipt(id, "purchase");
        {
            let mut state = state.lock().unwrap();
            state.writes += 1;
            state.requests.insert(id.into(), request.clone());
        }
        if id == LOST {
            // The mutation is committed before its response stream is lost.
            let stream = futures_util::stream::once(async {
                Err::<String, _>(std::io::Error::other("synthetic lost response"))
            });
            return Response::new(Body::from_stream(stream));
        }
        Json(json!({"request":request})).into_response()
    }
    async fn poll(State(state): State<Shared>, Path(id): Path<String>) -> Json<Value> {
        Json(json!({"request":state.lock().unwrap().requests.get(&id).unwrap()}))
    }
    #[tokio::test]
    async fn phone_services_http_intent_link_poll_inbox_and_lost_write() {
        let state = Shared::default();
        let app = Router::new()
            .route("/v1/services/phone/numbers/available", get(|uri: Uri| async move {
                assert_eq!(uri.query(), Some("country=US&area_code=415&limit=2"));
                Json(json!({"numbers":[{"phone_number":"+14155550123","country":"US","type":"local"}]}))
            }))
            .route("/v1/services/phone/numbers", get(|| async {Json(json!({"numbers":[number()]}))}).post(provision))
            .route("/v1/services/phone/numbers/{id}", get(|Path(id): Path<String>| async move {
                assert_eq!(id, NUMBER); Json(json!({"number":number()}))
            }).delete(|State(state): State<Shared>, Path(id): Path<String>, headers: HeaderMap, Json(input): Json<Value>| async move {
                assert_eq!(id, NUMBER);
                assert!(!headers.contains_key("x-nanocodex-phone-human-approval"));
                let request = receipt(input["operation_id"].as_str().unwrap(), "release");
                let mut state = state.lock().unwrap(); state.writes += 1;
                state.requests.insert(RELEASE.into(), request.clone());
                Json(json!({"request":request}))
            }))
            .route("/v1/services/phone/requests/{id}", get(poll))
            .route("/v1/services/links", get(|uri: Uri| async move {
                assert_eq!(uri.query(), Some(format!("service=phone&operation_id={OP}").as_str()));
                Json(json!({"url":format!("https://account.example/services/phone?operation_id={OP}"),"service":"phone","operationId":OP}))
            }))
            .route("/v1/services/phone/numbers/{id}/messages", get(|Path(id): Path<String>, uri: Uri| async move {
                assert_eq!(id, NUMBER);
                if uri.query() == Some("limit=1") {
                    Json(json!({"messages":[{"id":"SMsynthetic","from":"+14155550124","to":"+14155550123","body":"Private synthetic message ✓\nSecond line\twith a tab","received_at":"2026-10-06T00:01:00Z","expires_at":"2026-10-07T00:01:00Z"}],"next_cursor":CURSOR})).into_response()
                } else if uri.query() == Some("cursor=opaque%2B%2Fcursor%3D%3D&limit=1") {
                    Json(json!({"messages":[],"next_cursor":null})).into_response()
                } else {
                    (StatusCode::FORBIDDEN, "Private synthetic error body").into_response()
                }
            })).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key =
            crate::ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap();
        let client = ManagedClient::new(format!("http://{address}"), key).unwrap();
        let available = client
            .services_phone_available(PhoneAvailableQuery {
                country: Some(PhoneCountry::US),
                area_code: Some("415"),
                limit: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(available.numbers.len(), 1);
        let input = PhoneProvision {
            operation_id: OP.into(),
            phone_number: available.numbers[0].phone_number.clone(),
            country: PhoneCountry::US,
        };
        let pending = client
            .services_phone_provision(&input)
            .await
            .unwrap()
            .request;
        assert!(pending.status == PhoneRequestStatus::PendingApproval);
        let quote = pending.quote.unwrap();
        assert_eq!(quote.monthly_price, "1.1500");
        assert_eq!(quote.inbound_sms_price, "0.00830");
        assert!(quote.recurring);
        let link = client
            .services_link(crate::ServiceLinkOptions {
                service: Some("phone"),
                operation_id: Some(OP),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(link.url.contains(OP));
        assert!(
            client
                .services_phone_request_get(OP)
                .await
                .unwrap()
                .request
                .status
                == PhoneRequestStatus::PendingApproval
        );
        // Only the fixture's simulated external human approval changes status.
        state.lock().unwrap().requests.get_mut(OP).unwrap()["status"] = json!("complete");
        assert!(
            client
                .services_phone_request_get(OP)
                .await
                .unwrap()
                .request
                .status
                == PhoneRequestStatus::Complete
        );
        assert_eq!(client.services_phone_list().await.unwrap().numbers.len(), 1);
        let n = client.services_phone_get(NUMBER).await.unwrap();
        assert!(n.number.status == PhoneNumberStatus::Active);
        assert!(
            !serde_json::to_string(&n)
                .unwrap()
                .contains("discard-provider-secret")
        );
        let page = client
            .services_phone_messages(
                NUMBER,
                PhoneMessagesQuery {
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            page.messages[0].body,
            "Private synthetic message ✓\nSecond line\twith a tab"
        );
        assert_eq!(page.messages[0].expires_at, "2026-10-07T00:01:00Z");
        let page2 = client
            .services_phone_messages(
                NUMBER,
                PhoneMessagesQuery {
                    cursor: page.next_cursor.as_deref(),
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        assert!(page2.messages.is_empty() && page2.next_cursor.is_none());
        let error = client
            .services_phone_messages(NUMBER, PhoneMessagesQuery::default())
            .await
            .err()
            .unwrap();
        assert!(!format!("{error:?}").contains("Private synthetic"));
        let lost = PhoneProvision {
            operation_id: LOST.into(),
            ..input
        };
        assert!(client.services_phone_provision(&lost).await.is_err());
        assert_eq!(state.lock().unwrap().writes, 2);
        assert_eq!(
            client
                .services_phone_request_get(LOST)
                .await
                .unwrap()
                .request
                .operation_id,
            LOST
        );
        assert_eq!(state.lock().unwrap().writes, 2);
        let release = client
            .services_phone_release(
                NUMBER,
                &PhoneRelease {
                    operation_id: RELEASE.into(),
                },
            )
            .await
            .unwrap();
        assert!(
            release.request.kind == PhoneRequestKind::Release
                && release.request.status == PhoneRequestStatus::PendingApproval
        );
        assert_eq!(state.lock().unwrap().writes, 3);
        for bad in [
            "",
            "../approve",
            "11111111111141118111111111111111",
            "00000000-0000-0000-0000-000000000000",
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
        ] {
            assert!(
                client
                    .services_phone_provision(&PhoneProvision {
                        operation_id: bad.into(),
                        phone_number: "+14155550123".into(),
                        country: PhoneCountry::US
                    })
                    .await
                    .is_err()
            );
            assert!(client.services_phone_request_get(bad).await.is_err());
            assert!(
                client
                    .services_phone_release(
                        NUMBER,
                        &PhoneRelease {
                            operation_id: bad.into()
                        }
                    )
                    .await
                    .is_err()
            );
        }
        assert_eq!(state.lock().unwrap().writes, 3);
        server.abort();
    }
    #[test]
    fn phone_services_all_wire_statuses_preserved() {
        for status in [
            "pending_approval",
            "complete",
            "denied",
            "expired",
            "failed",
            "outcome_unknown",
        ] {
            let mut value = receipt(OP, "purchase");
            value["status"] = json!(status);
            let request: PhoneRequest = serde_json::from_value(value).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap()["status"], status);
        }
        for status in ["active", "release_pending", "released"] {
            let mut value = number();
            value["status"] = json!(status);
            let number: PhoneNumber = serde_json::from_value(value).unwrap();
            assert_eq!(serde_json::to_value(number).unwrap()["status"], status);
        }
    }
}
