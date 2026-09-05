use std::{future::Future, pin::Pin, time::Duration};

use crate::ModelConfig;

pub(crate) type RetryFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RetryDelay;

impl RetryDelay {
    pub(crate) const fn unconfigured() -> Self {
        Self
    }

    pub(crate) const fn from_config(_config: &ModelConfig) -> Self {
        Self
    }

    pub(crate) const fn clone_for_retry(&self) -> Self {
        *self
    }

    pub(crate) fn wait(&self, _session_id: String, duration: Duration) -> RetryFuture {
        Box::pin(tokio::time::sleep(duration))
    }
}
