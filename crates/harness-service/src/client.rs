use tower::{Service, ServiceExt};

use crate::attempt::ResponsesAttempt;

/// An owned Responses client over an arbitrary Tower service stack.
pub struct ResponsesClient<S> {
    service: S,
}

impl<S> ResponsesClient<S> {
    pub const fn new(service: S) -> Self {
        Self { service }
    }

    pub const fn service(&self) -> &S {
        &self.service
    }

    pub const fn service_mut(&mut self) -> &mut S {
        &mut self.service
    }

    pub fn into_service(self) -> S {
        self.service
    }

    #[must_use]
    pub fn map_service<T>(self, map: impl FnOnce(S) -> T) -> ResponsesClient<T> {
        ResponsesClient::new(map(self.service))
    }

    /// Executes one request through the owned service stack.
    ///
    /// # Errors
    ///
    /// Returns the composed service's readiness or call error.
    pub async fn execute(&mut self, request: ResponsesAttempt) -> Result<S::Response, S::Error>
    where
        S: Service<ResponsesAttempt>,
    {
        self.service.ready().await?.call(request).await
    }
}
