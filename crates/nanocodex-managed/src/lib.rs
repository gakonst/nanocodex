//! Native account-managed lifecycle backend for Nanocodex.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(target_family = "wasm")]
compile_error!("nanocodex-managed is a native lifecycle backend");

mod account_services;
mod auth;
mod builder;
mod claude;
mod client;
mod connectors;
mod driver;
mod error;
mod hand_shares;
mod model;
mod native_secure_input;
mod phone_services;
mod private_input;
mod share;
mod shared;
mod sites;
mod sse;
mod types;
mod vault;
mod vault_management;
#[cfg(feature = "voice")]
mod voice;
mod websocket;
#[cfg(feature = "voice")]
pub use voice::{ManagedVoiceCall, ManagedVoiceSocket};

#[cfg(feature = "tools")]
mod vm_host;

#[cfg(feature = "tools")]
mod attachment;

pub use account_services::*;
pub use auth::ManagedApiKey;
pub use builder::{Managed, ManagedBuilder, ManagedRequest, ManagedResponse, ManagedService};
pub use claude::{ClaudeLogin, ClaudeLoginCode, ClaudeLoginStatus};
pub use client::{ManagedClient, ManagedClientBuilder};
pub use connectors::*;
pub use driver::ManagedAgent;
pub use error::ManagedError;
pub use hand_shares::{CreatedHandShare, HandShare, HandShares, RedeemedHandShare};
pub use model::{
    AvailableModel, CatalogAvailabilityError, CatalogProviderAvailability, ManagedModel,
    ModelCatalog,
};
pub use nanocodex_agent::{Model, ReasoningMode, Thinking};
pub use native_secure_input::{
    NativeSecureInputDescription, NativeSecureInputEnvelope, NativeSecureInputReceipt,
    NativeSecureInputRequest, NativeSecureInputStatus,
};
pub use phone_services::*;
pub use private_input::{
    PrivateInputBody, PrivateInputKind, PrivateInputRequest, PrivateVaultItem,
    private_input_output_text,
};
pub use share::{CreatedShareLink, ShareLink, SharePermission};
pub use shared::{
    SharedEventStream, SharedHistoryPage, SharedThreadClient, SharedThreadMetadata,
    SharedTurnReceipt,
};
pub use sites::{
    PublishSite, PublishedSite, Site, SiteShare, SiteVersion, SiteView, valid_site_id,
};
pub use sse::{
    EventCursor, ManagedEventFuture, ManagedEventSource, ManagedEventStream, ManagedEvents,
};
pub use types::*;
pub use vault::{
    VAULT_REQUEST_MAX_BYTES, VaultBodyEncoding, VaultJwt, VaultKeyEncoding, VaultLogin,
    VaultRequest, VaultRequestMethod, VaultRequestReceipt, VaultSignatureEncoding, VaultSigning,
    VaultSigningAlgorithm, VaultSshTarget,
};
pub use vault_management::Item as VaultItemMetadata;

#[cfg(feature = "tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
pub use vm_host::{
    VmHostAllocationState, VmHostCommand, VmHostConnection, VmHostFence, VmHostProvision,
    VmHostRelease, VmHostScope, VmShape, connect_system_vm_host, validate_vm_factory_name,
};
