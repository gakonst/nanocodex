use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use crate::{ModelConfig, transport::host::HostTransport};

pub(crate) type RetryFuture = Pin<Box<dyn Future<Output = ()>>>;

#[derive(Clone, Default)]
pub(crate) struct RetryDelay {
    host: Option<Arc<dyn HostTransport>>,
}

impl RetryDelay {
    pub(crate) const fn unconfigured() -> Self {
        Self { host: None }
    }

    pub(crate) fn from_config(config: &ModelConfig) -> Self {
        Self {
            host: config.host_transport.clone(),
        }
    }

    pub(crate) fn clone_for_retry(&self) -> Self {
        self.clone()
    }

    pub(crate) fn wait(&self, session_id: String, duration: Duration) -> RetryFuture {
        let host = self.host.clone();
        Box::pin(async move {
            if let Some(host) = host {
                host.sleep(&session_id, duration).await;
            }
        })
    }
}

impl fmt::Debug for RetryDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetryDelay")
            .field("host_configured", &self.host.is_some())
            .finish()
    }
}
