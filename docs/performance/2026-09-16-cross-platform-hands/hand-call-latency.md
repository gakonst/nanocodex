# Hand call and attachment routing — September 16, 2026

## Before changes

Managed Worker version: `3184239f-2d34-4254-b9b4-adc4a7f2738f` (deployed
at 21:29:43 UTC). Failed local upload attempts did not change the live version.

Five sequential native Linux `printf hand_latency_probe` calls completed in
**440 / 407 / 418 / 404 / 405 ms**, with **5.33 / 5.21 / 4.94 / 4.72 / 4.89 ms**
in the host command. The matching account-broker invocation cohort took
**187 / 188 / 182 / 182 / 182 ms**, with **3 / 2 / 1 / 1 / 1 ms** of Worker CPU.
The baseline broker correlation is by time/order, not a shared call identifier.
These nested measurements cannot be added together.

A warm VM attachment at 21:48:45 UTC spent **282 ms** in `session.GET /tool-host`.
That interval was occupied by account Hand discovery (**282 ms**) and account
MCP discovery (**166 ms**) running concurrently. The session already had a live
router: it was executing the mount that caused this attachment. WebSocket connection took **1,305 ms** and catalog acknowledgement **222 ms**. Discovery is one part of the connection interval,
not an explanation of all of it.

## Changes

- Attachment admission reuses the live router and its dynamic catalog validator.
  It no longer rediscovers unrelated account tools before accepting a VM socket.
  First construction still loads discovery. Ordinary turn startup still refreshes
  account tools. Deletion, export, shutdown, lease and generation checks remain.
- SQL call transitions return their updated row with `UPDATE ... RETURNING`.
  This removes two follow-up SELECT queries from a successful dispatch/result cycle.
  A failed state comparison still reads the retained row, preserving duplicate,
  replay and conflict handling. This is a query-count reduction; no latency
  improvement is inferred from it.
- Broker/provider timing logs correlate source and transport call IDs without
  logging arguments, outputs, authorization or capability URLs. The admission,
  dispatch-to-result, settlement, account handler and provider HTTP spans are
  nested. Broker dispatch-to-result includes transport and any durable output
  gate wait; it must not be labeled pure network time.
- Native attachment traces separate catalog preparation, trust preparation,
  DNS, TCP, TLS/HTTP upgrade and catalog acknowledgement. Successful upgrades
  return a request ID while preserving the upgraded socket.

