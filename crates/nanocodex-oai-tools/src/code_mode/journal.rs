//! Host-owned durable admission, state and effect receipts for native Code Mode.

use std::collections::HashMap;

use serde_json::Value;

/// State selected before a cell starts, or the result retained by an earlier attempt.
pub enum CodeJournalAdmission {
    /// The journal durably admitted this new cell.
    Execute {
        /// Complete session-local `store` snapshot.
        stored: HashMap<String, Value>,
        /// Optimistic document version; zero means the document is absent.
        version: u64,
    },
    /// A terminal result encoded using the `CodeModeExecution` JSON contract.
    Replay(Value),
    /// A prior attempt remains unfinished. Its effects must never be redispatched.
    Unknown,
}

/// Durable journal supplied by a native embedding.
///
/// Admission must claim one session-local call ID and validate its source. A
/// pending cell must return `Unknown`, never `Execute`. `begin_effect` must
/// persist an unsettled receipt before dispatch; `complete_effect` must retain
/// its observed result. `complete_cell` must atomically commit successful store
/// writes and the terminal receipt, with the expected document version. Failed
/// cells supply no writes. Completed cell receipts must remain replayable after
/// a fresh runtime is constructed. Forks must select the store document at the
/// same historical boundary as the terminal checkpoint.
#[async_trait::async_trait]
pub trait CodeModeJournal: Send + Sync {
    /// Durably admits a cell or returns its previous receipt.
    async fn admit_cell(
        &self,
        session_id: &str,
        call_id: &str,
        source: &str,
    ) -> Result<CodeJournalAdmission, String>;

    /// Records a pending external effect before its handler is invoked.
    async fn begin_effect(
        &self,
        call_id: &str,
        effect_id: &str,
        name: &str,
        input: &Value,
    ) -> Result<(), String>;

    /// Records a nested call result, including an explicit unknown outcome.
    async fn complete_effect(
        &self,
        call_id: &str,
        effect_id: &str,
        receipt: &Value,
    ) -> Result<(), String>;

    /// Atomically retains the terminal execution and successful store writes.
    ///
    /// `stored` is the complete next snapshot; `None` preserves the previous
    /// snapshot. The receipt follows the `CodeModeExecution` JSON contract.
    async fn complete_cell(
        &self,
        call_id: &str,
        expected_version: u64,
        stored: Option<HashMap<String, Value>>,
        receipt: &Value,
    ) -> Result<(), String>;
}
