use std::sync::atomic::{AtomicBool, Ordering};

use crate::ResponsesTransport;

/// Session-scoped one-way fallback state carried by replayable attempts.
#[derive(Debug, Default)]
pub(crate) struct SessionTransport {
    fallback_to_https: AtomicBool,
}

impl SessionTransport {
    pub(crate) const fn new() -> Self {
        Self {
            fallback_to_https: AtomicBool::new(false),
        }
    }

    pub(crate) fn effective(&self, preferred: ResponsesTransport) -> ResponsesTransport {
        if matches!(preferred, ResponsesTransport::WebSocket)
            && self.fallback_to_https.load(Ordering::Acquire)
        {
            ResponsesTransport::Https
        } else {
            preferred
        }
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn activate_https_fallback(&self) -> bool {
        self.fallback_to_https
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    #[cfg(target_family = "wasm")]
    pub(crate) const fn activate_https_fallback(&self) -> bool {
        false
    }
}
