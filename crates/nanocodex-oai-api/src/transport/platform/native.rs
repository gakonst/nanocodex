use std::{future::Future, pin::Pin};

use crate::{
    ModelConfig, OpenAiAuthSnapshot, ResponsesError,
    http::ResponsesHttp,
    socket::{ConnectionMetadata, ResponsesSocket},
    tower::{ResponsesServiceError, ResponsesServiceResponse},
};

pub(crate) type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>> + Send>>;

#[derive(Clone)]
pub(crate) struct ServicePlatform {
    http: ResponsesHttp,
}

impl ServicePlatform {
    pub(crate) fn new(config: &ModelConfig) -> Self {
        crate::transport::install_default_rustls_crypto_provider();
        Self::with_http_client(config, reqwest::Client::new())
    }

    pub(crate) const fn with_http_client(_config: &ModelConfig, client: reqwest::Client) -> Self {
        Self {
            http: ResponsesHttp::new(client),
        }
    }

    pub(crate) const fn http(&self) -> &ResponsesHttp {
        &self.http
    }
}

pub(crate) async fn connect_socket(
    _platform: &ServicePlatform,
    config: &ModelConfig,
    auth: &OpenAiAuthSnapshot,
    session_id: &str,
    thread_id: &str,
    turn_state: Option<&str>,
) -> Result<(ResponsesSocket, ConnectionMetadata), ResponsesError> {
    ResponsesSocket::connect(
        &config.websocket_url,
        auth,
        session_id,
        thread_id,
        turn_state,
    )
    .await
}
