# Inference after discovery caching and runtime retention

Measured 2026-09-23 01:51:40–01:54:05 UTC on source `ff442e28b94da2e2cd447936de62c4c39775a3a8`. Fixed Astra, low thinking, standard reasoning, fast=false; model routing and automatic routing disabled. Two scratch agents, ten turns, with simultaneous WS/SSE observation. Eight normal turns returned `42`; two cancellation turns ended cancelled. Both scratch agents were deleted and verified absent. No harness failures, normal-turn retries, reconnects or tool calls occurred.

The end-of-run deployment receipt read failed. A fresh provider receipt at 01:55:05 matched every starting deployment ID/version; retained tails also matched the expected versions. Preserve the failed raw receipt instead of rewriting it as success. Private, cohort-filtered artifacts retain exact joins and deployment receipts; operational account identifiers are not part of this document.

All numbers below are milliseconds. Server admission is acceptance to model call start. Connection is the remaining connection wait reported by the run; speculative preconnection overlaps admission, so it is not the full upgrade duration.

| Turn | HTTP acceptance | WS TTFT | SSE TTFT | Server admission | Connection | Model to delta |
|---|---:|---:|---:|---:|---:|---:|
| HTTP cold | 189 | 5784 | 5778 | 2003 | 585 | 3605 |
| HTTP warm 1 | 176 | 4434 | 4357 | 307 | 0 | 4039 |
| HTTP warm 2 | 154 | 9104 | 9103 | 222 | 0 | 8787 |
| HTTP after 36s idle | 461 | 2073 | 2035 | 219 | 0 | 1751 |
| HTTP cancel | 303 | 2720 | 2680 | 221 | unavailable | 2244 |
| WS cold | — | 5237 | 5235 | 1756 | 900 | 3001 |
| WS warm 1 | — | 3602 | 3592 | 193 | 0 | 3330 |
| WS warm 2 | — | 3454 | 3452 | 190 | 0 | 3241 |
| WS after 36s idle | — | 3920 | 3886 | 205 | 0 | 3638 |
| WS cancel | — | 1967 | 1949 | 194 | unavailable | 1728 |

Both 36-second idle turns retained the runtime and provider connection: zero new connections, discovery hits, and zero recorded runtime/bootstrap preparation. In the older fixed-model cohort all four 36-second idle turns reopened a connection, taking 676–725ms. Discovery cache ages plus remaining TTL equal 900,000ms on every observed hit. This establishes retention behavior; the experiment does not test the full TTL or isolate a causal TTFT change.

Warm admission still took 190–307ms, entirely in context injection. Cold injection took 1,077–1,339ms after overlapping catalog/vault discovery (650–679ms). Source inspection found that Markdown startup snapshots fetched personal/team memory on every turn even when duplicate context appends were suppressed. These measurements precede the new memory-bootstrap cache and finer injection spans.

Exact foreground correlations show full cold upgrades of 1,840/1,962ms, with credential caller intervals 1,255/207ms and relay upstream intervals 585/1,755ms. Both containers were already running. Native tracing located one egress invocation in SJC, its credential broker in MXP, and the relay controller in DEN. The corresponding container instance was physically in DFW despite a WNAM controller hint. The broker outlier's application queue/method timers were zero while its platform invocation wall was 1,077ms; this does not establish network, CPU or decryption as its cause. Full upgrade timing must not be added to admission because preconnection overlaps it.

WS and SSE observed identical events, text, terminal state and cursors for every turn; they are two observations of ten turns, not twenty samples. Cancellation reached SSE/WS terminals 87/166ms after the HTTP request and 120/159ms after the WS request. Cancelled runs have no completed-run connection summary.

Cold admission was higher than earlier observations (1,756–2,003ms versus 645–735ms), and warm admission was higher than the earlier recorded zero. The 9.10s warm outlier is retained: 8,787ms was model-to-delta despite no connection work. Client load and other upstream source changes differ substantially between cohorts. These data support structural cache/connection conclusions, not a naive overall TTFT speedup or slowdown estimate. Provider compute and upstream delivery are not separately measured by model-to-delta.
