# Managed2 development

- Test behavior end to end through the public API Worker, real Session Durable Object, Egress2 service binding, and event stream. Use workerd with synthetic identities and a deterministic outbound provider, then a live authorized smoke run before rollout. Do not add unit tests for helpers, private methods, source text, or mocks.
- Define the user journey and failure/recovery cases before changing implementation. Keep a reproducible transcript of request, event cursor, turn status, and expected reply for every live test.
- Keep the new API independent of the existing managed API: never silently redirect or migrate its users, Durable Objects, or credentials.
- Avoid speculative defensive branches, catch-all fallback behavior, and arbitrary prompt/turn limits. If a boundary is needed for authentication, secret isolation, platform resources, or an upstream protocol, explain its concrete failure mode and exercise it through the Worker boundary.
- Never log, expose, or commit API keys, access/refresh tokens, credential hashes, or user prompts. Synthetic test data only.
