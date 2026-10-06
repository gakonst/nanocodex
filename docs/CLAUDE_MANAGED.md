# Managed Claude subscriptions

The managed platform has an account-scoped Claude subscription connection and a
native Messages execution path. It does not translate Claude through OpenAI
Responses or borrow an installed Claude Code login. The OpenAI and Claude tool
runtimes remain separate.

## Connect privately

In the web account's **Connections** section, choose **Claude → Connect**. Native
clients expose a Claude subscription section in their connection/account settings.
Open the provider sign-in page, approve access, and paste its `code#state` into the
dedicated private authorization-code field. Never paste the code into a chat.

The default uses the provider-registered manual callback. It does not assume a
Nanocodex HTTPS callback is registered for the public provider client. The actual
Rust `ClaudeSubscription` lifecycle owns PKCE, exchange, profile validation,
refresh, continuity, uncertain-exchange fencing and logout; the broker stores its
opaque state encrypted with durable compare-and-swap. Credentials and pending
OAuth material are separate from conversations, tools, checkpoints and logs.

The authenticated account API is:

| Request | Result |
| --- | --- |
| `POST /v1/credentials/claude/login` | Safe pending status, authorization URL and expiry |
| `GET /v1/credentials/claude/login` | Safe lifecycle status |
| `POST /v1/credentials/claude/login/complete` with `{ "code": "code#state" }` | Profile-validated status, or a detail-free failure |
| `DELETE /v1/credentials/claude` | Local connection removed; best-effort provider revocation |
| `GET /v1/credentials` | Safe connection metadata; no credential material |
| `GET /v1/models` | Account-available managed model catalog |

Expiry values are epoch **milliseconds**. Mutations require a persistent account
with full account authority and the normal same-origin checks. Connect grants
cannot connect a provider, obtain its credentials, or start Claude inference.
Native clients submit private form input directly to the account API, not via an
agent prompt.

If exchange or refresh may have consumed its one-use input but the result was
not retained, the manager reports an uncertain exchange. Refresh status and
perform a new explicit sign-in; do not automatically repeat the uncertain POST.
Disconnect fences the local grant before attempting revocation. It does not
claim to undo a previously accepted model request or external action.

## Model availability and session routing

A connected grant is not an entitlement to every model. The server obtains the
provider's authenticated model catalog and intersects it with models supported
by this runtime. Catalog lookup follows provider cursors under one 15-second provider-HTTP deadline, at most 10 pages of 100 rows (1 MiB per page), and one explicit-401 credential recovery for the whole lookup. Missing/repeated cursors, exceeded bounds or later-page failures produce an availability error rather than advertising a first-page catalog as complete. A failed catalog lookup is an explicit availability error, not
a fabricated list or an OpenAI fallback. The picker and native clients consume
the authoritative catalog. A Claude-only account can select its catalog default
and start a conversation without first connecting an OpenAI credential.

Claude model and effort are pinned when a session begins execution. Stale,
disconnected or unsupported selections are rejected. Each request resolves its
current private grant through `SessionModelEgress`, which pins the provider
origin and strips host-routing fields before dispatch. Only an explicit provider
401 can trigger bounded credential recovery; ambiguous failures and accepted
streams are not silently replayed.

## Tools, durability and limits

Managed Claude sessions expose native `Bash`, `Read`, `Write`, `Edit` and supported
account/Hand capabilities. Discovery uses `ToolSearch`/`ToolExecute` and
`MCPToolSearch`/`MCPExecute`, not Responses tool-search declarations.

Default managed Claude sessions expose the canonical `spawn_agent`, `list_agents`,
`send_agent_message`, `wait_agent`, `interrupt_agent` and `close_agent` tools.
Account-owned managed sessions can select `harness: "claude"` from a Codex
parent or `harness: "codex"` from a Claude parent. Each child uses its native
Messages or Responses transport and the spawning turn's retained authority;
selecting a child never changes the parent's model. Claude-root Codex children
currently support the available GPT models; gateway models are rejected at
admission. Child selection is checked against the account's available model
catalog before inference. Explicit `multi_agent: { enabled: false }` disables
delegation, and an explicit tool allowlist does not acquire additional tools. Existing configurations
that explicitly enable `Task` retain its blocking execution, durable receipts and
uncertainty after interruption. The session's native prompt describes its actual
tools rather than instructing Claude to call Codex Code Mode.

