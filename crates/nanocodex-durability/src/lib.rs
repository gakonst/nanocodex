#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod agent;
mod encoding;
mod memory;
#[cfg(all(feature = "postgres", not(target_family = "wasm")))]
mod postgres;
mod session;
mod shared_store;
#[cfg(all(feature = "sqlite", not(target_family = "wasm")))]
mod sqlite;
mod state;
mod store;

pub use memory::MemoryStore;
#[cfg(all(feature = "postgres", not(target_family = "wasm")))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(feature = "postgres", not(target_family = "wasm"))))
)]
pub use postgres::PostgresStore;
pub use session::{Admission, AutomaticAdmission, BeginStep, DurableSession};
#[cfg(all(feature = "sqlite", not(target_family = "wasm")))]
#[cfg_attr(docsrs, doc(cfg(all(feature = "sqlite", not(target_family = "wasm")))))]
pub use sqlite::SqliteStore;
pub use state::{
    DurableState, EncodedPayload, OperationState, OperationStatus, SteerState, StepState,
    StepStatus, Transition,
};
pub use store::{
    OwnedState, OwnerId, OwnerToken, StateStore, StoreError, StoreFuture, StoredState,
};

/// Result returned by durability operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Portable durable-execution failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The host store rejected or failed an operation.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Stored state could not be decoded.
    #[error("durability state at revision {revision} is invalid: {source}")]
    Decode {
        /// Revision containing the invalid state transition.
        revision: u64,
        /// JSON decoding failure.
        #[source]
        source: serde_json::Error,
    },
    /// Retained state violated the durability state machine.
    #[error("invalid durability state: {0}")]
    InvalidState(String),
    /// An operation ID was reused with different input.
    #[error("durable operation `{operation_id}` already has different input")]
    OperationConflict {
        /// Conflicting operation identity.
        operation_id: String,
    },
    /// Work was submitted behind an earlier unfinished operation.
    #[error("durable operation `{operation_id}` is blocked by unfinished operation `{pending_id}`")]
    OperationBlocked {
        /// Newly submitted operation.
        operation_id: String,
        /// Earlier unfinished operation.
        pending_id: String,
    },
    /// A step completed without first being started.
    #[error("durable step `{step_id}` in operation `{operation_id}` was not started")]
    StepNotStarted {
        /// Owning operation.
        operation_id: String,
        /// Missing step.
        step_id: String,
    },
    /// A terminal operation cannot be changed.
    #[error("durable operation `{operation_id}` is already terminal")]
    OperationTerminal {
        /// Terminal operation.
        operation_id: String,
    },
    /// Another caller is already executing this operation in the live process.
    #[error("durable operation `{operation_id}` is already active")]
    OperationActive {
        /// Active operation identity.
        operation_id: String,
    },
    /// A direct state caller attempted to mutate state while an Agent owns it.
    #[error("durability state has a live authoritative model owner")]
    ModelOwnerActive,
    /// A superseded Agent attempted to use its stale model-owner capability.
    #[error("durability model owner was fenced by a newer owner")]
    ModelOwnerFenced,
    /// A lifecycle mutation did not hold the operation's live claim.
    #[error("durable operation `{operation_id}` is not claimed by this owner")]
    OperationNotClaimed {
        /// Unclaimed operation identity.
        operation_id: String,
    },
    /// A step was requested before the owning operation began an attempt.
    #[error("durable operation `{operation_id}` has not begun an attempt")]
    AttemptNotStarted {
        /// Operation without a begun attempt.
        operation_id: String,
    },
    /// An operation already has a live attempt and cannot begin another one.
    #[error("durable operation `{operation_id}` already has an active attempt")]
    AttemptActive {
        /// Operation with a live attempt.
        operation_id: String,
    },
    /// Active cancellation omitted the safe interrupted checkpoint.
    #[error("active durable operation `{operation_id}` cancellation requires a checkpoint")]
    CancellationCheckpointRequired {
        /// Active operation that cannot be cancelled without a checkpoint.
        operation_id: String,
    },
    /// A native owner task was created without an active Tokio runtime.
    #[error("durability requires an active Tokio runtime")]
    RuntimeUnavailable,
    /// The spawned durability owner is no longer running.
    #[error("durability driver stopped")]
    DriverStopped,
    /// A caller-supplied payload was not valid JSON.
    #[error("durability payload is invalid JSON: {0}")]
    InvalidPayload(#[from] serde_json::Error),
}
pub use agent::DurableAgentExt;
