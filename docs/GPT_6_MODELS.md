# GPT-6 model integration

The supported OpenAI models are `gpt-6-astra`, `gpt-6-sol`, and `gpt-6-luna`.
The `astra`, `sol`, and `luna` aliases resolve to those IDs. GPT-5.6 Sol, Terra,
and Luna are not selectable models. Astra remains the default. Model selection
is fixed for an active conversation; existing provider checkpoints must not be
reused under another model. Retired model IDs remain intact in stored settings;
resuming a retired snapshot or rollout fails explicitly instead of switching models.

## Upstream contract

The Sol and Luna integration follows the Codex
[launch commit](https://github.com/openai/codex/commit/49e95cc73f4eb2999b1d14f863c009168df6122b),
including its
[model catalog](https://github.com/openai/codex/blob/49e95cc73f4eb2999b1d14f863c009168df6122b/codex-rs/models-manager/models.json),
[picker snapshot](https://github.com/openai/codex/blob/49e95cc73f4eb2999b1d14f863c009168df6122b/codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__model_selection_popup.snap),
and [catalog tests](https://github.com/openai/codex/blob/49e95cc73f4eb2999b1d14f863c009168df6122b/codex-rs/tui/src/app/tests/model_catalog.rs).
Sol and Luna each use an exact copy of their own upstream instruction template.
The [prompt manifest](../scripts/codex-parity/prompts.json) records their source
and hashes. Astra and the permission, voice, and goal prompts remain pinned to
`36430b36881cf5c289cb48e671cfc9e8b542ae7b`; only Sol and Luna use the launch pin.
Caller-supplied replacement and additive instructions remain supported.

Astra defaults to low effort; Sol and Luna default to medium. Explicit caller
effort wins. The public effort range ends at `max`; Codex's Sol `ultra` mode
requires orchestration beyond this model integration. Sol and Luna also retain
`none`, supported by the official [Sol](https://developers.openai.com/api/docs/models/gpt-6-sol)
and [Luna](https://developers.openai.com/api/docs/models/gpt-6-luna) API contracts.
Sol and Luna support standard and Pro reasoning independently of effort, following
the [reasoning-mode contract](https://developers.openai.com/api/docs/guides/reasoning#reasoning-mode).

The API context window is 1,050,000 tokens with up to 128,000 output tokens.
Nanocodex uses Codex's 272,000-token default prompt context and configurable
872,000-token maximum. Existing provider compaction manages retained context.

Responses Lite uses local Code Mode, all-turn reasoning context, and
`parallel_tool_calls: false`. Its image inputs omit `detail`, following
[upstream request construction](https://github.com/openai/codex/blob/49e95cc73f4eb2999b1d14f863c009168df6122b/codex-rs/core/src/client.rs#L859-L1002)
and [image normalization tests](https://github.com/openai/codex/blob/49e95cc73f4eb2999b1d14f863c009168df6122b/codex-rs/core/tests/suite/responses_lite.rs#L264-L335).
Nanocodex owns its execution tools, subagent lifecycle, and static model policy.
Upstream's dynamic catalog, Node REPL approval system, and model migration UI
are outside these contracts.

## Costs and service tier

Standard requests explicitly select `service_tier: "default"`; fast mode is
opt-in and uses the accepted `priority` wire value. This keeps the caller's
choice authoritative even when a provider catalog defaults to priority.

[Official pricing](https://developers.openai.com/api/docs/pricing), per million
tokens at standard short-context rates:

| Model | Input | Cached input | Cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| Astra | $10 | $1 | $12.50 | $50 |
| Sol | $2 | $0.20 | $2.50 | $10 |
| Luna | $0.10 | $0.01 | $0.125 | $0.50 |

Above 272,000 input tokens, the whole request uses twice the input and cache
rates and 1.5 times the output rate. Fast mode doubles those applicable rates.
Provider-reported usage drives result and trace estimates. API-equivalent
subscription estimates are not subscription charges. Historical measurements
retain their original model IDs and do not establish GPT-6 Sol or Luna performance.

## Live validation

Local subscription-authenticated SDK checks on September 22, 2026 completed tool
calls and follow-on turns for Astra, Sol, and Luna, retained each requested model
in snapshots, and reported `estimated_from_usage` costs at the rates above.
Sol and Luna also completed those checks with `none` effort.

The ChatGPT endpoint rejected Pro requests for both new models with
`unsupported_value` on `reasoning.mode`. The public Responses API documentation
lists Pro support; this subscription endpoint result does not establish API-key
availability. Nanocodex preserves the explicit API setting and surfaces provider
rejections without changing the requested mode or model.
