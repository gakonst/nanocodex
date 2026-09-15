# Managed configuration validation — 10 September 2026

[responses-lanes.json](responses-lanes.json) records a live run of
`js/nanocodex/scripts/response-lanes.bench.mjs` against OpenAI Responses using
GPT-5.6 Luna, low reasoning, no tools, `store=false`, and 128 output tokens.
The credential stayed in process memory; no credentials are included here.

All six output checks passed: two independent lanes, a cross-lane fork, a parent
continuation, and explicit cache write/read. Socket establishment took 1.219 s.
Completion after submission ranged from 0.693 to 0.967 s. The cache requests each
had 3,022 input tokens: the first reported 3,008 cache-write tokens and the second
reported 3,008 cache-read tokens. These are smoke measurements from one socket,
not throughput or tail-latency estimates and not a live Nanocodex comparison.

At the published Luna Standard rates ($0.20 ordinary input, $0.25 cache write,
$0.02 cache read, $1.20 output per million tokens), these six records price to
$0.00089176. This is an estimate for the retained records, not the account bill.
Two earlier attempts completed four lane generations each before a local adapter
composition error; their usage was not retained. The adapter now has a regression
test for that composition. No Agents sessions or sandboxes were created by this
script, and its sockets were closed. [OpenAI pricing](https://developers.openai.com/api/docs/pricing)

Local validation of the implementation:

- 103 focused Worker tests: configuration, settings, Hosted Tools, turn admission,
  large turn inputs and cron triggers. The configuration tests use real SQLite,
  durable workspace storage and embedded-shell setup; they cover signed retry,
  rollback, ownership, immutable bytes and deletion guards.
- 80 SDK/runtime tests, SDK declaration checks and package artifact checks.
- Managed Worker and account TypeScript checks; release WASM build.
- Account production build and docs check; managed Worker dry-run bundle with
  `--containers-rollout none` (unchanged native Docker images excluded).
- Browser inspection of the real inspector component against fixture HTTP data:
  usage, unknown accounting, child request details, output metadata, timeline,
  refresh and close. This is not an end-to-end deployed account test.

The implementation and remaining boundaries are documented in
[managed agent configuration](../../docs/MANAGED_AGENT_CONFIGURATION.md).
