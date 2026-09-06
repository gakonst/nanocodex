# Nanocodex `OpenAI` API

Tower-native building blocks for the `OpenAI` Responses API.

`nanocodex-oai-api` is useful without the Nanocodex agent loop. It owns the
typed request and response model, persistent Responses transport, replayable
Tower attempt boundary, and batteries-included conversation state.

## Quick start

Pass an `OpenAI` Platform API key to [`OpenAi::new`]. Developer instructions
create the stable boundary of a client-owned [`Session`], and follow-on calls
retain completed history automatically:

```rust,no_run
use nanocodex_oai_api::OpenAi;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let mut session = openai
    .instructions(
        "Remember user-provided deployment facts and say when information is missing.",
    )
    .build()?;

let mut turn = session.turn();
let completed = turn
    .create("The production deployment region is us-west-2.")
    .await?;

println!("{}", completed.output_text());
if let Some(cost) = completed.estimated_cost() {
    println!("estimated {}", cost.amount());
}
# Ok(())
# }
```

This crate supports `gpt-6-astra` (the default, with low reasoning),
`gpt-5.6-sol`, `gpt-5.6-terra`, and `gpt-5.6-luna`. Select a client default with
`OpenAi::builder(auth).model(Model::Terra)`. A session keeps that model for its
lifetime, and each replayable attempt retains it across retries. Changing
models would invalidate the provider checkpoint and require an inefficient
replay of the complete retained context.

API-key HTTPS OpenAI routing gateways may qualify those same closed model
identifiers with `OpenAi::builder(auth).model_id_prefix("openai")`. The prefix
changes only the wire model ID; model-specific reasoning, compaction, pricing,
and snapshots continue to use the typed [`Model`] value. It does not add an
alternate provider or arbitrary-model surface.

USD estimates require no pricing configuration. Each model applies its
published standard or long-context rates and its model-specific fast rates when
[`OpenAiBuilder::fast_mode`] is enabled. GPT-5.6 reports that tier as
`priority`; Astra reports it as `fast`. Terra, Luna, and Astra usage receive the
same complete estimate and status treatment as Sol. Provider-omitted usage
remains distinguishable as `usage_not_reported`.

## ChatGPT subscription login

> **Available on native targets.** The login and managed credential store are
> marked as such in docs.rs and are not compiled for WebAssembly.

[`auth::ChatGptLogin`] performs an authorization-code login with PKCE using a
loopback callback. The caller chooses the credential file, presents the
authorization URL, and waits for the browser callback. Successful completion
atomically writes the credential file.

The same file can then be loaded into a managed [`OpenAi`] client:

```rust,no_run
use std::path::PathBuf;

use nanocodex_oai_api::{
    OpenAi,
    auth::{ChatGptLogin, load_chatgpt_auth},
};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let auth_file = PathBuf::from(std::env::var("NANOCODEX_AUTH_FILE")?);

let login = ChatGptLogin::start(&auth_file).await?;
println!("Open this URL to sign in:\n\n{}", login.authorization_url());
let account = login.complete().await?;
println!("Signed in to ChatGPT account {}", account.account_id);

let auth = load_chatgpt_auth(&auth_file)?;
let openai = OpenAi::new(auth)?;
let mut session = openai
    .instructions(
        "Answer concisely. Preserve identifiers exactly and say when information is missing.",
    )
    .build()?;

let completed = session
    .turn()
    .create("Explain what deployment identifier deploy_01J8Y7Q2 refers to.")
    .await?;
println!("{}", completed.output_text());
# Ok(())
# }
```

Keep the credential file outside source control and reuse the same path on
later runs. It uses Codex's `auth.json` format, so Codex and multiple Nanocodex
processes can safely share the same path. The loader accepts both Codex OAuth
sessions and its `personal_access_token` format. OAuth sessions adopt a
same-account rotation from disk before refreshing; persistent Business and
Enterprise access tokens resolve their account metadata once and never enter
the OAuth refresh path.

Applications that receive a persistent `at-...` token directly can skip the
credential file:

```rust,no_run
use nanocodex_oai_api::{OpenAi, auth::chatgpt_access_token};

# fn run() -> Result<(), Box<dyn std::error::Error>> {
let auth = chatgpt_access_token(std::env::var("CODEX_ACCESS_TOKEN")?)?;
let openai = OpenAi::new(auth)?;
# let _ = openai;
# Ok(())
# }
```

[`auth::resolve_chatgpt_auth_status`] inspects either stored credential type
without exposing tokens, and [`auth::logout_chatgpt`] removes the stored
credentials.

A [`Response`] is also a typed stream. It retains the completed aggregate
after the stream reaches [`ResponseEvent::Completed`]:

```rust,no_run
use futures_util::TryStreamExt;
use nanocodex_oai_api::{OpenAi, ResponseEvent};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
let mut session = openai
    .instructions("Answer concisely and preserve exact identifiers.")
    .build()?;
let mut turn = session.turn();
let mut response = turn.create("Explain the identifier req_7f3.");

while let Some(event) = response.try_next().await? {
    if let ResponseEvent::OutputTextDelta(delta) = event {
        print!("{delta}");
    }
}

let completed = response.await?;
assert!(!completed.output_text().is_empty());
# Ok(())
# }
```

## GPT Realtime voice

> **Available on native targets with the `realtime` feature.**

