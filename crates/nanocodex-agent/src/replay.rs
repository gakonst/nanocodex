//! Recovery permission for effects whose durable intent has no receipt.
use serde::{Deserialize, Serialize};
/// Whether an interrupted effect may be invoked again. Both the persisted intent
/// and the current handler must opt in. Completed receipts always replay.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaySafety {
    /// Outcome may be unknown after interruption; do not dispatch again.
    #[default]
    Unsafe,
    /// Exact repetition is safe, including host-enforced idempotency.
    Safe,
}
