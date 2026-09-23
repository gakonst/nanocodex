# Inference performance investigation, 2026-09-22/23

The implemented inference, voice and CLI changes are on master. Production account, egress and managed Workers were verified on `a2f56cc296762a0ed395f536bd927b76963cd2c4` at 02:45:18 and 03:03:42 UTC on September 23. Both completed measurement cohorts retained stable Worker versions.

Start with [text measurements](summary-after-colocation.md), [voice connection measurements](summary-voice-after-colocation.md), and the [architecture](architecture.md). The raw API harness holds Astra settings fixed with routing disabled; it does not substitute ordinary GET latency for inference latency.

| Change | Verified outcome or limit |
|---|---|
| 15-minute discovery and startup-memory caches | All eight non-cold text admissions recorded 0ms versus 190–307ms previously; six have exact memory-cache-hit spans. Zero is clock resolution, not zero CPU or total API overhead. |
| Five-minute runtime/socket retention | Ordinary warm turns opened no new sockets. Both prior 36-second idle turns retained sockets; one current idle turn retried a send failure, so retention does not guarantee socket liveness. |
| Trusted original-ingress first-touch hints and seven regional relay applications | Text's exact instance was DFW; voice B's current instance was YVR. No SJC relocation claim. Existing broker identity remained unchanged. |
| Complete voice SDP answer returned in one RPC | All three captured voice calls used RPC and returned 201; only B has fine relay timing, including a 0ms body-read scope. No before/after voice speedup estimate. |
| Private voice sideband ownership reuse | Removes duplicate directory lookup while retaining live reconnect checks. Focused runtime tests passed; live native measurement used the data channel instead. |
| Inference-key admission and completion | One awaited initial pin/counter write; passive telemetry no longer delays response completion. Separate API from managed Astra measurements. |
| TUI startup/closing fixes | Both CLIs bound stalled tmux work; legacy removes blocking graphics detection and detached input-reader race. Matched silent-terminal median 1079.8→301.3ms; responsive-terminal results did not improve. |

Four ordinary warm text turns produced client WS TTFT of 1805–2761ms with no connection/retry work. Two other turns retried transport failures and reached 8057/9109ms TTFT. Voice connection readiness was 4599/2218/1669ms for three sequential receive-only starts. These small sequential cohorts support phase and behavior findings, not causal total-TTFT percentages or tail-percentile claims.

Implementation notes: [discovery and retention](discovery-cache.md), [memory and placement](colocation-and-memory.md), [separate inference-key admission](critical-path.md), [response completion](streaming.md). The implementation notes and earlier [post-TTL measurements](summary-after-long-ttl.md) retain the state and validation at their respective revisions; newer measurements above supersede statements that follow-up measurement was pending.

Focused runtime/contract suites and managed/account/egress typechecks passed; the final production WASM build was made from the exact deployed source. Native PTY tests verified draft input, successful exit, helper cleanup and terminal restoration for the CLI changes. Do not sum overlapping test batches into a unique suite count.

Operational limit: the manual managed-Worker deployment omitted optional source-map upload after repeated upload EPIPE failures. That version retains structured timing/log instrumentation but lacks the uploaded maps for mapped error stacks. The tracked normal deployment configuration still enables source maps; a later restoration was not verified. No causal upload-root-cause claim is made.

Remaining measured work includes the remote existing credential broker, cold discovery preceding owned preconnection, transport-close/retry diagnosis, missing client-to-voice correlation IDs and caller-side ownership timing, plus the gap between public regional placement controls and actual instance geography. Neither cached metadata nor a changed location hint safely migrates the authoritative credential state.
