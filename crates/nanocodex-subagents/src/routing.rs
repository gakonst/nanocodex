//! Host-owned, task-aware routing before a clean child is constructed.
use async_trait::async_trait;
use nanocodex_agent::{Model, SpawnOptions};

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

/// A host may explicitly keep native GPT spawning, including live parent defaults.
/// The reference is still bound before inference, just like a routed choice.
pub enum SpawnDecision {
    Routed(SpawnRoute),
    Native { reference: String },
}

impl SpawnDecision {
    pub(crate) fn validate(&self, requested: SpawnOptions) -> std::io::Result<()> {
        match self {
            Self::Routed(route) => route.validate(requested),
            Self::Native { reference } => {
                if reference.trim().is_empty()
                    || requested.selected_model().is_some_and(|model| {
                        !matches!(model, Model::Astra | Model::Sol | Model::Luna)
                    })
                {
                    return Err(std::io::Error::other("invalid native subagent choice"));
                }
                Ok(())
            }
        }
    }

    pub(crate) const fn options(&self, requested: SpawnOptions) -> SpawnOptions {
        match self {
            Self::Routed(route) => route.options,
            Self::Native { .. } => requested,
        }
    }

    pub(crate) fn reference(&self) -> &str {
        match self {
            Self::Routed(route) => &route.reference,
            Self::Native { reference } => reference,
        }
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

    /// Additive native choice support; existing routers continue routing every child.
    async fn resolve_spawn(
        &self,
        parent_session_id: &str,
        role: &str,
        task: &str,
        options: SpawnOptions,
        host_context: Option<&str>,
    ) -> std::io::Result<SpawnDecision> {
        self.resolve(parent_session_id, role, task, options, host_context)
            .await
            .map(SpawnDecision::Routed)
    }

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
    use super::{SpawnDecision, SpawnRoute};
    use nanocodex_agent::{Model, SpawnOptions, Thinking};

    #[test]
    fn native_choices_preserve_defaults_and_overrides_but_reject_non_gpt() {
        let native = SpawnDecision::Native {
            reference: "native-ticket".into(),
        };
        for requested in [
            SpawnOptions::new(),
            SpawnOptions::new().thinking(Thinking::Max),
            SpawnOptions::new()
                .model(Model::Sol)
                .thinking(Thinking::None),
        ] {
            assert!(native.validate(requested).is_ok());
            assert_eq!(
                native.options(requested).selected_model(),
                requested.selected_model()
            );
            assert_eq!(
                native.options(requested).selected_thinking(),
                requested.selected_thinking()
            );
        }
        for model in [Model::Kimi, Model::Mimo, Model::Glm53] {
            assert!(native.validate(SpawnOptions::new().model(model)).is_err());
        }
        assert!(
            SpawnDecision::Native {
                reference: "".into()
            }
            .validate(SpawnOptions::new())
            .is_err()
        );
    }

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
