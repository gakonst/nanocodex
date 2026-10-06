//! Native account-managed lifecycle backend for Nanocodex.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(target_family = "wasm")]
compile_error!("nanocodex-managed is a native lifecycle backend");

mod auth;
mod builder;
mod claude;
mod client;
mod driver;
mod error;
mod model;
mod native_secure_input;
mod private_input;
mod share;
mod sse;
mod types;
mod vault;
#[cfg(feature = "voice")]
mod voice;
mod websocket;
#[cfg(feature = "voice")]
pub use voice::{ManagedVoiceCall, ManagedVoiceSocket};

#[cfg(feature = "tools")]
mod vm_host;

#[cfg(feature = "tools")]
mod attachment;

pub use auth::ManagedApiKey;
pub use builder::{Managed, ManagedBuilder, ManagedRequest, ManagedResponse, ManagedService};
pub use claude::{ClaudeLogin, ClaudeLoginCode, ClaudeLoginStatus};
pub use client::{ManagedClient, ManagedClientBuilder};
pub use driver::ManagedAgent;
pub use error::ManagedError;
pub use model::{
    AvailableModel, CatalogAvailabilityError, CatalogProviderAvailability, ManagedModel,
    ModelCatalog,
};
pub use nanocodex_agent::{Model, ReasoningMode, Thinking};
pub use native_secure_input::{
    NativeSecureInputDescription, NativeSecureInputEnvelope, NativeSecureInputReceipt,
    NativeSecureInputRequest, NativeSecureInputStatus,
};
pub use private_input::{
    PrivateInputBody, PrivateInputKind, PrivateInputRequest, PrivateVaultItem,
    private_input_output_text,
};
pub use share::{CreatedShareLink, ShareLink, SharePermission};
pub use sse::{
    EventCursor, ManagedEventFuture, ManagedEventSource, ManagedEventStream, ManagedEvents,
};
pub use types::*;
pub use vault::{
    VAULT_REQUEST_MAX_BYTES, VaultBodyEncoding, VaultJwt, VaultKeyEncoding, VaultLogin,
    VaultRequest, VaultRequestMethod, VaultRequestReceipt, VaultSignatureEncoding, VaultSigning,
    VaultSigningAlgorithm, VaultSshTarget,
};

#[cfg(feature = "tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
pub use vm_host::{
    VmHostAllocationState, VmHostCommand, VmHostConnection, VmHostFence, VmHostProvision,
    VmHostRelease, VmHostScope, VmShape, connect_system_vm_host, validate_vm_factory_name,
};
