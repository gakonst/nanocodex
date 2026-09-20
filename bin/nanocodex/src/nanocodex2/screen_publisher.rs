//! Managed-host adapter for the shared standalone/VM/native publisher runtime.
use super::observation_providers::{Context, Registry};
use super::screen_video::VideoSource;
use futures_util::future::BoxFuture;
use nanocodex_managed::ManagedError;
use nanocodex_remote::{runtime, target::PublisherTarget};
use nanocodex_tools::attachment::{AttachmentMachine, AttachmentTarget};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

pub(crate) type ScreenBackend =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, ManagedError>> + Send + Sync>;
pub(crate) struct ScreenPublisher(runtime::Publisher);
impl ScreenPublisher {
    pub(crate) fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
    pub(crate) async fn start(
        target: &AttachmentTarget,
        machine: &AttachmentMachine,
        backend: ScreenBackend,
        video: Option<VideoSource>,
        broadcast: Option<super::screen_broadcast::Source>,
        audio: Option<VideoSource>,
        providers: Registry,
    ) -> Result<Self, ManagedError> {
        #[cfg(target_os = "macos")]
        let native_broadcast = broadcast.is_some();
        let broadcast = super::screen_broadcast::Broadcast::new(broadcast, audio.clone())
            .with_encoded(video.clone());
        #[cfg(target_os = "macos")]
        let broadcast = if native_broadcast {
            broadcast.with_raw(super::screen_native::native_broadcast_frames())
        } else {
            broadcast
        };
        let require_video = cfg!(target_os = "macos") && video.is_some();
        let video = if std::env::var("NANOCODEX_SCREEN_TRANSPORT").as_deref() == Ok("frames-v1") {
            if require_video {
                return Err(error(
                    "macOS remote viewing requires WebRTC; remove NANOCODEX_SCREEN_TRANSPORT=frames-v1",
                ));
            }
            None
        } else {
            video
        };
        let backend: runtime::Backend = Arc::new(move |value| {
            let result = backend(value);
            Box::pin(async move { result.await.map_err(|error| Box::new(error) as _) })
        });
        let machine = runtime::Machine::new(machine.id(), machine.name()).map_err(error)?;
        let microphone_factory = if video.is_some() {
            nanocodex_remote::audio_duplex::native_factory(machine.id()).await
        } else {
            None
        };
        runtime::Publisher::start(
            &publisher_target(target)?,
            &machine,
            backend,
            runtime::Options {
                video,
                audio,
                microphone_factory,
                require_video,
                observation: Some(Arc::new(providers)),
                broadcast: Box::new(broadcast),
            },
        )
        .await
        .map(Self)
        .map_err(error)
    }
    pub(crate) async fn refresh(&self, target: &AttachmentTarget) -> Result<(), ManagedError> {
        self.0
            .refresh(&publisher_target(target)?)
            .await
            .map_err(error)
    }
    pub(crate) async fn shutdown(self) -> Result<(), ManagedError> {
        self.0.shutdown().await.map_err(error)
    }
}
fn publisher_target(target: &AttachmentTarget) -> Result<PublisherTarget, ManagedError> {
    PublisherTarget::from_attachment(target.endpoint().as_str(), target.bearer()).map_err(error)
}
fn error(value: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(value.to_string())
}
impl runtime::Observation for Registry {
    fn valid_context(&self, context: Option<&Value>) -> bool {
        Context::parse(context).is_ok()
    }
    fn collect(
        &self,
        context: Option<Value>,
        captured_at: u64,
        budget: Duration,
    ) -> BoxFuture<'_, Value> {
        Box::pin(async move {
            self.collect(
                Context::parse(context.as_ref()).ok().flatten(),
                captured_at,
                budget,
            )
            .await
        })
    }
}
impl runtime::Broadcast for super::screen_broadcast::Broadcast {
    fn supported(&self) -> bool {
        self.supported()
    }
    fn request<'a>(&'a mut self, value: &'a Value) -> BoxFuture<'a, Value> {
        Box::pin(self.request(value))
    }
    fn stop(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(self.stop())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanocodex_remote::runtime::Observation;
    #[tokio::test]
    async fn stalled_provider_adapter_finishes_within_budget() {
        let registry = Registry::stalled();
        assert!(registry.valid_context(None));
        let result = tokio::time::timeout(
            Duration::from_millis(300),
            Observation::collect(&registry, None, 0, Duration::from_millis(50)),
        )
        .await
        .unwrap();
        assert_eq!(result["providers"][0]["status"], "timeout");
    }
}
