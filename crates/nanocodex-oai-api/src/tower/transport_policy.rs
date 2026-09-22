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

    pub(crate) fn activate_https_fallback(&self) -> bool {
        self.fallback_to_https
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_one_way_and_scoped_to_the_session() {
        let session = SessionTransport::new();
        let other = SessionTransport::new();
        assert_eq!(
            session.effective(ResponsesTransport::WebSocket),
            ResponsesTransport::WebSocket
        );
        assert!(session.activate_https_fallback());
        assert!(!session.activate_https_fallback());
        assert_eq!(
            session.effective(ResponsesTransport::WebSocket),
            ResponsesTransport::Https
        );
        assert_eq!(
            session.effective(ResponsesTransport::Https),
            ResponsesTransport::Https
        );
        assert_eq!(
            other.effective(ResponsesTransport::WebSocket),
            ResponsesTransport::WebSocket
        );
    }
}
