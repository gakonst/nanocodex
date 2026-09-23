# Repeating the inference journeys

Run from the repository root on an authorized native client with existing Nanocodex credentials. The harness reads credentials only in memory, checks the exact production origin, creates at most two disposable fixed-Astra agents, runs eight 45-second turns (ten with the optional idle cases), compares WS and SSE delivery, exercises cancellation and cursor replay, and deletes each agent with a 404 verification. It records service deployment fingerprints before and after. Use a new output directory for every run; inspect its journal before retrying an interrupted invocation.

```sh
NC_DEPS_ROOT="$PWD" \
NC_OUTPUT=output/inference-after-unique-run \
NC_PHASE=after-unique-run \
NC_WRANGLER="$PWD/js/managed/node_modules/wrangler/bin/wrangler.js" \
node docs/performance/2026-09-22-inference/harness/measure.mjs
```

`NC_WRANGLER` must point to Wrangler's JavaScript CLI entry, not a shell shim: the collector invokes it through Node. Resolve that entry for the installed dependency layout before running. Set `NC_TRANSPORTS=http` or `ws` to limit the cohort to one transport. The default is both. No ordinary GET latency cohort is used; state/history reads serve validation and cleanup.

The collector allows timing fields and bounded correlation metadata only. It discards raw request headers, bodies, errors and query strings. It also retains deployment fingerprints, cursor sequences, outcome/retry events, synthetic generated text, client event-loop delay and host load; review generated reports before publishing. A stable version set is necessary but not sufficient for attributing a latency change: compare server phases, client load, provider variability and container warm state as well.

Set `NC_IDLE_MS=36000` to add one turn per transport after a measured 36-second pause following warm2. The harness sends transport pings during that pause, which do not request runtime preparation. It records the actual pause and then runs cancellation and replay as before. This adds two turns; all scratch resources still follow the same cleanup journal.
