# nanocodex-anthropic

`nanocodex-anthropic` is an embedded compatibility adapter from Anthropic
Messages to Nanocodex's existing OpenAI Responses Tower contract. It owns
Anthropic authentication, OAuth refresh, HTTP/SSE transport, request
translation, and response normalization. The agent and managed-session layers
continue to use `nanocodex_oai_api::OpenAi`.

```rust,no_run
use nanocodex_anthropic::{Anthropic, load_anthropic_auth};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let openai = Anthropic::client(load_anthropic_auth().await?)?;
let mut session = openai
    .instructions("Preserve exact identifiers and answer concisely.")
    .build()?;
let completed = session.turn().create("Explain req_7f3.").await?;
println!("{}", completed.output_text());
# Ok(())
# }
```

The returned value is a normal `OpenAi` recipe whose custom in-process Tower
service translates complete Responses attempts to Anthropic Messages. No
loopback proxy or application-server protocol is involved.
