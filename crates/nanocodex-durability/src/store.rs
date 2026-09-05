use std::{future::Future, pin::Pin};

/// Fresh, unguessable identity proposed by a state owner.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct OwnerId(String);

impl OwnerId {
    /// Generates a fresh UUIDv7 owner identity.
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }
    /// Returns the encoded owner identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for OwnerId {
    fn default() -> Self {
        Self::new()
    }
}

/// Authority installed by one successful owner acquisition.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct OwnerToken {
    owner_id: OwnerId,
    fence: u64,
}

impl OwnerToken {
    /// Reconstructs a token returned by an authoritative host store.
    pub const fn new(owner_id: OwnerId, fence: u64) -> Self {
        Self { owner_id, fence }
    }
    /// Returns the identity installed by this acquisition.
    pub const fn owner_id(&self) -> &OwnerId {
        &self.owner_id
    }
    /// Returns the monotonically increasing fencing generation.
    pub const fn fence(&self) -> u64 {
        self.fence
    }
}

/// Complete opaque durable state retained by a host store.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct StoredState {
    /// Current compare-and-replace revision.
    pub revision: u64,
    /// Rust-owned JSON payload. `None` is valid only at revision zero.
    pub payload: Option<String>,
}

/// One owner acquisition and its same-transaction state snapshot.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct OwnedState {
    /// Newly installed owner authority.
    pub owner: OwnerToken,
    /// Self-consistent state observed by that acquisition.
    pub state: StoredState,
}

/// Host-store failure.
///
/// Only [`StoreError::NotCommitted`] promises that retrying the same operation
/// is safe. Every other mutation failure requires reopening from authoritative
/// state or resolving the structural conflict.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StoreError {
    /// A newer or different owner holds authority for this state.
    #[error("durability state owner was fenced")]
    Fenced,
    /// Another writer advanced the state.
    #[error("durability state revision conflict: expected {expected}, found {actual}")]
    Conflict {
        /// Revision supplied by the caller.
        expected: u64,
        /// Revision currently retained by the store.
        actual: u64,
    },
    /// The host guarantees that the requested operation made no durable change.
    #[error("durability store operation was not committed: {0}")]
    NotCommitted(String),
    /// The selected storage backend failed without proving whether it committed.
    #[error("durability store failed: {0}")]
    Backend(String),
}

/// Boxed host operation used by [`StateStore`].
#[cfg(not(target_family = "wasm"))]
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed host operation used by [`StateStore`].
#[cfg(target_family = "wasm")]
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Minimal host-owned persistence contract.
///
/// `acquire` atomically installs a fresh fencing owner and returns the complete
/// state from the same transaction. `replace` verifies that owner and the
/// expected revision, then atomically replaces the complete opaque payload and
/// advances the revision by exactly one.
#[cfg(not(target_family = "wasm"))]
pub trait StateStore: Send {
    /// Acquires exclusive authority and loads one complete state.
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>>;

    /// Atomically replaces the complete opaque state.
    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>>;
}

/// Minimal host-owned persistence contract.
#[cfg(target_family = "wasm")]
pub trait StateStore {
    /// Acquires exclusive authority and loads one complete state.
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>>;

    /// Atomically replaces the complete opaque state.
    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>>;
}
