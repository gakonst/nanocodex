# Authenticated gateway verification — 2026-09-21

This follow-up uses the actual PR-built `nanocodex` and `nanocodex2` binaries, the PR gateway adapters, and authenticated provider inference. Each route pins GLM-5.3 with low reasoning. Selection is a deterministic fixture through the production router; these runs test gateway execution and durable history, not live Jev classification quality or the full model catalog.

## OpenRouter compatibility fix

OpenRouter's strict provider matching treats `parallel_tool_calls: false` as a required parameter. In live testing this excluded otherwise tool-capable endpoints: GLM requests failed with 429, and a separately admitted Luna route failed with 404. The adapter now omits this parameter only for OpenRouter's single-call mode. It keeps strict parameter matching, the selected model, and the reasoning effort. A buffered response containing multiple tool calls is rejected before any tools execute, and is recorded as a protocol failure. Explicit parallel mode still requires provider support.

## Cloudflare runtime compatibility fix

The actual managed Worker initially failed before provider dispatch: workerd does not support Fetch's `redirect: "error"`. The gateway adapter now uses `redirect: "manual"`, supported by both Node and workerd. Non-2xx responses, including redirects, still fail closed; credentials are not forwarded to the redirect target. A keyless replay established this failure and verified the proposed fix without spending inference credits.

## Native execution

Both OpenRouter and Vercel pass the same actual `nanocodex` task:

1. Read a synthetic marker and copy it to a result file using one shell command.
2. Verify the successful tool receipt, exact file contents, and model response.
3. Start a second CLI process using the same SQLite history and recall the marker without tools.

Each passing run makes three authenticated model requests, invokes the synthetic chooser once, and retains the same provider/model/effort. Native route admission is externally composed through the PR adapter; the CLI has no standalone routing-admission flag.

| Provider | Passing run | Provider requests | Reported model cost |
| --- | --- | ---: | ---: |
| OpenRouter | `native-2026-09-21T15-51-58.407Z-7fe99d53` | 3 | $0.012430000 |
| Vercel | `native-2026-09-21T15-51-58.681Z-22148bfd` | 3 | $0.014155960 |

These are provider-reported request charges, not task cost predictions. Vercel initially returned 403 because the model required paid credits; the funded run passed. No cross-model fallback was used.

## Managed execution

Both gateways also pass actual `nanocodex2` against the local PR Worker, real Rust/WASM, SQLite, and Just Bash. The first CLI turn executes the marker-copy command and checks its exact tool result. A second CLI process recalls that marker using conversation history only. Both durable turns complete, the route remains unchanged, and the chooser runs once per thread. No fallback or unexpected inference occurs.

| Provider | Passing run | Provider requests | Reported model cost |
| --- | --- | ---: | ---: |
| OpenRouter | `managed-2026-09-21T15-52-29.528Z-af832ced` | 3 | $0.003026612 |
| Vercel | `managed-2026-09-21T15-50-49.063Z-98bed634` | 3 | $0.006416520 |

The account identity, chooser, ancillary services, and never-allocated container cleanup are synthetic. Provider inference, adapter translation, CLI protocol, durable state, and tool execution are real. This verification does not replace the separate live Jev and child-idle-recovery results.

## Validation boundaries

The local helpers enforce fixed provider endpoints, a pinned model and effort, bounded request counts and timeouts. Early native and the first passing OpenRouter managed runs imposed a 2,048-token ceiling in the bridge; final runs preserve the production adapter payload without rewriting output-limit parameters. Failed intermediate attempts remain in the private local evidence. Credentials are supplied only to the native gateway bridge, never to CLI prompts or durable route metadata. Private evidence and credentials are excluded from Git.

The runs measure local Mac-to-gateway transport. They do not establish globally deployed Worker latency, TTFT, all-model compatibility, held-out task performance, or arbitrary crash recovery. Worker secrets have been configured; the PR code has not been deployed to production.

Focused validation: 39 gateway/Workers AI transport tests, 96 routing/controller/gateway tests, public package typecheck, and package integrity checks pass. A clean keyless Worker bundle and workerd replay pass. Regression tests cover single-call enforcement, protocol-error telemetry, explicit parallel mode, and redirect refusal on both gateways.
