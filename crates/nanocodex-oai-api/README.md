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

USD estimates require no pricing configuration. This crate supports only
`gpt-5.6-sol` and applies OpenAI's published standard rates, or its priority
rates when [`OpenAiBuilder::fast_mode`] is enabled. If the provider omits
usage, [`CompletedResponse::estimated_cost`] returns `None`.

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
processes can safely share the same path. [`auth::load_chatgpt_auth`] adopts a
same-account rotation from disk before refreshing, refreshes expiring
credentials, and recovers an unauthorized request once with the refreshed
authorization.
[`auth::chatgpt_auth_status`] inspects the selected account without exposing
tokens, and [`auth::logout_chatgpt`] removes the stored credentials.

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

## Ownership and replay

A session owns authoritative typed history and one concrete Tower service.
A [`ResponseTurn`] marks a logical agent turn and keeps WebSocket turn-scoped
state stable across sequential `create` and `compact` calls. Only completed
operations commit. Healthy calls send a delta plus a private continuation ID;
reconnects replay complete committed history.

The higher-level `nanocodex-agent` crate decides *when* to compact and how to
execute tools. This crate implements the provider operation and atomic history
replacement without embedding agent policy.

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
- [`pricing`] and [`events`] expose automatic `gpt-5.6-sol` cost estimates,
  explicit unpriced-provider status, and lifecycle-event components.
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
