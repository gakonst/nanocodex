//! Screen-only recovery lives in `nanocodex-screen-supervisor` so its
//! paused-clock tests do not enable tokio test-util for the whole CLI test build.
pub(super) use nanocodex_screen_supervisor::{
    Report, Session, Stop, supervise_observed, while_attached_observed, while_attached_reported,
};