[`OpenAi::realtime`] opens an independent GPT Realtime conversation using the
same credential and API base. Platform API keys use a direct Realtime
WebSocket. Managed ChatGPT credentials create the media call through the
ChatGPT backend and join its sideband control WebSocket with the same bearer
and account identity. When no host attestation is available, Nanocodex sends
the same unavailable-token envelope Codex uses when attestation generation
times out; hosts that own an attestation integration may override it with
[`realtime::RealtimeSessionBuilder::attestation_header`].
The library accepts and emits signed 16-bit little-endian, 24 kHz mono PCM through a cheap
[`realtime::RealtimeSession`] handle and an independent
[`realtime::RealtimeEvents`] stream. It does not open audio devices, so callers
can connect a microphone, files, or ordinary stdin and stdout pipes.
The experimental `nanocodex-voice` crate packages default desktop devices and
background-agent delegation without moving those policies into this transport
boundary.

Embeddings that already own WebRTC use
[`realtime::RealtimeSessionBuilder::connect_with_sdp`]. It creates the remote
call and returns a [`realtime::RealtimeSdpConnection`] immediately after the
answer SDP is available, while the authenticated sideband joins in the
background. The caller applies the answer and owns its peer and media for the
entire call. [`OpenAi::attach_realtime_call`] instead joins a call created and
negotiated elsewhere. Attachment defaults to Realtime V1, supports V3 when
selected explicitly, performs no call-create request, and sends no
`session.update`. Closing either external mode detaches Nanocodex's sideband; it
does not send `session.close` or terminate the caller-owned media call. See the
`realtime-external` example for both modes.

Both transports expose background-agent delegation as
[`realtime::RealtimeEvent::AgentRequest`]. An embedding handles that event with
its existing agent or tool loop, then calls
[`realtime::RealtimeSession::complete_agent_request`] with the typed result.
The `nanocodex` Ratatui consumer is one concrete desktop adapter: on macOS and
Windows, `/voice` connects the default microphone and speaker while preserving
the coding agent's normal history and lifecycle. `/voice list` prints the
available voice names, `/voice cove` starts a named voice, and `/voice off`
stops it. Managed ChatGPT sessions use Codex's current voice set and default to
`cove`; Platform sessions default to `marin`. The TUI uses either the coding
session's ChatGPT subscription credential or its Platform API key directly; no
second credential is required. Other native hosts can use the device-neutral
`realtime-pipe` example with their audio stack.

## Ownership and replay

A session owns authoritative typed history and one concrete Tower service.
A [`ResponseTurn`] marks a logical agent turn and keeps WebSocket turn-scoped
state stable across sequential `create` and `compact` calls. Only completed
operations commit. Healthy calls send a delta plus a private continuation ID;
reconnects replay complete committed history.

The higher-level `nanocodex-agent` crate decides *when* to compact and how to
execute tools. This crate implements the provider operation and atomic history
replacement without embedding agent policy.

## Attempt accounting

Transport metrics distinguish physical Responses attempts from retries. A sent
attempt that fails or is cancelled still emits `ModelAttemptFailed` with its
failure phase and retryability. Completed responses retain the provider-reported
usage used for normal cost accounting.

## Contract-only builds

The default `client` feature remains the complete OpenAI boundary, including
authentication, managed sessions, Tower services, transports, telemetry, and
pricing. Process companions that only need the dependency-light prompt,
response-item, and tool wire contracts may disable default features. This
keeps one canonical contract without linking an unused network client; it does
not create an alternate provider or transport implementation.

## Tools and managed sessions

The [`tools`] module defines the model-visible tool contract shared with
`nanocodex-tools`. A standalone [`Session`] does not run a tool loop or attach
a `nanocodex-tools::Tools` registry automatically. Use `nanocodex-agent` for
that batteries-included composition. Consumers implementing their own loop can
install definitions with [`SessionBuilder::tool_definitions`] and return paired
tool outputs with [`session::ResponseInput::items`].

[`tools::ToolDefinition::namespace`] represents the provider-native Responses
namespace shape for related function tools. Function output schemas remain
client-owned execution metadata: they are available through
[`tools::ToolDefinition::output_schema`] for Code Mode declarations but are not
serialized into the provider's function declaration.

## Going lower level

The crate root keeps the normal conversation path and shared input policy
prominent:
[`OpenAi`], [`Session`], [`ResponseTurn`], [`Response`],
[`CompletedResponse`], [`Prompt`], [`Thinking`], and their errors.

- [`session`] adds typed multimodal input, session identity, and explicit
  compaction results.
- [`responses`] contains the complete typed `OpenAI` Responses protocol.
- [`tools`] defines the shared tool contract; `nanocodex-tools` supplies the
  batteries-included runtime and implementations.
- [`auth`] owns API-key credentials plus native managed ChatGPT login,
  persistence, refresh, and logout.
- [`pricing`] and [`events`] expose automatic model-specific cost estimates and
  lifecycle-event components.
- [`realtime`] exposes native GPT Realtime PCM streams and typed voice events.
- [`tower`] contains the generic attempt, response, and retry contracts.
- [`transport`] contains WebSocket/HTTPS selection, replay policy, transport
  failures, and connection statistics.

## Custom Tower stacks

[`OpenAiBuilder::layer`] wraps each session's concrete service without boxing
it. [`OpenAiBuilder::service`] installs a fresh caller-defined
`Service<tower::ResponsesAttempt>` and is useful for custom transports,
deterministic tests, and controlled replay. The standard stack owns its retry
and reconnect policy; caller middleware should add deadlines, concurrency
control, tracing, metrics, or error mapping rather than a second retry loop.
Managed sessions own attempt construction and mutable transport state; callers
do not construct the standard service or transport requests directly.

Both methods change the builder's inferred concrete service-factory type.
Ordinary inline call chains need no type annotation. Application wrappers can
name or bound the generic result through [`tower::CallerServiceFactory`],
[`tower::LayeredServiceFactory`], and [`tower::ResponsesServiceFactory`]
without boxing the service stack.
