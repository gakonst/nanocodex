# Inference critical path and placement

This investigation prioritizes fixed-model inference with routing disabled. It separates managed agent turns, which use the retained agent runtime and subscription Responses WebSocket, from the separate inference-key Responses API. Completion improvements to the latter are not evidence of faster managed Astra TTFT.

```mermaid
sequenceDiagram
    participant C as Client
    participant A as Account Worker
    participant M as Managed Worker
    participant S as Session Durable Object
    participant E as SessionModelEgress
    participant B as Credential broker DO
    participant R as Relay DO and container
    participant P as Provider
    C->>A: Initial HTTP/WS authentication
    A->>M: Service binding
    M->>S: Authorized session request
    Note over C,S: Later prompts can use the established client WebSocket
    S->>S: Admission, cached account discovery, runtime construction if needed
    S->>E: Private model connection, live local owner check
    E->>B: Resolve current credential
    B-->>E: Credential snapshot
    E->>R: Responses WebSocket upgrade
    R->>P: DNS / TCP / TLS / HTTP upgrade
    P-->>S: Model events through retained connection
    S-->>C: Durable event publication over WS/SSE
    Note over S,P: Warm turns reuse the provider connection; broker lookup is not per token
```

The service-binding boundaries are not equivalent to network round trips. Cloudflare documents that service-bound Workers run on the same server and thread by default. Ordinary Workers start near ingress; Smart Placement is an opt-in mechanism that may move fetch execution nearer a backend and does not apply to named entrypoints or RPC methods. A global Smart Placement switch is therefore not a remedy for the observed remote Durable Objects. Sources: [service bindings](https://developers.cloudflare.com/workers/runtime-apis/bindings/service-bindings/), [Worker placement](https://developers.cloudflare.com/workers/configuration/placement/).

Durable Object location is established on its first use and does not automatically follow a travelling user. A new location hint does not relocate an existing identity. Regional relay identities can avoid a legacy per-user relay anchor, but their hints remain best effort. Containers also have an independent location: startup chooses a nearby prefetched image, and controller/container colocation is not guaranteed. Sources: [DO location](https://developers.cloudflare.com/durable-objects/reference/data-location/), [container lifecycle](https://developers.cloudflare.com/containers/concepts/architecture/).

## Measured boundaries before these changes

- An exactly joined native trace places SessionModelEgress in LAX and its credential broker in MXP. The caller span was 241 ms; the broker method duration was 0 ms at the available clock resolution. This identifies a remote-state boundary, not expensive credential computation. Separate application measurements across eight calls were 188–253 ms. Cross-colo absolute timestamps are not used as a one-way network clock.
- The earlier 12-turn cohort measured cold TTFT 4.05/8.05 s, warm 1.41–2.54 s, and 36-second-idle TTFT 2.56–3.60 s. Idle reconnection cost 676–725 ms and readmission 188–364 ms. Preconnect preparation had a five-second lead outside the measured prompt interval; its 1.98/2.17 s TTFT is not free work.
- The later HTTP/WS plus simultaneous WS/SSE cohort, on a stable set of production versions, measured cold WS-delivered TTFT 5.20–6.11 s and successful warm TTFT 1.49–3.57 s. HTTP prompt acceptance was 111–204 ms. These small sequential cohorts are not matched causal estimates, and the client Mac was heavily loaded.
- One warm HTTP turn retried after output, producing two `42` deltas; its completion is not a clean successful sample. The separate HTTP completion cohort passed all four turns. HTTP cancellation returned 202 in 138 ms and the cancellation terminal arrived over SSE 133 ms after cancellation. A short WS cancellation raced with normal completion. Both cursor replay checks returned the exact expected sequence without duplicates.
- All three scratch agents in the later cohort were deleted and independently verified absent (404); tail collectors stopped. Deployment fingerprints and sanitized measurement data remain with the benchmark receipts. Ordinary state GETs were used for setup/cleanup, not as inference latency substitutes.

## Measurement changes

The account Worker previously had native tracing disabled, so existing egress traces could show the outgoing relay fetch but no incoming ChatGptEgress span. Native tracing is now enabled there at the same full sampling used for this investigation, allowing the relay controller boundary to join the trace. This increases observability volume; no per-token application logging is added.

Responses connections now carry generated correlation IDs across egress, relay controller and Node relay, stripped before provider forwarding. The relay records process age, DNS, TCP, TLS, write queue, first byte and completed HTTP upgrade timing, with one record per handshake. Controller timing includes whether its container was running. Credential instrumentation distinguishes activation phases, time waiting behind serialized operations, method work and refresh cause; refresh/revocation serialization remains intact. Worker clock zeros are not CPU profiles.

The account proxy joins HTTP admission, cancellation and WS/SSE establishment to the managed response request ID without consuming response bodies or logging credentials, query strings or prompt contents. Observation errors cannot replace the response. Focused validation: 27 egress tests, 9 relay tests and 19 account proxy tests passed; account and egress typechecks passed before the later placement change. Placement validation is recorded separately.

The idle fix retains existing discovery metadata for its original 120-second TTL and authority key instead of discarding it at the 30-second runtime teardown. This removes a repeated metadata read where the original snapshot is still valid. It does not cache provider credentials, extend authorization, or remove the provider reconnect. The separate inference-key path combines the first pin/admission storage write and removes passive telemetry from response completion; see `critical-path.md` and `streaming.md` for runtime evidence and limits.

A post-deployment comparison must record the actual versions of every participating service, separate the first regional-container start from reuse, preserve retries/failures as outcomes, compare server phases as well as client elapsed time, and keep model, reasoning, routing, prompt, setup and idle intervals fixed.
