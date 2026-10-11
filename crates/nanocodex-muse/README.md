# Nanocodex Muse

Muse Spark 1.3 over HTTP/SSE Responses, using the `nanocodex-agent` reference
loop and shared tool, transport, event and usage types. Muse owns its protocol
adapter, authentication, images and Claude-style summary compaction.

## Quick start

```rust,no_run
use nanocodex_muse::{Muse, Nanocodex};
# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let provider = Muse::builder(std::env::var("META_MODEL_API_KEY")?).build()?;
let (agent, _events) = Nanocodex::builder(provider)
    .workspace(std::env::current_dir()?)
    .build()?;
let result = agent.prompt("Say hello.").await?.await?;
println!("{}", result.final_message());
agent.shutdown().await?;
# Ok(())
# }
```

Defaults: `muse-spark-1.3`, low reasoning, `https://api.meta.ai/v1`, and
client-owned history (`store: false`). `.model(MuseModel::Contributor)` selects
`muse-spark-1.3-contributor`, which permits Meta training. Neither model supports
`Thinking::None`; Contributor also excludes `Thinking::Max`. Tools use mandatory
Code Mode, matching the reference harness.

## Authentication

Pass an API key as above, or use native device OAuth:

```rust,no_run
use nanocodex_muse::{Muse, auth::{MuseAuth, MuseLogin}};
# async fn login() -> Result<(), Box<dyn std::error::Error>> {
let login = MuseLogin::start().await?;
println!("Open {} and enter {}", login.verification_url(), login.user_code());
let manager = MuseAuth::new(login.complete().await?)?;
let provider = Muse::builder(manager.authorization()).build()?;
let credentials = manager.credentials().await; // Persistence is the host's choice.
# let _ = (provider, credentials);
# Ok(())
# }
```

`MuseCredential` exposes both the OAuth `access_token` and inference `api_key`.
`MuseAuth` keeps them in memory and recovers an unauthorized inference request
with one key exchange and retry. Call `credentials().await` after a turn to
retrieve updated credentials. Storage belongs to the caller.

## Images

`input::Prompt::content` accepts `UserInput::LocalImage`, `Image` and `ImageFile`.
User images, MCP screenshots and image tool results retain typed image content.

Image generation is disabled by default. On native targets,
`Tools::builder().image_generation(true)` enables `tools.image_gen__imagegen`
inside `exec`, calling
`muse-image-1.0` through Responses for generation and reference-image edits.
Results return as image content to Spark and are saved in the chosen workspace.
Transparency is unsupported; account access and quota determine availability.
See [Meta's image guide](https://dev.meta.ai/docs/image-generation).

## Configuration

The optional `nanocodex` facade's `muse` feature exports the provider and
`nanocodex::muse` lifecycle types. The crate-local `prompts/muse.md` adapts
[OpenCode's Meta prompt](https://github.com/anomalyco/opencode/blob/b9f3b382fcfd82b57103b29b77572f112ce9e1e5/packages/opencode/src/session/prompt/meta.txt);
attribution is in `THIRD-PARTY-LICENSES`. Override it with `.instructions(...)`.

Token usage and Spark cost estimates use Responses usage; context estimates
before a response are approximate. Muse Image's per-image fee is separate.

WASM callers provide `MuseBuilder::host_transport(...)`; the host must honor
`HostConnectRequest::responses_lite_headers()` and `additional_headers()`.
The existing JavaScript bindings do not expose Muse.
