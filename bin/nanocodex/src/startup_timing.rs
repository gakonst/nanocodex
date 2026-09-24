//! Shared opt-in, content-free client timings for real binary measurements.
use std::{sync::OnceLock, time::Instant};

pub(super) struct Stage {
    name: &'static str,
    started: Option<Instant>,
}
impl Stage {
    pub(super) fn new(name: &'static str) -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled = *ENABLED.get_or_init(|| {
            std::env::var_os("NANOCODEX_STARTUP_TIMING").is_some_and(|value| value == "1")
        });
        Self {
            name,
            started: enabled.then(Instant::now),
        }
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            // Call sites supply static phase names only. No request, account,
            // path, prompt, error or credential data enters the diagnostic.
            eprintln!(
                "{}",
                serde_json::json!({
                    "type": "client.startup", "stage": self.name,
                    "duration_ms": started.elapsed().as_secs_f64() * 1000.0,
                })
            );
        }
    }
}
