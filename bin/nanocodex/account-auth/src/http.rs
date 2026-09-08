use std::time::{Duration, Instant};

use reqwest::{
    Method,
    header::{AUTHORIZATION, COOKIE, HeaderMap, HeaderValue, SET_COOKIE},
};
use serde_json::{Value, json};

use crate::{Error, Result, validate_key};

const RESPONSE_LIMIT: usize = 16_384;

pub(crate) struct Session {
    client: reqwest::Client,
    origin: String,
    cookie: Option<String>,
    pub(crate) minted_key: Option<String>,
}

impl Drop for Session {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.cookie.zeroize();
        self.minted_key.zeroize();
    }
}

pub(crate) struct Challenge {
    phone: String,
    id: String,
    pub(crate) expires: Instant,
}

impl Session {
    pub(crate) fn new(origin: String) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| Error::message("Cannot start the account HTTP client"))?;
        Ok(Self {
            client,
            origin,
            cookie: None,
            minted_key: None,
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> Result<(Value, HeaderMap)> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("origin", &self.origin);
        if let Some(cookie) = &self.cookie {
            request = request.header(COOKIE, sensitive(cookie)?);
        }
        if let Some(key) = key {
            request = request.header(AUTHORIZATION, sensitive(&format!("Bearer {key}"))?);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request.send().await.map_err(|_| {
            Error::message("Cannot reach Nanocodex; check your connection and try again")
        })?;
        let status = response.status();
        let headers = response.headers().clone();
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::message("Could not read the account response"))?
        {
            if bytes.len() + chunk.len() > RESPONSE_LIMIT {
                return Err(Error::message("Account response is too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        if !status.is_success() {
            return Err(Error::response(status.as_u16(), &body, &headers));
        }
        if !body.is_object() {
            return Err(Error::message("Invalid account response"));
        }
        Ok((body, headers))
    }

    pub(crate) async fn start(&self, phone: String) -> Result<Challenge> {
        let (body, _) = self
            .request(
                Method::POST,
                "/v1/auth/sms/start",
                Some(json!({"phone": phone})),
                None,
            )
            .await?;
        let id = body["challenge_id"].as_str().filter(|id| token(id, 43));
        let expires = body["expires_in"]
            .as_u64()
            .filter(|value| (1..=3600).contains(value));
        let resend = body["resend_after"].as_u64().filter(|value| *value <= 3600);
        match (id, expires, resend) {
            (Some(id), Some(expires), Some(_)) => Ok(Challenge {
                phone,
                id: id.to_owned(),
                expires: Instant::now() + Duration::from_secs(expires),
            }),
            _ => Err(Error::message("Invalid SMS challenge response")),
        }
    }

    pub(crate) async fn verify(&mut self, challenge: &Challenge, code: &str) -> Result<()> {
        if Instant::now() >= challenge.expires {
            return Err(Error::message("Your code has expired; run login again"));
        }
        let (_, headers) = self
            .request(
                Method::POST,
                "/v1/auth/sms/verify",
                Some(json!({
                    "phone": challenge.phone, "challenge_id": challenge.id, "code": code,
                })),
                None,
            )
            .await?;
        self.cookie = headers
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .find(|value| {
                value
                    .strip_prefix("nanocodex_account=s_")
                    .is_some_and(|value| token(value, 43))
            })
            .map(str::to_owned);
        if self.cookie.is_none() {
            return Err(Error::message("Missing or invalid account session cookie"));
        }
        Ok(())
    }

    pub(crate) async fn mint(&mut self, label: &str) -> Result<()> {
        let (mut body, _) = self
            .request(
                Method::POST,
                "/v1/api-keys",
                Some(json!({"label": label})),
                None,
            )
            .await?;
        let key = body["api_key"]
            .take()
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::message("Missing account API key"))?;
        validate_key(&key)?;
        // Retain before cancellation is observed so the caller can revoke it.
        self.minted_key = Some(key);
        Ok(())
    }

    pub(crate) async fn identity(&self, key: &str) -> Result<Value> {
        let (body, _) = self.request(Method::GET, "/v1/me", None, Some(key)).await?;
        if body["authentication"] != "api_key" {
            return Err(Error::message("Invalid account identity response"));
        }
        let user = identity_id(&body["user"]["id"])?;
        let organization = identity_id(&body["organization"]["id"])?;
        let team = identity_id(&body["team"]["id"])?;
        let role = body["role"]
            .as_str()
            .filter(|role| matches!(*role, "owner" | "writer" | "reader"))
            .ok_or_else(|| Error::message("Invalid account role response"))?;
        Ok(json!({"user": user, "organization": organization, "team": team, "role": role}))
    }

    pub(crate) async fn finish(&mut self, saved: bool) -> Result<()> {
        let revoke = if !saved && let Some(key) = &self.minted_key {
            let id = &key[9..21];
            match self
                .request(Method::DELETE, &format!("/v1/api-keys/{id}"), None, None)
                .await
            {
                Err(error) if error.status == Some(404) => Ok(()),
                result => result.map(|_| ()),
            }
        } else {
            Ok(())
        };
        if self.cookie.is_some() {
            // A failed temporary-session logout must never undo a saved key.
            let _ = self
                .request(Method::POST, "/v1/auth/logout", Some(json!({})), None)
                .await;
        }
        revoke.map_err(|_| {
            Error::message(
                "Could not revoke the unused CLI key; remove it in the account API Keys menu",
            )
        })
    }
}

fn sensitive(value: &str) -> Result<HeaderValue> {
    let mut value =
        HeaderValue::from_str(value).map_err(|_| Error::message("Invalid account credential"))?;
    value.set_sensitive(true);
    Ok(value)
}

fn token(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn identity_id(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && !value.contains("ncx_live_")
                && !value.starts_with("s_")
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .ok_or_else(|| Error::message("Invalid account identity response"))
}
