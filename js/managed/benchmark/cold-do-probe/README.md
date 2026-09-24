# Cold Durable Object activation control

This opt-in synthetic benchmark compares the existing **admin-only** managed
`key-unique` activation probe with a minimal separate Worker exporting the same
`activationProbe()` method as `ApiKeyRecord`. Neither target creates a managed
agent; both allocate fresh `newUniqueId()` DOs and delete their probe storage.
The isolated Worker is not part of production deployment or normal CI.

The Worker refuses every request until `BENCH_TOKEN` is configured. On a test
account (or with an explicitly chosen unique script name), from this directory:

```sh
npx wrangler deploy --config wrangler.jsonc
# Supply an independently generated high-entropy token via stdin, never in a command argument.
npx wrangler secret put BENCH_TOKEN --config wrangler.jsonc
# Configure NANOCODEX_PROBE_API_KEY, NANOCODEX_COLD_PROBE_TOKEN and
# NANOCODEX_COLD_PROBE_URL privately in your environment.
node compare.mjs
```

Do not paste actual credentials into chat, source files, shell history or
artifacts. The admin key must belong to the configured admin user and carry
`agents:write`. Results contain only timings, HTTP statuses and ingress colo.
Default is 24 interleaved pairs, maximum 48; the script never retries an
ambiguous write. Remove the temporary Worker and its probe namespace after
measurement; never delete an existing production DO class.

`dispatch_ms` is caller-side monotonic time through DO RPC and the tiny
storage cleanup, not just the constructor. `before_handler_ms` estimates the
cross-isolate interval to the first method statement; clock skew can affect it.
`cf-ray` gives ingress colo, **not DO location**. Compare DO placement using
Cloudflare OpenTelemetry `jsrpc` spans by trace/DO ID before attributing a
latency gap to code size. Fresh IDs ensure a new *object*, not necessarily a
new Worker isolate. Wrangler's deploy-time startup figure is not a production
first-request measurement.
