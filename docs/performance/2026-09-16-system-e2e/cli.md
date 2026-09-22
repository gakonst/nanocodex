# Installed CLI greeting comparison

Twelve completed turns, three fresh/resume pairs per client, alternated by repetition. Prompt: `yo`. Both use `gpt-6-astra`, low effort, standard/default service tier. Settings returned by both clients confirm the requested model/tier; fast mode is off. Installed versions: Nanocodex2 0.6.1 (nightly recorded by hash) and Codex CLI 0.154.0.

[Individual timings, usage, event timelines and correlated Nanocodex server traces](cli-measurements.json).

Each measurement starts a **new client process**, including resume. Nanocodex2 uses `run`; Codex uses its installed app-server interface to expose actual incremental text deltas. The empty temporary working directory avoids project instructions. Existing personal client configuration remains loaded. Both greet without tool calls, but their instructions/tool context and provider caches are not identical; this is a product journey comparison, not an isolated provider A/B test.

| Client | Conversation | Process start→first text, median | Range | n |
|---|---|---:|---:|---:|
| nanocodex2 | fresh_session | 7,336 ms | 7,291–7,436 ms | 3 |
| nanocodex2 | resume_session | 3,997 ms | 3,419–5,275 ms | 3 |
| codex | fresh_session | 5,849 ms | 5,327–6,154 ms | 3 |
| codex | resume_session | 5,409 ms | 4,439–6,031 ms | 3 |

Nanocodex is slower for a fresh CLI journey in this small cohort, faster when resuming. Resuming Nanocodex preserves the remote agent runtime; restarting Codex app-server creates a fresh local process. Do not generalize this result to continuously running interactive TUIs.

## Nanocodex server versus client setup

| State | Accepted→model start (same agent event clock) | Model start→text (same clock) | Remaining process/setup/delivery residual |
|---|---|---|---|
| Fresh | 795 /528 /709 ms | 3,293 /3,422 /3,807 ms | 3,203 /3,386 /2,920 ms |
| Resume | 360 /321 /338 ms | 1,521 /1,543 /3,051 ms | 2,116 /1,555 /1,886 ms |

The residual is total process TTFT minus the accepted-to-text interval. It includes CLI initialization, agent creation or resume work, event connection and delivery. It is not a measurement of network latency alone. Exact turn traces separately show repeated account hosted-tool refresh during admission, as in the HTTP cohort.

For Codex, prompt submission→first text was 4,372–5,348 ms fresh and 4,242–5,432 ms on resume. This run records app-server event arrivals and token usage, but does not establish equivalent internal provider/credential-stage spans for Codex. None are invented from the client residual.

All twelve turns completed. Three scratch Codex threads were archived; all three Nanocodex agents were deleted. The temporary working directory was removed. Host load was falling during this later cohort, unlike the much busier earlier screen/voice window; do not treat differences between cohorts as changes caused by an optimization.
