use std::{future::Future, pin::Pin, sync::Arc};

use crate::{
    ModelConfig, OpenAiAuthSnapshot, ResponsesError,
    socket::{ConnectionMetadata, ResponsesSocket},
    tower::{ResponsesServiceError, ResponsesServiceResponse},
    transport::host::HostTransport,
};

pub(crate) type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>>>>;

#[derive(Clone)]
pub(crate) struct ServicePlatform {
    host: Option<Arc<dyn HostTransport>>,
    http: crate::http::ResponsesHttp,
}

impl ServicePlatform {
    pub(crate) const fn http(&self) -> &crate::http::ResponsesHttp {
        &self.http
    }

    pub(crate) fn new(config: &ModelConfig) -> Self {
        Self {
            host: config.host_transport.clone(),
            http: crate::http::ResponsesHttp::new(config.host_transport.clone()),
        }
    }
}

pub(crate) async fn connect_socket(
    platform: &ServicePlatform,
    config: &ModelConfig,
    auth: &OpenAiAuthSnapshot,
    session_id: &str,
    thread_id: &str,
    turn_state: Option<&str>,
) -> Result<(ResponsesSocket, ConnectionMetadata), ResponsesError> {
    let host = platform
        .host
        .as_deref()
        .ok_or(ResponsesError::HostUnavailable)?;
    ResponsesSocket::connect(
        host,
        &config.websocket_url,
        auth,
        session_id,
        thread_id,
        turn_state,
    )
    .await
}
