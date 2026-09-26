# egress2

Private Cloudflare Worker, inspired by [iron-proxy](https://github.com/paradigmxyz/iron-proxy)'s placeholder substitution. It has no public routes (`workers_dev: false`; do not add routes/custom domains). Bind its **default** `Egress2` entrypoint to a trusted host as a service binding. Only that host may supply `x-managed2-owner` after authenticating the user; never expose this binding to untrusted callers.

- `await EGRESS2.putCredential(ownerId, "openai", key)` stores/replaces the owner's OpenAI API key and selects it as active. With JS, `putCredential(ownerId, undefined, key)` uses the default provider.
- `await EGRESS2.putChatGptCredential(ownerId, { access_token, refresh_token, account_id, expires_at, fedramp })` validates the same exact import fields as `js/egress` (including JWT expiry; account and FedRAMP claims are checked when present), replaces this owner's one subscription, and selects it as active. The stored refresh token and Rust-owned `ChatGptSubscription` lifecycle stay in the per-owner SQLite `UserCredentials` Durable Object, not in Agent. RPC errors never include token values.
- `await EGRESS2.fetch(new Request("https://api.openai.com/v1/responses", { method: "POST", headers: { "x-managed2-owner": ownerId, "authorization": "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "content-type": "application/json" }, body: JSON.stringify(payload) }))` substitutes the active owner's credential. An active subscription routes to **exactly** `https://chatgpt.com/backend-api/codex/responses`, with `chatgpt-account-id`/FedRAMP derived from the credential; an active key routes to `https://api.openai.com/v1/responses`. A trusted caller can instead use the exact Codex URL when a subscription is active. The upstream response streams without buffering.
- Accepted Responses requests emit `Server-Timing` phases `egress_credential` (in-isolate cache/DO lookup), `egress_dispatch` (handler entry until outbound dispatch), `egress_upstream_headers` (provider/relay until HTTP headers), and `egress_total`, with fixed `egress_route` (`openai_api` or `chatgpt_subscription`) and `egress_cache` (`hit` or `miss`). A structured `responses_egress` log contains only those fixed route/cache labels, upstream HTTP status, and durations; it never contains credentials, owner/account identifiers, prompts, headers, response bodies or URLs. A 101 WebSocket response must pass through unchanged, so it is logged but has no added `Server-Timing`. Timing stops before the streaming body completes and does not establish client-visible time-to-first-token.
- Exact HTTPS paths only; POST or GET WebSocket upgrade (including 101 passthrough). The owner header is removed upstream. Redirects are not followed or relayed. Missing owner/placeholder/credential fails closed. Up to 256 access credentials are cached per isolate for at most 60s; a local RPC write invalidates this isolate while other isolates can serve an old credential for at most 60s. ChatGPT cache entries expire before the refresh window; Rust-owned lifecycle refreshes before expiry via `auth.openai.com/oauth/token` and CAS-persists rotating refresh tokens in this owner's DO. A subscription POST or WebSocket handshake receiving 401 invokes Rust `recover` for its rejected revision and retries once with the new credential; a second 401 is not retried.

Set the **required** Worker secret `CREDENTIAL_ENCRYPTION_KEY` before deployment to a random 32-byte key encoded as standard padded base64 (44 characters, e.g. generate with `openssl rand -base64 32`). Never put the key in `wrangler.jsonc`, source, or logs. The API key in SQLite and the opaque Rust-owned subscription payload in DO storage are sealed independently using AES-256-GCM with fresh 96-bit IVs and owner/record-bound authenticated data (`v1` envelope). No plaintext fallback or migration exists: existing unencrypted test records must be discarded, and losing/changing the key renders stored credentials unreadable. Non-secret active selection and CAS revision metadata remain unencrypted. The Workerd test supplies only a synthetic key. Do not deploy or set the secret as part of local tests.

Subscription egress uses the `GATEWAY` VPC Network binding (`cf1:network`)
to reach ChatGPT through Cloudflare Gateway, passing WebSocket upgrades through
unchanged. This skips the account relay Durable Object and Linux container.
The regional `CHATGPT_EGRESS_*` and legacy `CHATGPT_EGRESS` bindings remain as a
rollback path: remove `GATEWAY` from the Egress2 deployment config and redeploy;
no credentials, Session IDs, or relay identities migrate. Direct public Worker
fetches received provider HTTP 403 in live tests. The Gateway route succeeded
in a separate canary Worker/Managed2 session before activation. Gateway policies
can change independently; monitor HTTP 101/401/403/502 and first-answer latency.
This is a hop reduction, not a proven first-token speedup. API-key outbound uses
neither subscription route. Test-only `wrangler.test.jsonc` keeps exercising
the Container DO fallback and never serves real credentials.

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

## Timing diagnostics

`responses_egress` is emitted for both HTTP replies and WebSocket 101 upgrades.
A validated, host-generated `trace_id` correlates it with Managed2; only the
private ChatGPT relay receives a separately generated `egress_request_id`.
Fixed fields include final response/upstream status, credential cache hit/miss,
credential lookup, post-lookup dispatch, upstream handshake, total elapsed, and
subscription 401 recovery/retry outcome and duration. HTTP `Server-Timing` has
matching spans, but 101 cannot be rewrapped to add headers. These measurements
end at headers/upgrade, not first model token or stream completion. Credential
lookup can include a DO wake, decrypt, and refresh, not separately timed here.
No secrets, prompts, owner IDs or upstream bodies are logged.

## Regional subscription relay

Managed2 records a new agent's trusted Cloudflare ingress colo as a coarse
relay region for Container fallback. For recognized colos, its model WebSocket and web-search requests
carry that fixed region over the private Egress2 service binding. Egress2 strips
all Managed2 headers before forwarding to ChatGPT, selects the matching existing
account-owned regional Container class, and creates `text-v2:<region>:<owner>`
with a best-effort Cloudflare location hint. `text-v2` is a fresh identity,
not a migration of the immobile legacy `user-v1` relay. Agents created before
this change (or at an unmapped ingress colo) use the legacy relay only if
Gateway is disabled; existing agent and credential records are not migrated.
A missing regional binding or invalid region fails closed when that fallback is
active. The production VPC `GATEWAY` is the direct route and bypasses the
Container DO; regional classes remain available for rollback. The Egress2 relay span
records only a fixed region label, never owner or prompt content.

The hint and container region constraint do not guarantee a specific city,
identify OpenAI's inference region, or prove an end-to-end win. Compare same
subscription and prompts in interleaved warm/cold live cohorts using the
Cloudflare trace's actual DO/Container locations, Egress2 upstream-header time,
model first provider event and first answer delta. HTTP search duration and
long-lived WebSocket span duration are not interchangeable with TTFT.