Native Messages history, opaque content and completed receipts survive normal
Durable Object reopen in the shared durability store. Events retain streaming
assistant text and tool cards. This does **not** make OpenAI snapshots portable
to Claude. Managed Claude accepts ordered text, HTTPS/base64 images and inline
PDF or text documents. Uploaded native-client image descriptors are resolved to
bounded JPEG previews before dispatch. OpenAI image file IDs and raw audio input
remain unsupported; documents remain unsupported on GPT sessions.

GPT Realtime can provide the voice frontend for a Claude thread. Completed voice
transcripts and start/stop markers become bounded, once-consumed session context,
not Messages audio or Responses history. Delegations start an idle Claude turn or
steer an accepted active turn. Identified steering (`message_id`) and withdrawal
remain unsupported by the native Claude backend and return a typed HTTP 400;
they are never silently downgraded to non-idempotent steering. Snapshot forks and portable import/export remain
explicitly unsupported, rather than silently converting or discarding state.
See the [Claude runtime](CLAUDE_RUNTIME.md),
[JavaScript SDK](CLAUDE_JAVASCRIPT.md) and
[tool matrix](CLAUDE_TOOL_MATRIX.md) for the distinct library boundaries.

## Subscription wire compatibility

The explicit subscription profile uses the pinned [OMP v18.4.4 wire helpers](https://github.com/can1357/oh-my-pi/blob/v18.4.4/packages/ai/src/providers/anthropic.ts#L634-L709), ported under MIT into `nanocodex-claude`. This replaces the former Nanocodex User-Agent profile, at the user's request. Native, WASM and managed execution share one implementation:

- Claude Code `2.1.280` / SDK `0.112.1` fingerprint headers and ordered OAuth utility/agent betas; `PI_AI_CLAUDE_CODE_VERSION` or explicit public version overrides the fallback.
- First-system-block billing fingerprint: SHA256 with OMP's salt and JavaScript UTF-16 indices 4/7/20 from the first user text. The following public identity block carries the selected cache policy.
- Exact OMP `cch`: XXHash64 with seed `0x4d659218e32a3268`, low 20 bits, anchored to system[0] and patched over the final serialized UTF-8 bytes. Literal markers in caller content are not changed.
- OMP's account-scoped device hash and JSON-string `metadata.user_id`, with stable session affinity and preservation of supported caller metadata IDs. Custom tool names acquire one wire-only `_`; the four pinned native names are exempt and local tool names stay unchanged.

Managed execution binds installation identity to the owning account, with stable session IDs across DO reopens. It does not invent an Anthropic account UUID. SDK embedders may supply public `subscriptionIdentity` (installation/account/session affinity is not a credential). Without an explicit installation ID, native callers use the stable backend session ID.

The public wire profile (version, installation, platform and session affinity) is frozen in the admitted cursor. Reopening under changed client defaults keeps that profile; only new operations use the new defaults. The final attested bytes, including metadata and tool mapping, become the durable model effect identity **before** HTTP. A bounded 401 credential refresh resends identical bytes. Private egress only adds authorization and forwards the public fingerprint; it never computes a checksum or rewrites the body. API-key/non-profile behavior is unchanged. Completed receipts still replay without HTTP. Old cursors without a wire profile retain legacy request/effect encoding rather than silently adopting OMP transformations. Any irreconcilable recorded identity still fails closed; no external effect is replaced under a new identity.

This copies the subscription wire helpers, not OMP's entire model registry, prompt-cache placement policy, TLS stack or automatic version-adoption/retry engine. In particular, a server-requested version change cannot mutate an already frozen durable operation. Nanocodex retains its own bounded refresh/recovery rules. The pinned independent oracle requires Bun >=1.4; Bun 1.2.4 gives a different seeded checksum and is not a valid OMP runtime for this comparison.

Synthetic public-HTTP and actual WASM/managed restart tests establish protocol behavior, **not** new live-provider admission, subscription billing or production deployment. The earlier live admission evidence used the previous profile and does not prove this new profile has been accepted live.

## Deployment and acceptance

Build the shared WASM/package first, then deploy the private broker/egress and
managed service dependencies before the account application. A source change or
passing synthetic fixture is not proof of a production rollout. Real workerd
journeys exercise the shipped broker, Rust WASM lifecycle, account API and
SQLite session transport with synthetic upstream provider traffic. A separate,
user-authorized live managed login, catalog and inference smoke is still required
before claiming live provider admission or subscription billing behavior.
