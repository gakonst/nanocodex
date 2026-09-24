# Regional inference measurements

`run.mjs` measures a configurable paired matrix of models, prompt families,
stream modes and caller endpoints. `core.mjs` parses response streams and records
first meaningful output, terminal completion and usage. `compare.mjs` pairs rows
while holding every dimension except the selected comparison constant.

Run the local checks and inspect an example plan without sending requests:

```sh
node --test scripts/regional-inference-bench/core.test.mjs
node scripts/regional-inference-bench/run.mjs scripts/regional-inference-bench/plan.example.json
```

Copy the example plan outside the checkout and set the run ID, current model IDs,
endpoints, credential environment-variable names, budget and new output directory.
The runner sends requests only with `--execute`; it refuses an existing output
directory, enforces its request/budget limits and stops on a failed or incomplete
response without retrying. Keep results outside tracked source.

`worker.mjs` and `wrangler.example.jsonc` provide the optional placed caller.
Configure a unique Worker name, target, model allowlist and future expiry in a
private copy. Supply `BENCH_TOKEN` and `INFERENCE_KEY` through secret bindings.
Worker placement and API ingress are separate observations; neither establishes
provider compute location.

Compare saved JSONL files, choosing exactly one of `stage`, `arm` or `stream`:

```sh
node scripts/regional-inference-bench/compare.mjs stage baseline streaming BEFORE.jsonl AFTER.jsonl
```

First meaningful output includes network, routing and generation time. Small
samples and sequential runs do not isolate a causal latency improvement.
