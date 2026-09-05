use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use nanocodex_oai_api::auth::{
    OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
    OpenAiAuthSource,
};

#[derive(Default)]
pub(crate) struct RotatingChatGptAuth {
    revision: AtomicU64,
    recoveries: AtomicU64,
}

impl RotatingChatGptAuth {
    pub(crate) fn shared() -> (OpenAiAuth, Arc<Self>) {
        let source = Arc::new(Self::default());
        let auth = OpenAiAuth::managed_chatgpt(Arc::clone(&source) as Arc<dyn OpenAiAuthSource>);
        (auth, source)
    }

    pub(crate) fn recoveries(&self) -> u64 {
        self.recoveries.load(Ordering::Relaxed)
    }
}

impl OpenAiAuthSource for RotatingChatGptAuth {
    fn validate(&self) -> Result<(), OpenAiAuthError> {
        Ok(())
    }

    fn snapshot(&self) -> OpenAiAuthFuture<'_, Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        let revision = self.revision.load(Ordering::Acquire);
        Box::pin(async move {
            Ok(OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ChatGpt,
                format!("oauth-token-{revision}"),
                Some("account-test"),
                true,
                revision,
            ))
        })
    }

    fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, Result<(), OpenAiAuthError>> {
        let rejected_revision = rejected.revision();
        Box::pin(async move {
            self.recoveries.fetch_add(1, Ordering::Relaxed);
            let _ = self.revision.compare_exchange(
                rejected_revision,
                rejected_revision.saturating_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            Ok(())
        })
    }
}
