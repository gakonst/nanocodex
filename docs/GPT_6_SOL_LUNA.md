# GPT-6 Sol and Luna

The `sol` and `luna` aliases select `gpt-6-sol` and `gpt-6-luna`. Exact
`gpt-5.6-sol` and `gpt-5.6-luna` identifiers keep their original models,
including retained agent settings. Terra remains `gpt-5.6-terra`.

Both models support `none`, `low`, `medium`, `high`, `xhigh`, and `max`
reasoning effort, with `medium` as the model default. Explicit caller settings
remain authoritative. Responses supports `standard` and `pro` reasoning modes.
Pro's aggregated reasoning tokens use standard model rates.

| Model | Input | Cached input | Cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| GPT-6 Sol | $2 | $0.20 | $2.50 | $10 |
| GPT-6 Luna | $0.10 | $0.01 | $0.125 | $0.50 |

Rates are USD per million tokens. Above 272,000 input tokens, input and cache
rates double and output rates increase by 1.5x for the entire request. Fast
mode doubles the applicable rates. Tool charges are separate.

The API documents a 1,050,000-token context, up to 922,000 input tokens, and
128,000 output tokens. The Codex catalog uses a 272,000-token default context
and an 872,000-token maximum configured context.

Use Responses for tools with reasoning. Chat Completions function calling
requires reasoning effort `none` for these models.

Sources verified September 22, 2026:

- [Sol model reference](https://developers.openai.com/api/docs/models/gpt-6-sol)
- [Luna model reference](https://developers.openai.com/api/docs/models/gpt-6-luna)
- [Reasoning guide](https://developers.openai.com/api/docs/guides/reasoning)
- [Codex catalog](https://github.com/openai/codex/blob/49e95cc73f4eb2999b1d14f863c009168df6122b/codex-rs/models-manager/models.json)

Managed routing exposes all six efforts through native and Cloudflare Responses
transports. GPT-6 OpenRouter and Vercel routes are omitted from the managed
automatic catalog until their availability is verified. Explicit SDK Chat
Completions adapters require `none` effort for GPT-6 agent tools. OpenAI's model
reference does not establish availability or prices on third-party gateways.
