# Voice connection setup after regional relay deployment

Three receive-only native WebRTC starts completed on 2026-09-23, 02:50:00.811–02:50:26.808 UTC, against source `a2f56cc296762a0ed395f536bd927b76963cd2c4`. Start/end versions matched the text cohort. Readiness was **4599.065, 2217.921 and 1668.823ms**. This is a post-deployment baseline, not a measured before/after speedup.

The test used a genuine receive-only peer and `oai-events` data channel. Assertions confirmed zero audio bytes sent, unchanged microphone authorization and successful no-op session.update acknowledgements. It sent no microphone/input audio, requested no speech and delegated no model work. Connection readiness is distinct from first spoken audio. The existing compiled Apple test bundle was reused; its selected test source matched, but the full client was not rebuilt from the measured server commit.

## Client timing

Milliseconds measured on the same client clock. Each column is one sequential start on the owned scratch agent. Admission, SDP calls and event preparation overlap; do not add them.

| Scope | Start 1 | Start 2 | Start 3 |
|---|---:|---:|---:|
| Test readiness | 4599.065 | 2217.921 | 1668.823 |
| Peer preparation | 90.056 | 5.830 | 5.477 |
| Voice admission request | 1262.579 | 344.820 | 234.117 |
| SDP call | 3746.185 | 1478.317 | 890.751 |
| Apply answer → WebRTC connected | 532.737 | 513.622 | 520.623 |
| Session update → acknowledgement | 55.452 | 78.826 | 56.621 |

SDP negotiation finished after admission in every start. The first local peer setup took 90ms; later setups took about 6ms. Approximately 514–533ms remained between applying each answer and the WebRTC peer connecting.

## Server timing

Three exact voice chains were recovered for the owned agent. Ownership → egress → credential resolution uses exact voice, subject and resolve IDs. The client timing log lacks voice/request IDs, so matching client starts 1/2/3 to chains A/B/C is an **ordinal inference**, supported by three sequential starts and no retry marker. Managed route/handler measurements below are likewise ordered within the owned agent, rather than exactly joined to a voice ID.

| Scope (ms) | Chain A | Chain B | Chain C |
|---|---:|---:|---:|
| Managed SDP calls route | 3686 | 1252 | 753 |
| Managed voice-start route | 1127 | 108 | 100 |
| Session admission handler | 826 | 0 | 0 |
| Egress total | 3305 | 732 | 702 |
| Credential caller wait | 167 | 171 | 166 |
| Egress relay/provider interval | 3138 | 561 | 536 |
| Account relay RPC | unavailable | 512 | unavailable |
| Node fetch | unavailable | 451.39 | unavailable |
| Node response wait | unavailable | 446.48 | unavailable |
| RPC answer-body read | unavailable | 0 | unavailable |

All three calls returned 201 over the enabled relay RPC, with no credential-recovery branch recorded. The first admission had a catalog/MCP discovery miss taking 826ms; subsequent admissions hit the cache and recorded 0ms. Broker method/queue/activation scopes recorded 0ms while caller waits remained 166–171ms. These coarse clocks do not establish zero CPU, and the caller interval cannot be divided into network, platform scheduling and execution from these records. End-to-end ownership RPC time remains unmeasured; the local ownership scope is insufficient.

Only chain B has retained controller and Node timing detail. Its relay was already running, its upstream socket was reused, and process age at fetch completion was 4136.65ms. Socket wait was 3.17ms, upload 1.18ms and response wait 446.48ms. The 512ms controller, 451.39ms Node and 446.48ms response scopes are nested. First-call slowness alone does not prove a cold container; A/C container state is unknown.

## Placement and transport changes

Trusted initial ingress now guides first-touch Session and credential-broker placement. SDP creation selects a regional voice application and returns the complete answer in one RPC. Private HTTP sideband reuses managed live ownership validation, avoiding a duplicate Session lookup; reconnect still rechecks ownership. Sideband goes directly from egress to the provider, preserving the upgraded response and call identity.

Normal Cloudflare instance inspection places the exact captured B controller's current instance in **Vancouver (yvr01)**, in the same WNAM-constrained application whose text instance is in Dallas. A matching dashboard event confirms the exact controller, RPC entrypoint, version and timings. This is current instance evidence after the cohort; it is not an independent cohort-time physical-colo log. No SJC placement or causal geographic speedup is established.

The live native test exercises the data channel, not HTTP sideband. Sideband has focused runtime/protocol coverage, including denial/reconnect, pinned accounts, credential recovery, exact upgrades and ambiguous-failure behavior. Negotiated media/ICE geography was not measured.

Three client taps and three exact server call chains were observed, with no retry markers. The harness permits one media retry per iteration, so its configured maximum was six provider sessions. The test exited successfully, deleted its scratch agent and received a confirming 404; tail collection stopped. Private metadata-only artifacts preserve all 123 client stages, exact cohort joins, source hashes and deployment receipts, without audio, SDP, credential values or global account-tail publication.
