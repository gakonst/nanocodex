use tower::{
    ServiceBuilder,
    layer::util::{Identity, Stack},
};

/// Marker used until the standard Responses service is constructed by the
/// agent builder.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardResponses;

/// Deferred Tower layers applied to the standard Responses service when the
/// agent is built.
#[doc(hidden)]
#[derive(Clone)]
pub struct LayeredResponses<L>(pub(crate) ServiceBuilder<L>);

/// Deferred caller service factory used to create one independent stack per
/// conversation branch.
#[doc(hidden)]
#[derive(Clone)]
pub struct FactoryResponses<F>(pub(crate) F);

/// Responses transport configuration with standard or caller-supplied Tower
/// service factory policy.
#[derive(Clone)]
pub struct Responses<S = StandardResponses> {
    pub(crate) websocket_url: Option<String>,
    pub(crate) api_base_url: Option<String>,
    pub(crate) service: S,
}

impl Default for Responses<StandardResponses> {
    fn default() -> Self {
        Self {
            websocket_url: None,
            api_base_url: None,
            service: StandardResponses,
        }
    }
}

impl Responses<StandardResponses> {
    #[must_use]
    pub fn builder() -> ResponsesBuilder<StandardResponses> {
        ResponsesBuilder {
            responses: Self::default(),
        }
    }
}

impl ResponsesBuilder<StandardResponses> {
    /// Adds a Tower layer around the SDK's standard persistent WebSocket and
    /// retry service. Layers are not materialized until
    /// [`crate::NanocodexBuilder::build`].
    #[must_use]
    pub fn layer<L>(self, layer: L) -> ResponsesBuilder<LayeredResponses<Stack<L, Identity>>> {
        ResponsesBuilder {
            responses: Responses {
                websocket_url: self.responses.websocket_url,
                api_base_url: self.responses.api_base_url,
                service: LayeredResponses(ServiceBuilder::new().layer(layer)),
            },
        }
    }

    /// Replaces the standard stack with a factory that constructs one fresh
    /// caller-composed service for the root and every child or fork.
    #[must_use]
    pub fn service<F, S>(self, factory: F) -> ResponsesBuilder<FactoryResponses<F>>
    where
        F: Fn() -> S,
    {
        ResponsesBuilder {
            responses: Responses {
                websocket_url: self.responses.websocket_url,
                api_base_url: self.responses.api_base_url,
                service: FactoryResponses(factory),
            },
        }
    }
}

impl<L> ResponsesBuilder<LayeredResponses<L>> {
    /// Adds another Tower layer to the deferred standard service stack.
    #[must_use]
    pub fn layer<T>(self, layer: T) -> ResponsesBuilder<LayeredResponses<Stack<T, L>>> {
        ResponsesBuilder {
            responses: Responses {
                websocket_url: self.responses.websocket_url,
                api_base_url: self.responses.api_base_url,
                service: LayeredResponses(self.responses.service.0.layer(layer)),
            },
        }
    }
}

/// Builder for the standard Responses endpoints or a caller-composed service.
pub struct ResponsesBuilder<S> {
    responses: Responses<S>,
}

impl<S> ResponsesBuilder<S> {
    #[must_use]
    pub fn websocket_url(mut self, url: impl Into<String>) -> Self {
        self.responses.websocket_url = Some(url.into());
        self
    }

    #[must_use]
    pub fn api_base_url(mut self, url: impl Into<String>) -> Self {
        self.responses.api_base_url = Some(url.into());
        self
    }

    #[must_use]
    pub fn build(self) -> Responses<S> {
        self.responses
    }
}
