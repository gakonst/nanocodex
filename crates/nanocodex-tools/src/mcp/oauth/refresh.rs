//! Cancellation-safe, serialized MCP OAuth refresh transactions.

use std::{sync::Arc, time::Duration};

use oauth2::TokenResponse;
use rmcp::transport::{
    AuthError, AuthorizationManager, CredentialStore, InMemoryCredentialStore, StoredCredentials,
};
use tokio::time::timeout;

use super::{McpOAuthCredentials, OAuthRuntime, now_millis, now_seconds};

const REFRESH_SKEW_MILLIS: u64 = 30_000;
const REFRESH_LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const REFRESH_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

impl McpOAuthCredentials {
    fn needs_refresh(&self) -> bool {
        self.expires_at_millis.is_some_and(|expires_at| {
            now_millis().saturating_add(REFRESH_SKEW_MILLIS) >= expires_at
        })
    }

    fn from_refresh_response(
        client_id: String,
        response: &rmcp::transport::auth::OAuthTokenResponse,
        previous: &Self,
    ) -> Self {
        let mut credentials =
            Self::from_token_response(client_id, response, previous.issuer.clone());
        if response.refresh_token().is_none() {
            credentials
                .refresh_token
                .clone_from(&previous.refresh_token);
        }
        if response.scopes().is_none() {
            credentials.scopes.clone_from(&previous.scopes);
        }
        credentials
    }
}

impl OAuthRuntime {
    /// Refreshes known-expiring credentials before an MCP operation starts.
    ///
    /// The owned task continues through persistence if its caller is cancelled after a rotating
    /// refresh token may have been consumed.
    pub(crate) async fn refresh_if_needed(self: &Arc<Self>) -> Result<(), String> {
        let refresh_needed = self
            .last_credentials
            .lock()
            .await
            .as_ref()
            .is_none_or(McpOAuthCredentials::needs_refresh);
        if !refresh_needed {
            return Ok(());
        }

        let runtime = Arc::clone(self);
        let task = tokio::spawn(async move {
            let result = runtime.refresh_transaction().await;
            if let Err(error) = &result {
                tracing::warn!(
                    server = %runtime.server_name,
                    refresh_reason = "expiry",
                    %error,
                    "MCP OAuth refresh transaction failed"
                );
            }
            result
        });
        task.await.map_err(|error| {
            format!(
                "OAuth refresh task failed for MCP server `{}`: {error}",
                self.server_name
            )
        })?
    }

    async fn refresh_transaction(&self) -> Result<(), String> {
        let _refresh_lock = timeout(
            REFRESH_LOCK_TIMEOUT,
            self.store
                .acquire_refresh_lock(&self.server_name, &self.server_url),
        )
        .await
        .map_err(|_| {
            format!(
                "timed out after {REFRESH_LOCK_TIMEOUT:?} waiting for the OAuth refresh lock for `{}`",
                self.server_name
            )
        })??;

        let latest = self
            .store
            .load(&self.server_name, &self.server_url)
            .await
            .map_err(|error| {
                format!("failed to reread authoritative OAuth credentials: {error}")
            })?;
        let Some(latest) = latest else {
            self.manager
                .lock()
                .await
                .set_credential_store(InMemoryCredentialStore::new());
            *self.last_credentials.lock().await = None;
            return Err(format!(
                "OAuth credentials for `{}` were removed; authorization required",
                self.server_name
            ));
        };

        let mut manager = self.manager.lock().await;
        let metadata = timeout(REFRESH_REQUEST_TIMEOUT, manager.resolve_metadata())
            .await
            .map_err(|_| {
                format!(
                    "timed out after {REFRESH_REQUEST_TIMEOUT:?} resolving current OAuth metadata for `{}`",
                    self.server_name
                )
            })?
            .map_err(|error| {
                format!(
                    "failed to resolve current OAuth metadata for `{}`: {error}",
                    self.server_name
                )
            })?
            .metadata;
        super::validate_authorization_server_endpoints(&metadata).map_err(|error| {
            format!(
                "current OAuth metadata for `{}` is invalid: {error}",
                self.server_name
            )
        })?;
        let current_issuer = super::authorization_issuer(&metadata)?;
        super::validate_refresh_token_issuer(&latest, current_issuer.as_deref()).map_err(
            |error| {
                format!(
                    "OAuth refresh credentials for `{}` cannot be used: {error}",
                    self.server_name
                )
            },
        )?;
        manager.set_metadata(metadata);

        if !latest.needs_refresh() {
            install_credentials(&mut manager, &latest).await?;
            *self.last_credentials.lock().await = Some(latest);
            return Ok(());
        }
        if latest
            .refresh_token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
        {
            return Err(format!(
                "OAuth credentials for `{}` cannot be refreshed; authorization required",
                self.server_name
            ));
        }

        install_credentials(&mut manager, &latest)
            .await
            .map_err(|error| format!("failed to stage OAuth credentials for refresh: {error}"))?;
        let response = match timeout(REFRESH_REQUEST_TIMEOUT, manager.refresh_token()).await {
            Ok(Ok(response)) => response,
            Ok(Err(AuthError::TokenRefreshRejected(error))) => {
                return Err(format!(
                    "OAuth refresh token for `{}` was rejected; authorization required: {error}",
                    self.server_name
                ));
            }
            Ok(Err(error)) => {
                return Err(format!(
                    "temporarily failed to refresh OAuth credentials for `{}`: {error}",
                    self.server_name
                ));
            }
            Err(_) => {
                return Err(format!(
                    "timed out after {REFRESH_REQUEST_TIMEOUT:?} refreshing OAuth credentials for `{}`",
                    self.server_name
                ));
            }
        };
        let refreshed = McpOAuthCredentials::from_refresh_response(
            latest.client_id.clone(),
            &response,
            &latest,
        );

        if let Err(error) = self
            .store
            .save(&self.server_name, &self.server_url, &refreshed)
            .await
        {
            install_credentials(&mut manager, &latest).await.map_err(|restore_error| {
                format!(
                    "failed to persist refreshed OAuth credentials ({error}) and failed to restore the previous credentials: {restore_error}"
                )
            })?;
            return Err(format!(
                "failed to persist refreshed OAuth credentials for `{}`: {error}",
                self.server_name
            ));
        }

        install_credentials(&mut manager, &refreshed).await.map_err(|error| {
            format!(
                "refreshed OAuth credentials for `{}` were persisted but could not be installed: {error}",
                self.server_name
            )
        })?;
        *self.last_credentials.lock().await = Some(refreshed);
        Ok(())
    }
}

async fn install_credentials(
    manager: &mut AuthorizationManager,
    credentials: &McpOAuthCredentials,
) -> Result<(), String> {
    let store = InMemoryCredentialStore::new();
    store
        .save(
            StoredCredentials::new(
                credentials.client_id.clone(),
                Some(credentials.to_token_response()),
                credentials.scopes.clone(),
                Some(now_seconds()),
            )
            .with_issuer(credentials.issuer.clone()),
        )
        .await
        .map_err(|error| format!("failed to stage OAuth credentials: {error}"))?;
    manager.set_credential_store(store);
    manager
        .initialize_from_store()
        .await
        .map_err(|error| format!("failed to adopt OAuth credentials: {error}"))?;
    Ok(())
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;
