# Gateway models and mobile selection

The iPhone/iPad composer offers a model picker, thinking-effort control, Auto
button, and the resolved provider. Model and provider lock when the first message
is accepted. Auto remains opt-in. A manual Kimi/MiMo choice constrains the root's
model and effort, and chooses an available gateway on first input. Children can
choose their own routes. Explicit caller-supplied routing-policy restrictions
still apply to children.

| Canonical model | Gateway model | Providers | Routed efforts |
| --- | --- | --- | --- |
| `kimi-k3` | `moonshotai/kimi-k3` | OpenRouter, Vercel | low, high |
| `mimo-v2.6-pro` | `xiaomi/mimo-v2.6-pro` | OpenRouter, Vercel | low, medium, high |

Provider catalogs checked September 22, 2026. Kimi's additional upstream effort
levels are not exposed by the current three-level routing interface. These models
require a gateway; they never fall back to a ChatGPT subscription. Model and gateway
pins do not select an individual upstream endpoint behind OpenRouter/Vercel.

After the first message, the mobile effort control remains available only for
native Astra, which uses the existing appended, cache-preserving configuration
update. Routed conversations retain their initial effort. Routing does not promise
provider-side cache hits.

## Managed mobile API

Full account authorization is required:

```http
POST /v1/agents/AGENT_ID/routing
Content-Type: application/json

{"model":"kimi-k3","thinking":"high"}
```

An empty object enables automatic routing, preserving the existing API. Both
operations require an empty managed thread. Selecting a native OpenAI model
returns to manual account-model selection. Invalid combinations fail before state
changes; accepted history prevents changes. `/state` exposes `settings`,
`model_routing_enabled`, `model_routing_automatic`, and `model_route` with the actual
`model`, `backend`, and `thinking`. Pending routing has no resolved provider.

## Standalone Responses inference

The inference API accepts these canonical models and provider/effort candidate
IDs returned by `/v1/inference/models`. Existing inference keys retain inference-only
scope: no account connectors, Hands, CUA execution, or hosted agent tools. Function
calls are returned for the caller to execute; full Responses history, including
reasoning items and tool outputs, must be replayed. Gateway reasoning envelopes
in `encrypted_content` are opaque transport metadata, not a claim of encryption.

User image parts and image tool outputs accept HTTPS URLs or PNG/JPEG/WebP/GIF
base64 data URLs. Images have a 6 MiB encoded URL limit, requests an 8 MiB total
limit, and non-image input retains its 32 KiB limit. GLM's adapter remains text-only.

Managed agents use the existing authorized tool dispatcher, including Hands and
CUA. The adapter maps namespaced and free-form tools to gateway function calls,
preserves reasoning metadata, and returns screenshot observations in a supported
image-message format. OpenRouter MiMo currently rejects forced tool choice;
the adapter restricts the advertised tools and validates the resulting call
before dispatch. A missing or different forced call fails rather than executing
an unintended tool.

## Verification

- Real browser screenshot: both models on both gateways selected `Learn more`.
  The host executed the shared requested click through CUA and observed IANA's
  `Example Domains` page. All four provider/model cases consumed that real tool
  result and returned its verification nonce.
- Real WASM agents on Vercel: each model called a host tool which executed a fixed
  harmless native `printf`, then returned the observed nonce (two requests each).
- Regression coverage includes streaming reasoning replay, images from parallel
  tool calls, Cloudflare screenshot conversion, child routing, manual/auto admission,
  first-message locks, inference-only authorization, and the iOS model projection.

These smoke tests verify the tool plumbing; they are not a capability benchmark
or an exhaustive evaluation of every tool. Four diagnostic calls failed before
fixing MiMo forced-choice handling and streaming validation; those failures were
not counted as successful tests. Live trials used 16 provider requests in total.
