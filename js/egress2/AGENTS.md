# Egress2 development

- Test complete Worker/credential-Durable-Object/outbound journeys in workerd and the authorized relay route in a live smoke run. Do not add unit tests that mock `DurableSubscriptionStore`, `OwnerSubscription`, routing helpers, or private internals.
- Cover owner isolation, secret substitution, refresh/rotation, failed replacement preserving the old credential, WebSocket upgrade, and denied routes at the service boundary. Use synthetic tokens; inspect durable state only when proving at-rest secrecy or recovery.
- Do not add speculative error-swallowing, fallback routes, or arbitrary account/credential limits. Preserve narrowly justified security and platform resource bounds, documenting the failure they prevent. A missing relay or credential must fail closed.
- Never make Egress2 publicly routable or emit credential values, owner IDs, prompts, upstream headers or response bodies in logs.
