# egress2

Private Cloudflare Worker, inspired by [iron-proxy](https://github.com/paradigmxyz/iron-proxy)'s placeholder substitution. It has no public routes (`workers_dev: false`; do not add routes/custom domains). Bind its **default** `Egress2` entrypoint to a trusted host as a service binding. Only that host may supply `x-managed2-owner` after authenticating the user; never expose this binding to untrusted callers.

- `await EGRESS2.putCredential(ownerId, "openai", key)` stores/replaces the owner's OpenAI API key and selects it as active. With JS, `putCredential(ownerId, undefined, key)` uses the default provider.
- `await EGRESS2.putChatGptCredential(ownerId, { access_token, refresh_token, account_id, expires_at, fedramp })` validates the same exact import fields as `js/egress` (including JWT expiry; account and FedRAMP claims are checked when present), replaces this owner's one subscription, and selects it as active. The stored refresh token and Rust-owned `ChatGptSubscription` lifecycle stay in the per-owner SQLite `UserCredentials` Durable Object, not in Agent. RPC errors never include token values.
- `await EGRESS2.fetch(new Request("https://api.openai.com/v1/responses", { method: "POST", headers: { "x-managed2-owner": ownerId, "authorization": "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json" }, body: JSON.stringify(payload) }))` substitutes the active owner's credential. An active subscription routes to **exactly** `https://chatgpt.com/backend-api/codex/responses`, with `chatgpt-account-id`/FedRAMP derived from the credential; an active key routes to `https://api.openai.com/v1/responses`. A trusted caller can instead use the exact Codex URL when a subscription is active. The upstream response streams without buffering.
- Accepted Responses requests emit `Server-Timing` phases `egress_credential` (in-isolate cache/DO lookup), `egress_dispatch` (handler entry until outbound dispatch), `egress_upstream_headers` (provider/relay until HTTP headers), and `egress_total`, with fixed `egress_route` (`openai_api` or `chatgpt_subscription`) and `egress_cache` (`hit` or `miss`). A structured `responses_egress` log contains only those fixed route/cache labels, upstream HTTP status, and durations; it never contains credentials, owner/account identifiers, prompts, headers, response bodies or URLs. A 101 WebSocket response must pass through unchanged, so it is logged but has no added `Server-Timing`. Timing stops before the streaming body completes and does not establish client-visible time-to-first-token.
- Exact HTTPS paths only; POST or GET WebSocket upgrade (including 101 passthrough). The owner header is removed upstream. Redirects are not followed or relayed. Missing owner/placeholder/credential fails closed. Up to 256 access credentials are cached per isolate for at most 60s; a local RPC write invalidates this isolate while other isolates can serve an old credential for at most 60s. ChatGPT cache entries expire before the refresh window; Rust-owned lifecycle refreshes before expiry via `auth.openai.com/oauth/token` and CAS-persists rotating refresh tokens in this owner's DO. A subscription POST or WebSocket handshake receiving 401 invokes Rust `recover` for its rejected revision and retries once with the new credential; a second 401 is not retried.

Set the **required** Worker secret `CREDENTIAL_ENCRYPTION_KEY` before deployment to a random 32-byte key encoded as standard padded base64 (44 characters, e.g. generate with `openssl rand -base64 32`). Never put the key in `wrangler.jsonc`, source, or logs. The API key in SQLite and the opaque Rust-owned subscription payload in DO storage are sealed independently using AES-256-GCM with fresh 96-bit IVs and owner/record-bound authenticated data (`v1` envelope). No plaintext fallback or migration exists: existing unencrypted test records must be discarded, and losing/changing the key renders stored credentials unreadable. Non-secret active selection and CAS revision metadata remain unencrypted. The Workerd test supplies only a synthetic key. Do not deploy or set the secret as part of local tests.

For subscription egress, a configured `GATEWAY` VPC Network binding takes the
exact authorized request directly to ChatGPT and passes WebSocket upgrades
through unchanged. The `CHATGPT_EGRESS` binding to the existing account-owned
relay DO is used only when `GATEWAY` is absent. Direct public Worker fetches
received provider HTTP 403 in live tests. The VPC route was verified in a
service-binding-only staging Worker using this same Egress2 implementation and
the existing credential DO via an external binding. Keep the network identifier
in a private deployment config; `wrangler.jsonc` retains the relay route until
the managed Worker and Egress2 are promoted together. API-key outbound uses
neither subscription route. Test-only `wrangler.test.jsonc` never serves real credentials.

This first slice has no credential provisioning HTTP endpoint, login UI, subscription account pool, or account failover. Deployment/service-binding wiring and trusted-host authentication are intentionally external to this package. `pnpm --filter nanocodex-egress2-service test` exercises the actual workerd Worker/DO and Rust/WASM refresh with a synthetic outbound provider. Run `typecheck` and `build` (dry-run only) after installing workspace dependencies.

Cloudflare Workers Logs are persisted at 100% sampling for this greenfield Worker.
Open its **Observability → Logs** tab in Cloudflare to search for
`managed2.model_route` or `responses_egress`; both are structured objects
with fixed labels and timing fields. Live `wrangler tail` is separate from
the persisted dashboard. Workers tracing is enabled at 100% sampling with
retention in Cloudflare Observability. The trace follows service bindings and
Durable Object calls across Managed2, Egress2, and the account-owned
`ChatGptEgress` Container DO when that Worker also has tracing enabled.
The external Linux container and ChatGPT provider do not propagate Cloudflare
trace context; inspect the Container DO and outbound-fetch spans plus Egress2
header timings there. A create request, turn request, and an alarm-resumed
execution can have distinct root traces. Search by the turn request CF-Ray;
`0ms` spans can be timer-resolution artifacts.
