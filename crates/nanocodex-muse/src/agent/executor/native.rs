use std::{future::Future, sync::Arc};

use nanocodex_oai_api::__private::ModelConfig;

use crate::{NanocodexError, Result};

pub(crate) type ServiceFactory<S> = Arc<dyn Fn(Arc<ModelConfig>) -> S + Send + Sync>;

pub(crate) trait AgentSend: Send {}

impl<T: Send> AgentSend for T {}

pub(crate) trait AgentFactory: Send + Sync {}

impl<T: Send + Sync> AgentFactory for T {}

pub(crate) fn spawn_driver<F>(driver: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| NanocodexError::TokioRuntimeUnavailable)?;
    drop(runtime.spawn(driver));
    Ok(())
}
