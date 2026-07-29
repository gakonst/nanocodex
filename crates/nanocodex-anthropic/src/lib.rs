#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

//! Anthropic Messages client, OAuth login, and Responses-model translation.

mod auth;
mod client;
mod login;
mod resolve;
mod service;
mod translate;
mod transport;
mod wire;

pub use auth::{
    ANTHROPIC_OAUTH_BETA, ANTHROPIC_VERSION, AnthropicAuth, AnthropicAuthError,
    AnthropicAuthFuture, AnthropicAuthMode, AnthropicAuthSnapshot, AnthropicAuthSource,
};
pub use client::{ANTHROPIC_MODEL, Anthropic, AnthropicBuilder, AnthropicError};
pub use login::{
    AnthropicLogin, AnthropicLoginError, AnthropicLoginStatus, AnthropicOAuthConfig,
    default_anthropic_auth_file, load_stored_anthropic_auth,
    load_stored_anthropic_auth_with_config, logout_anthropic, stored_anthropic_status,
};
pub use resolve::{
    AnthropicAuthStatus, AnthropicSetupError, anthropic_auth_status, load_anthropic_auth,
};
pub use service::{AnthropicResponsesService, AnthropicService};
