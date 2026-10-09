# Claude authentication

`nanocodex-claude` supports Console API keys, caller-supplied header providers,
and a Rust-owned subscription OAuth lifecycle. Authentication lives outside the
agent's durable transcript. Reopening an agent attaches the same subscription
manager and the same `DurableSession`; it does not place credentials in that
session or discover an installed Claude Code login.

## Native CLI

The shipped CLI owns the subscription host and private credential store:

```sh
ncl --claude auth login
ncl --claude auth status
ncl --claude --model sonnet
ncl run "inspect the repository" --harness claude --model opus
ncl --claude auth logout
```

You can also launch `nanocodex` and use `/model` or `/model sonnet` before the
first prompt to select Claude. The picker includes both families; model selection
is locked once the thread starts. A missing Claude login leaves the previous
selection intact and shows the login command. Queued input is not sent after a
failed selection; select a model again before resubmitting. Claude-only credentials
are sufficient when the first prompt selects a Claude model.

Login opens the authorization URL and accepts the browser's `code#state` through
private terminal input. `--no-open` prints the URL for opening manually; piped
stdin is also accepted. Never put the code in command arguments or agent prompts.
The same subscription is used by Claude children of either harness family. One
manager per CLI task tree serializes refreshes; its durable compare-and-swap
revisions fence exchanges across processes and logout. Roots and children retain
their own native session identity and transcript.

Credentials default to `CODEX_HOME/claude/private/auth`, separately from agent
journals, encrypted with a local key in `auth.key`. On Unix, the private directory
requires mode `0700`, and credential, key and lock files require mode `0600`.
Logout clears the saved grant while retaining the store's revision and installation
identity; use the logout command instead of deleting the store.
`--claude-auth-file` or `NANOCODEX_CLAUDE_AUTH_FILE` selects a separate store.
`--claude-oauth-config` accepts a public JSON `ClaudeSubscriptionConfig` for an
explicit integration; omitted fields use defaults and unknown fields fail.
The configuration is bound to saved credentials, so use the same configuration
for login and subsequent runs. Default authentication uses the subscription
Messages URL and the pinned OMP compatibility profile described below.

`--claude-api-key` or `ANTHROPIC_API_KEY` explicitly selects Console API-key
authentication. Subscription login does not modify ChatGPT credentials;
`ncl auth login` continues to select Codex unless `--claude` or
`--harness claude` is provided. Missing Claude login fails before provider dispatch
or child admission and names the login command.

## Subscription login

`nanocodex_claude::subscription::ClaudeSubscription` implements
`ClaudeAuthProvider`. The Rust manager owns PKCE, callback validation, token
exchange, expiry, rotation, identity continuity and logout. The embedding supplies
`ClaudeSubscriptionHost`: a private secret store with durable compare-and-swap,
and bounded HTTP. This is the same separation between provider lifecycle and host
capabilities used by the hosted ChatGPT integration.

The defaults were derived from the installed public Claude Code **2.1.283**
executable and checked against an isolated invocation of `claude auth login
--claudeai`. That invocation used an empty configuration, a browser stub and
blocked outbound networking. Extracted public functions were also exercised with
synthetic values. No existing CLI credentials were imported. The measured
protocol is versioned evidence, rather than a claim that these endpoints and
scopes will never change.

| Operation | Observed default |
| --- | --- |
| Subscription authorization | `https://claude.com/cai/oauth/authorize` |
| Public client registration | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` |
| Code exchange and refresh | `https://platform.claude.com/v1/oauth/token`, JSON POST |
| Manual redirect | `https://platform.claude.com/oauth/code/callback` |
| Profile | `https://api.anthropic.com/api/oauth/profile` |
| Messages | `https://api.anthropic.com/v1/messages?beta=true` |
| OAuth request beta | `oauth-2025-04-20` |

Authorization uses independent random 32-byte state and verifier, S256 PKCE,
`response_type=code`, and `code=true`. The current default scopes are
`org:create_api_key user:profile user:inference user:sessions:claude_code
user:mcp_servers user:file_upload user:plugins`. The configuration accepts an
integration's own client registration, endpoints and scopes. These are the current
CLI defaults, not the older `claude.ai` / `console.anthropic.com` endpoints.

Call `begin_login(ClaudeLoginMode::Manual)` and present the returned
`authorization_url` to the account owner. Complete it with the browser's
`code#state` through the application's private input path. For an application-owned
callback, use `ClaudeLoginMode::Callback { redirect_uri }` and pass the callback
URL to `complete_login`. The host supplies the callback listener or route.
Both paths validate state; callback URLs must also match the stored origin and
path. Pending login survives reopening the secret store. Login URLs and codes
must not enter model prompts, tool results or agent history.

After login, construction uses the ordinary agent and durability APIs:

```rust,ignore
use std::sync::Arc;
use nanocodex::{Claude, DurableAgentExt, Nanocodex};
use nanocodex::claude::{ClaudeClient, subscription::*};
use nanocodex::durability::DurableSession;

// `host` implements ClaudeSubscriptionHost using private secret storage and HTTP.
let subscription = Arc::new(ClaudeSubscription::new(
    host, "account-provider-connection", ClaudeSubscriptionConfig::default(),
)?);
let client = ClaudeClient::subscription(reqwest::Client::new(), subscription);
let state = DurableSession::open(agent_store, "agent-session").await?;
let (agent, events) = Nanocodex::builder(Claude::latest(client))
    .cache_one_hour()
    .keep_thinking()
    .durability(state).await?
    .build()?;
```