Workers clocks advance only after I/O. A reported 0 ms synchronous SQL/admission
span does **not** measure its CPU cost. Use invocation CPU metadata for that
question. [Cloudflare timer documentation](https://developers.cloudflare.com/workers/runtime-apis/performance/).

## Verification

Focused broker/account tests passed (54), including diagnostic sink failure and
exact receipt replay. The real SQLite transition test and account suite passed
(17): successful transitions do not re-read, failed transitions return retained
completed results, and missing rows remain absent. Broker/VM pool/timing checks
passed (49), including live catalog replacement, generation fencing and preserving
101 sockets. Managed TypeScript checking and the Worker bundle dry-run passed.

[Curated measurements](hand-call-latency-measurements.json) contain the baseline
tool results and sanitized Worker events. Raw local logs live in
`output/hand-latency/`. The post-deployment cohort uses the explicitly labeled integrated-master
`3b51b5fa` deployment, managed version `dd307c9e-d2aa-4752-af53-ea1559716905`,
after deploying its egress dependency `54233409-b942-47ea-bae2-b27361c7b3e6`.
The upload succeeded using the pinned Node 24 runtime with IPv4-first DNS
ordering; earlier attempts failed with EPIPE. This does not isolate which
of runtime, address ordering or transient network behavior caused those errors.


## Post-deployment results

The Linux host/executor binary was unchanged for this comparison. Five new
scratch-agent calls all returned the exact marker and exit code zero; the
scratch agent was deleted after verification.

| Nested measurement (ms) | Call 1 | Call 2 | Call 3 | Call 4 | Call 5 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Managed tool result | 446 | 456 | 427 | 436 | 447 |
| Provider HTTP fetch | 446 | 456 | 427 | 436 | 447 |
| Account handler / broker dispatch-to-result | 159 | 159 | 158 | 157 | 159 |
| Host command | 4.46 | 4.97 | 4.90 | 5.07 | 5.92 |
| Provider minus account-handler span | 287 | 297 | 269 | 279 | 288 |

**There is no measured end-to-end native speedup.** The five-sample median was
407 ms before and 446 ms after. The SQL change removes two reads, but does not
justify a user-visible latency claim.

The provider already calls its bound account Durable Object directly. There is
no extra public account Worker or API-key authorization hop to remove in this
path. JSON response decode reports 0 ms; the account's ownership and tool
resolution stages also report 0 ms with the timer limitation above. The
269–297 ms residual is at the provider/broker boundary, including routing and any outgoing durability gates. It is
not established to be pure wire latency, geographical placement, or SQL.
[Cloudflare output gates](https://developers.cloudflare.com/durable-objects/best-practices/rules-of-durable-objects/)
hold outgoing requests/responses until pending writes commit.

A post-deploy warm VM used request ID
`37143c8a-8e6e-454a-81ca-56a7bfd2f3d7`. The matching public managed
attachment route took **318 ms**, including **25 ms** grant validation.
Post-deploy attachment scopes report **0 ms** router setup and no account
discovery; their opaque Durable Object IDs were not conclusively mapped to this
specific VM, so they are separate supporting observations.
The host's WebSocket connection took **979 ms**, followed by **252 ms** catalog
acknowledgement, versus the earlier 1,305 ms connection/222 ms acknowledgement.
The complete managed mount still took **3,979 ms**, so this first post-deploy
sample is not evidence of an overall mount speedup.

A second warm mount on the same managed version and factory binary repeated
**983 ms** WebSocket connection and **219 ms** catalog acknowledgement,
**1,203 ms** tools attachment overall. Its correlated request
`f7f99f62-e4ab-401a-b0a7-0b8a898a56a7` spent **241 ms** in the managed attachment
route, including **9 ms** grant validation (Worker wall 250 ms). DNS/trust/TCP
accounted for **141 ms** of the host's connection interval. Desktop attachment
ran concurrently and completed in **1,195 ms**. Local VM claim took **0.8 ms**.
The complete mount took **3,096 ms**, leaving approximately **1,893 ms** outside
the factory attachment span. This remains slower than the earlier **2,955 ms**
complete baseline mount. The account frontend had deployed version
`c627a2a8-6dc6-475e-af81-22fecdc2bbb6` before this repeat; the managed Worker and
factory remained unchanged. Both scratch VMs were deleted after verification.

## Remaining time, ranked

- **VM mount outside factory attachment: about 1,893 ms** in the repeat. This is
  the largest unresolved mount interval; measure reservation, factory command
  delivery and result delivery separately before selecting a change.
- **VM socket setup: 983 ms**, including 141 ms trust/DNS/TCP and 241 ms in the
  managed request. These spans are nested; about 600 ms remains elsewhere in
  TLS, HTTP/frontdoor and response delivery. Catalog acknowledgement adds 219 ms.
- **Native provider/broker boundary: 269–297 ms** per call, outside the handler.
  Direct bound-DO fetch already avoids a public HTTP/auth hop. Distinguish
  Durable Object routing from output-gate/storage waits before changing transport.
- **Native broker dispatch-to-result: 157–159 ms**, containing a **4.5–5.9 ms**
  command. This includes socket transit and durability gates; command execution
  is a small share. SQL read count improved, but the traces show no end-to-end
  native speedup.

After merging the integrated source, all **68** Hand/account/auth/timing tests
and managed TypeScript checks passed again. The native connection tracing changes
passed **14** attachment tests and **37** host lifecycle tests in the Linux build.
