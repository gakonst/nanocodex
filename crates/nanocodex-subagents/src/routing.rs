//! Host-owned, task-aware routing before a clean child is constructed.
use async_trait::async_trait;
use nanocodex_agent::SpawnOptions;

/// A prepared choice. The opaque reference carries no provider credentials.
pub struct SpawnRoute {
    pub options: SpawnOptions,
    pub reference: String,
}

impl SpawnRoute {
    /// Reject incomplete or conflicting routes before constructing a child.
    pub(crate) fn validate(&self, requested: SpawnOptions) -> std::io::Result<()> {
        let model = self
            .options
            .selected_model()
            .ok_or_else(|| std::io::Error::other("subagent route must select a model"))?;
        let thinking = self
            .options
            .selected_thinking()
            .ok_or_else(|| std::io::Error::other("subagent route must select thinking"))?;
        if requested
            .selected_model()
            .is_some_and(|value| value != model)
            || requested
                .selected_thinking()
                .is_some_and(|value| value != thinking)
        {
            return Err(std::io::Error::other(
                "subagent route conflicts with explicit override",
            ));
        }
        if !model.supports_thinking(thinking) {
            return Err(std::io::Error::other(
                "subagent route selected unsupported thinking",
            ));
        }
        if self.reference.trim().is_empty() {
            return Err(std::io::Error::other("empty subagent route reference"));
        }
        Ok(())
    }
}

/// Implemented by the embedding host. Resolve must enforce the invoking child's
/// authority and explicit overrides; bind must retain the choice in memory before
/// any child turn starts. Returning an error fails the spawn closed.
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
pub trait SpawnRouter: Send + Sync {
    async fn resolve(
        &self,
        parent_session_id: &str,
        role: &str,
        task: &str,
        options: SpawnOptions,
        host_context: Option<&str>,
    ) -> std::io::Result<SpawnRoute>;

    fn bind(
        &self,
        parent_session_id: &str,
        child_session_id: &str,
        reference: &str,
        host_context: Option<&str>,
    ) -> std::io::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::SpawnRoute;
    use nanocodex_agent::{Model, SpawnOptions, Thinking};

    #[test]
    fn routes_require_complete_supported_choices_and_preserve_overrides() {
        let valid = SpawnOptions::new()
            .model(Model::Sol)
            .thinking(Thinking::High);
        for (options, reference, requested) in [
            (SpawnOptions::new(), "route", SpawnOptions::new()),
            (
                SpawnOptions::new().model(Model::Sol),
                "route",
                SpawnOptions::new(),
            ),
            (valid, "  ", SpawnOptions::new()),
            (valid, "route", SpawnOptions::new().model(Model::Astra)),
            (valid, "route", SpawnOptions::new().thinking(Thinking::Low)),
            (
                SpawnOptions::new()
                    .model(Model::Astra)
                    .thinking(Thinking::None),
                "route",
                SpawnOptions::new(),
            ),
            (
                SpawnOptions::new()
                    .model(Model::Glm53)
                    .thinking(Thinking::Max),
                "route",
                SpawnOptions::new(),
            ),
        ] {
            assert!(
                SpawnRoute {
                    options,
                    reference: reference.to_owned()
                }
                .validate(requested)
                .is_err()
            );
        }
        let route = SpawnRoute {
            options: valid,
            reference: "route".to_owned(),
        };
        assert!(route.validate(SpawnOptions::new()).is_ok());
        assert!(route.validate(valid).is_ok());
    }
}