Enable `claude` on the `nanocodex` facade; its existing default durability feature
includes the Claude adapter. Direct crate users enable `claude` on
`nanocodex-durability`. `ClaudeClient::subscription` selects the observed Messages
route and consumes the manager's headers. It enables the explicit subscription
compatibility profile measured below. Authentication beta headers combine with
request feature betas, so context management remains enabled alongside OAuth.
Tools, nested model requests and compaction share the same authenticated client.

The current explicit subscription wire profile is ported from pinned OMP v18.4.4: Claude Code version/runtime headers, SHA256 billing fingerprint, final-byte XXHash64 attestation, public device/session metadata and wire-only custom-tool prefixes. See [the managed wire guide](CLAUDE_MANAGED.md#subscription-wire-compatibility) for its scope and durable migration behavior. It imports no CLI login, private prompt or credentials. Installation affinity comes from the embedding host; the authenticated subscription manager remains separate.

The raw `client.request_body(&request, streaming)` API exposes the exact final bytes. Agent builders prepare the public system blocks and freeze the attested body as the durable effect identity before sending. A bounded 401 refresh preserves those bytes; the cursor freezes public profile/identity/version so reopening under changed client defaults preserves that request. Irreconcilable legacy identity mismatches fail closed. Custom endpoints opt in with `client.subscription_compatibility()`.

Earlier live admission measurements below used the former Nanocodex User-Agent profile. They are historical evidence for the auth lifecycle and native loop, not live acceptance of the new OMP profile.

## Credential persistence and failures

The host stores the opaque payload separately from `DurableSession`, encrypted at
rest and scoped to the account/provider connection. Store revisions are monotonic,
including across logout. HTTP must disable redirects and automatic retries,
respect the request deadline, and enforce the response bound while streaming.
Request/response Debug output and errors redact credentials; hosts must keep raw
bodies, authorization headers, codes and storage payloads out of logs.

Token exchange is claimed in the secret store before POST. A successful rotation
is staged durably before the replayable profile GET; a restart can finish profile
validation without repeating the exchange. CAS prevents a late login or refresh
from replacing a newer login or undoing logout. Identity changes are rejected.
If an exchange may have consumed a code or refresh token but its outcome was not
stored, the manager reports an uncertain exchange and requires a fresh login.
It does not guess that the remote POST failed and replay it.

Messages retries once after an explicit HTTP 401. A late rejection of an older
token cannot invalidate its replacement. The client does not automatically replay
403, 429, transport failures or an accepted stream; Claude agent sessions apply
their own bounded transient-failure retries. Refresh scope negotiation
allows one separate POST after a definitive `invalid_scope` rejection, using the
exact previously granted scopes. Expired refresh grants, invalid grants and an
account-on-hold response have explicit lifecycle outcomes. Logout commits its
local fence before best-effort provider revocation. Token exchange and agent
execution have separate recovery rules: uncommitted model/tool effects retain the
shared durability crate's at-least-once semantics.

`status()` returns public lifecycle state and authenticated account/organization
identity, never token values. The credential lifecycle is Rust/WASM portable;
the host remains responsible for storage placement, callback UI and networking.
The standalone library does not install a sign-in screen. The Worker SDK
exposes the same Rust lifecycle through `ClaudeSubscription.open`; its private
store and HTTP remain caller-owned. The
[managed platform integration](CLAUDE_MANAGED.md) supplies encrypted account
storage, a private connection form and authoritative model selection.

## API keys and other token sources

`ClaudeClient::official(http, api_key)` uses an explicitly supplied Console API
key. `RefreshingClaudeAuth` and `ClaudeTokenSource` remain available for a host
credential broker such as Workload Identity Federation. That adapter caches
usable tokens, serializes refreshes in one instance and rejects expired or malformed
replacements. The source owns its actual exchange and persistence. See Anthropic's
[API authentication documentation](https://platform.claude.com/docs/en/manage-claude/authentication).

## Verification and support boundary

Run `cargo test -p nanocodex-claude --test subscription --test auth_lifecycle` and
`cargo test -p nanocodex-durability --features claude,sqlite --test claude_subscription`.
The composed journey performs login, a signed tool round, compaction, real SQLite
reopen, 401 recovery and refresh rotation, terminal replay without another model
request, and logout. Provider traffic in these tests is synthetic loopback HTTP.

Live independent-native subscription admission was verified with the measured
compatibility profile. An actual interactive tmux reference exercised the CLI's
tool, compaction and recall behavior. The independent Rust client then performed
provider-backed tool execution, summary compaction and recall after a real SQLite
reopen, with provider-reported cache hits. Authentication for that measurement
came from an in-memory broker of the user's authenticated CLI; native request
bodies and successful response bytes were unchanged.

Two alternative first system blocks returned generic HTTP 429 before the fixed
public compatibility block succeeded with the same native comparison settings.
This establishes the observed difference in that environment, not a universal
provider rule or the meaning of the earlier rate-limit error. Applications should
validate their selected model and approved subscription integration against the
live provider; a synthetic transport test cannot establish admission or billing.

A separate fresh native PKCE login and deliberately triggered real refresh
were verified with provider-backed inference after each exchange and terminal
replay after process reopen. Natural expiry was not exercised. This is distinct
from managed product login/catalog admission and subscription billing; those
require their own authorized live acceptance. Anthropic documents third-party
subscription use in its [Agent SDK guidance](https://code.claude.com/docs/en/agent-sdk/overview)
and user login paths in [Claude Code authentication](https://code.claude.com/docs/en/authentication).
