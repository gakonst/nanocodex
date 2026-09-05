use std::{num::NonZeroU32, sync::Arc};

use crate::{DefaultResponsesService, ModelConfig, OpenAiError, ResponsesService};

#[derive(Clone, Copy, Default)]
pub(crate) struct FactoryPlatform;

impl FactoryPlatform {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) fn validate_config(&self, config: &ModelConfig) -> Result<(), OpenAiError> {
        if config.host_transport.is_some() {
            Ok(())
        } else {
            Err(OpenAiError::InvalidConfiguration {
                detail: "the standard WebAssembly client requires a Responses host transport",
            })
        }
    }

    pub(crate) fn make(
        &self,
        config: Arc<ModelConfig>,
        max_attempts: NonZeroU32,
    ) -> DefaultResponsesService {
        ResponsesService::standard_with_max_attempts(config, max_attempts)
    }
}
