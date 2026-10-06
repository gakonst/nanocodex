# nanocodex-managed

Native account-managed lifecycle backend for Nanocodex. The crate owns the
authenticated managed HTTP service, resumable durable event stream, and
optional reverse attachment of a caller-owned `Tools` recipe. The cloud owns
model execution and retained history; this crate never reads provider
tokens or application environment variables.

For optional local discovery, `ManagedBuilder::tools_async` prepares a complete
`Tools` recipe concurrently with creation or opening. It does not delay first
prompt admission or event delivery. The recipe attaches when ready; failed or
cancelled builds and disconnect cancel unfinished preparation. Validate required
configuration before building, and handle optional discovery errors in the
future. Use `tools` for an already prepared recipe or a required provider whose
errors must be resolved before admission.

`Managed::create` and `create_live` resolve the authenticated account catalog
default when no explicit `with_settings` policy is supplied.
`ManagedClient::create_with_settings` sets the initial model policy atomically.
The existing `set_model`, `set_thinking`, `set_reasoning_mode`, and
`set_fast_mode` methods patch individual fields for subsequent turns.

Managed model policy uses `ManagedModel`: native Responses identities convert
through `From<Model>`, while Claude identities remain managed-only. Use
`AgentSettings::new(model)` for a standard policy with model-specific default
effort. Existing native `set_model(Model::...)` calls remain supported; explicit
settings struct literals now use `model: Model::Sol.into()`. The common native
agent handle still has its Responses-only model setter; use the managed client
or initial managed settings to select Claude.

`ManagedClient::models()` reads authenticated `GET /v1/models`. Its catalog is
authoritative for account availability; known local identities are not proof
that a subscription is connected. Claude models support only the returned
low/medium/high efforts, standard reasoning, and no fast mode. The catalog also
projects `partial` and public provider `availability`: an unavailable Claude
catalog does not hide healthy OAI entries, but an explicitly selected Claude
model is never coerced into a healthy OAI fallback.

Direct-account subscription sign-in uses `claude_login_start`,
`claude_login_status`, `claude_login_complete`, and `claude_disconnect`.
The caller must obtain explicit user authorization and collect completion input
privately using `ClaudeLoginCode`, never from a prompt or agent tool. Pending
`ClaudeLogin` URLs include private state: they are for a private browser, not
logs or retained conversation data. Both types redact Debug; completion input
zeroizes on drop. Auth responses and errors expose only bounded public states,
not provider payloads or tokens. Auth writes are single-shot, including protocol
failures; inspect status after an unknown outcome rather than replaying a code.
Authorization destinations must match the registered native manual flow: exact
`https://claude.com/cai/oauth/authorize`, public `code=true`, registered client
and manual redirect, response type, scopes, state, and S256 PKCE parameters.
Callback codes are not permitted in the destination URL.
Expiration timestamps are Unix milliseconds. Connect grants cannot manage
subscription credentials.

`ManagedClient::compact(agent_id)` and the common `agent.compact()` handle post
an empty authenticated body to `/v1/agents/{id}/compact` and validate synchronous
`{compacted:true}` acknowledgement. Compaction is never automatically replayed;
inspect retained history after an uncertain outcome. The server requires full
account authority and an idle session.

`ManagedClient::fork(parent_agent_id, idempotency_key)` posts an empty body to
`/v1/agents/{parent_agent_id}/forks`. The service returns a child `AgentReceipt`
from the parent's latest committed model boundary. Reuse the same key to
reconcile uncertain admission; no transcript or side prompt is submitted to the
parent.

Durable schedules are exposed through `triggers`, `trigger`, `put_trigger`,
and `delete_trigger`. `CronTriggerConfig` contains the complete cron expression,
timezone, prompt, enabled state, and `CronSessionMode` (`New` or `Continue`).
The client validates identifiers and input bounds; the managed service validates
schedule syntax and timezone semantics. `put_trigger` replaces a named schedule
using PUT. Schedule receipts include delivery timestamps and the last agent/turn.

`ManagedClient::vault_request(&VaultRequest)` sends one authenticated
`POST /v1/vault/request`. Supply only an opaque saved `vault_id` and public HTTPS
request templates; the broker resolves credentials and computes optional HMAC,
PKCS#8 signatures or JWTs at the outbound boundary. `VaultSigning` contains the
algorithm and public message or JWT claims, never the key. The response is a
closed `VaultRequestReceipt { status, ok }` with no destination body, headers,
cookies, signature or token. The client bounds requests to 96 KiB and receipts
to 4 KiB, rejects malformed receipts, projects fixed error codes, and never
retries. An unknown outcome may have executed; reconcile before another call.
The server requires direct account authority and rejects Connect grants.

The CLI uses the existing account login and reads the same public request JSON
from a file or stdin:

```sh
nanocodex2 vault request --file request.json
nanocodex2 vault request --stdin < request.json
```

For example, `request.json` can contain a synthetic saved-item reference and
an authorized destination:

```json
{
  "vault_id": "abcdefghijklmnopqrstuv",
  "url": "https://example.com/authorized",
  "method": "POST",
  "headers": { "authorization": "Bearer {{NANOCODEX_VAULT_API_KEY}}" },
  "body": "public-payload"
}
```

Successful dispatch prints only `{"status":201,"ok":true}` (with the actual
destination status). A valid destination failure such as HTTP 403 still returns
a receipt with `ok:false`; CLI failure means input, authorization, transport or
receipt validation failed. This operation does not export secrets to native
processes or implement multi-step native login protocols such as xtool SRP.

Fresh prompt callers can opt into server-side catalog selection with
`ManagedBuilder::settings_selection(InitialSettingsSelection { ... })` before
`build_with_prompt`. `InitialSettingsPolicy::Cli` preserves the CLI preference
for xhigh (when offered) and catalog-supported fast mode; `Sdk` uses model-default
effort and fast mode off. Optional thinking/reasoning/fast overrides are validated
against the live catalog. A ChatGPT pin selects only an available OpenAI model,
preferentially Sol. Explicit `with_settings` takes precedence. Existing builders
without this opt-in retain their prior behavior and server compatibility.

The opt-in sends `settings_selection` in the combined request and requires the
server's `X-Nanocodex-Settings` response header before starting the driver. An old
server or missing/invalid header fails closed; no local model default substitutes
for the selected model. `build()` rejects this selection option: it applies to
combined creation with a known first prompt.

For an opt-in unknown model, the first prompt keeps structural validation (and
still rejects transcript/local media) but defers document-family validation to
server admission. The selected-model response seeds the driver before it adopts
the first prompt. The server retains the original request fingerprint and
resolved settings, so replay after catalog failure or default changes reuses the
same admission; changing policy, pin, overrides or input with that key conflicts.
