# nanocodex2: actual terminal TTFT remeasurement

## Interactive result

The newly built release measured **2.54 s median Enter-to-first-terminal-text for
the first `yo`, and 2.07 s for a follow-up in the same process**. The installed
nightly measured 2.93 s and 2.47 s. Each cell has three samples; this is a small
live comparison with provider variance, not a latency guarantee.

| Terminal measurement | Installed nightly | New release build |
| --- | ---: | ---: |
| First `yo`, median | 2.929 s | 2.545 s |
| First `yo`, range | 2.644–3.896 s | 2.121–4.831 s |
| Follow-up `yo`, median | 2.474 s | 2.071 s |
| Follow-up `yo`, range | 1.657–4.426 s | 1.510–2.107 s |
| Attach → connected composer, median | 0.830 s | 1.210 s |

These are real CLI processes in a pseudo-terminal, typing `yo` and Enter. The
first prompt followed a five-second pause after the connected composer appeared.
Each CLI attached to a fresh, explicitly configured agent; creation happened
outside this interactive timing. The follow-up kept the same process and session.
Readiness required leaving the Connecting state, verified in all six transcripts.
The slower readiness median in the new build is retained; this run does not
establish its cause or claim startup got faster.

## Confirmed component change

Both clients already had a prepared model connection after sitting open. Every
interactive turn reported **0 ms connection cost**, so the earlier API-only
event-socket versus preparation result is not the CLI's incremental gain.

The installed CLI's first turn still fetched account vault metadata during
admission: **197 / 211 / 217 ms**. Its catalog and hosted-tool snapshots were
already reusable. The new CLI automatically issued `/prepare` once per session,
received 202, and reused startup account metadata. Its three first-turn
admissions reported **0 ms** at Worker timer resolution, with no vault-read span.
All follow-up admissions in both clients also reported 0 ms.

| Component, median | Installed first turn | New first turn | New follow-up |
| --- | ---: | ---: | ---: |
| Model call → first output | 2,412 ms | 2,383 ms | 1,971 ms |
| Terminal TTFT minus model first-output duration | 487 ms | 162 ms | 100 ms |
| Admission, nested in that residual | 211 ms | 0 ms | 0 ms |
| Model connection, nested in model call | 0 ms | 0 ms | 0 ms |

The strongest supported incremental saving is the roughly **200 ms metadata
wait removed from first-turn admission**. The observed median TTFT reduction is
about 0.4 s, but the whole difference cannot be attributed to this hook: these
are different client revisions/profiles, provider timings vary, and the sample
is small. Medians and overlapping rows must not be added together.

The largest remaining warm interval is the model call. For the new build it
ranged from 1.912–4.670 s on first turns and 1.261–2.027 s on follow-ups. The
4.831 s terminal outlier included 4.670 s in that model span and is retained.
These traces do not split provider queueing, inference and upstream transport.
No tool calls or response retries occurred in the twelve interactive turns.

## Fresh one-shot launch still costs much more

Separate diagnostic runs of `nanocodex2 run yo` started a new process and created
a new agent, with no advance preparation interval. There is only one sample per
binary; these are diagnostics, not a before/after regression verdict.

| Boundary | Installed | New release |
| --- | ---: | ---: |
| Process launch → first stdout text delta | 9.629 s | 10.698 s |
| Process launch → agent-ready receipt | 3.641 s | 5.801 s |
| Turn admission | 679 ms | 959 ms |
| Model call → first output, including connection | 5.244 s | 3.791 s |
| Model connection, included above | 1.911 s | 1.425 s |

In the new release's exact Durable Object trace, turn admission started at
`1789607635452`; the background preparation stage started at `1789607636170`,
**718 ms later**. This is ordering within the same object, not a cross-clock
latency subtraction. Sending immediately over the already-open agent socket
beat the separate preparation HTTP request. That request used live authorization
(191 ms) and its managed route took 648 ms, including nested work. Admission
shared the in-flight discovery but still waited for it. No retry occurred.

The largest observed new-launch interval was the **5.80 s process-to-agent-ready
boundary**, ahead of the 3.79 s model first-output span. The startup boundary
includes local setup, authentication, creation and socket readiness; current
CLI telemetry does not fully split those costs. The root create-live request
has no correlation ID in these logs, so the time-window-associated 1.36 s
managed-route duration is a candidate span, not proof that the remaining gap
belongs to one component. This is the next startup boundary to instrument.

## Method, provenance and limits

- Prompt `yo`; model `gpt-6-astra`, low effort, standard reasoning, fast mode off.
  Agent settings and model telemetry were retained. Temporary workspaces were
  empty. User-wide hosted context and tool definitions were still present.
- Three alternating installed/release pairs, two turns per session. The release
  compiler completed before timed trials. Ambient system/provider load was not
  controlled; local load averages are retained.
- Enter and terminal output use the same Python monotonic clock. ANSI output is
  replayed into a terminal emulator; the first newly appearing assistant line
  must match the first assistant text delta and increase its baseline count.
  All twelve matches were found. This measures terminal output, not physical
  monitor scanout. The independent observer is a separate delivery path, so its
  difference from terminal timing is not a pure rendering-cost measurement.
- Server admission and transport telemetry are matched by exact turn ID and
  Durable Object ID. No SQL audit was enabled; named read counters are retained,
  and 0 ms synchronous timings do not imply zero CPU cost.
- A setup probe timed out because its harness expected an `Enter send` footer
  omitted at this terminal width. It submitted no turn. The corrected pilot
  submitted one turn during compilation; that sample is excluded from results.
- All ten scratch agents (six interactive, two launch, two setup/pilot) returned
  404 after deletion. Trace collectors are stopped.
- No direct Codex, voice/audio or physical mobile benchmark was rerun here.

The installed binary is nightly `92528e17b4feb63fed238abc2f2b766777fc24ff`, SHA-256
`3e3ccc3e3093f9b7049b31e03d6fa1b6099bda356674b60d1b94993bdde35ad0`.
The new optimized release is built from `68a7f4edc39dfdd35388270160c0ecf935848658`
plus the then-uncommitted preparation patch, SHA-256
`f9a76391a2b491c95aaa65cc08cb4721da9096292bc38c74d49eddcaa3786117`.
Both print version 0.6.1; the hashes distinguish them.

The build command was `cargo build --release --locked -p nanocodex2-bin --bin nanocodex2`.
The resulting binary is `target/release/nanocodex2` in this worktree.
**The installed launcher still points to the nightly; no installation pointer,
deployment, commit or push was changed in this measurement task.**

Both arms used managed Worker `4f6c836f-921d-475c-a4a5-64906c1a7bb9`. Account,
egress, connect and managed deployment receipts stayed unchanged across the
measured cohort and each launch diagnostic.

## Evidence

- [Interactive measurements and component spans](measured-measurements.json)
- [Correlated interactive server traces](measured-traces.json)
- [First-text terminal transcripts](terminal-first-text.txt)
- [Fresh-launch measurements](launch-measurements.json) and [traces](launch-traces.json)
- [Binary/source provenance, receipts and cleanup](manifest.json)

Reusable harnesses, raw PTY frames, sanitized tails and the release build log
are in `output/nanocodex2-ttft-20260916/`. The harness uses the existing local
account credential without printing it and cleans up its scratch agents.
