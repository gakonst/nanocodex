use tower::{
    ServiceBuilder,
    layer::util::{Identity, Stack},
};

use nanocodex_core::{ResponsesHistory, ResponsesTransport};

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
    pub(crate) transport: ResponsesTransport,
    pub(crate) history: Option<ResponsesHistory>,
    pub(crate) store: Option<bool>,
    pub(crate) service: S,
}

impl Default for Responses<StandardResponses> {
    fn default() -> Self {
        Self {
            websocket_url: None,
            api_base_url: None,
            transport: ResponsesTransport::WebSocket,
            history: None,
            store: None,
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
    /// Adds a Tower layer around the SDK's standard Responses transport and
    /// retry service. Layers are not materialized until
    /// [`crate::NanocodexBuilder::build`].
    #[must_use]
    pub fn layer<L>(self, layer: L) -> ResponsesBuilder<LayeredResponses<Stack<L, Identity>>> {
        ResponsesBuilder {
            responses: Responses {
                websocket_url: self.responses.websocket_url,
                api_base_url: self.responses.api_base_url,
                transport: self.responses.transport,
                history: self.responses.history,
                store: self.responses.store,
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
                transport: self.responses.transport,
                history: self.responses.history,
                store: self.responses.store,
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
                transport: self.responses.transport,
                history: self.responses.history,
                store: self.responses.store,
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
    /// Selects the transport once for the complete lifetime of an agent and
    /// every child or fork created from it.
    #[must_use]
    pub const fn transport(mut self, transport: ResponsesTransport) -> Self {
        self.responses.transport = transport;
        self
    }

    /// Selects incremental response-ID chaining or complete history replay.
    ///
    /// When omitted, HTTPS with `store: false` selects full replay and all
    /// other combinations select incremental chaining.
    #[must_use]
    pub const fn history(mut self, history: ResponsesHistory) -> Self {
        self.responses.history = Some(history);
        self
    }

    /// Controls whether Responses checkpoints are retained by the API.
    ///
    /// The default is `true` for API-key authentication and `false` for
    /// `ChatGPT` subscription authentication.
    #[must_use]
    pub const fn store(mut self, store: bool) -> Self {
        self.responses.store = Some(store);
        self
    }

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
