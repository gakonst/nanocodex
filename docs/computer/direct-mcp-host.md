# Direct macOS CUA MCP host — no Codex executable

The managed macOS runtime keeps the official CUA MCP provider and signed native
helper. It does **not** install or run the official `codex` executable, a Codex
app server, or the Electron desktop shell.

```text
Hand -> Nanocodex direct MCP host -> upstream cua-repl / node_repl / Node
                                  -> signed Sky native helper
```

`direct-cua-host.mjs` is a small transport/lifecycle adapter using the existing
bundled Node. MCP tools, descriptions, schemas, arguments, caller metadata,
results, images, and structured content remain upstream-owned. The adapter's
internal MCP client advertises form elicitation; it never invokes a model or
creates a Codex thread. The direct host supplies `CODEX_CLI_PATH` only to its own narrow policy responder.
The generated kernel launcher executes signed `node_repl --disable-sandbox`,
preventing a Codex sandbox process launch. Provider analytics are disabled.
Managed receipts never export `CODEX_CLI_PATH`.

## Host-owned application consent

`NANOCODEX_CUA_APP_CONSENT=allow` is a trusted host setting for blanket native
**application-access** consent. The managed launcher defaults to that policy; a trusted host can set `deny`
to disable it. The module itself denies application access without `allow`. Tool arguments cannot change it. This setting does not authorize arbitrary
external actions, financial transactions, messages, account changes, microphone
recording, or forms that collect data. The authenticated caller's authorization
and agent-level action boundaries continue to apply. The upstream kernel runs
standalone; this host does not import Codex sandbox profiles or claim to provide
a Codex-managed execution sandbox.

The adapter accepts empty application-access forms during an active JavaScript
invocation: known native actions from `computer-use`, and browser origin/file
access from `browser-use` for HTTP(S) origins. Audio requests,
nonempty forms, URL-mode forms, requests outside an active invocation, and unknown
provider requests are declined or rejected. Consent is not persisted into Codex
configuration. The unmodified signed native helper retains its protected-target
checks and OS permission requirements. Locked-computer access and persistent
approval are not enabled by the compatibility policy.

## Native policy compatibility

The signed macOS helper asks a host executable for three newline-delimited
JSON-RPC methods: `initialize`, `configRequirements/read`, and `config/read`.
A generated `cua-policy-host` launcher directs these to this module's `--policy`
mode. The helper uses the historical argument spelling `app-server --listen
stdio://`; this is only a compatibility invocation of **our policy reader**, not
an official Codex binary or general app-server implementation. The helper's
`CODEX_CLI_PATH` points to this tiny launcher, never to Codex. The reader identifies
itself as `nanocodex-cua-policy-host` and describes this host's own access policy.

The browser config client also receives an honest signed-out `getAuthStatus`
response with no token. Other account/authentication methods, models, threads,
execution and configuration writes return method-not-found. Codex authentication material
and ordinary user configuration file contents are not read, copied, or fabricated.
Enforced-preference existence probes discard returned values without decoding
or logging them. The helper has its own temporary
Codex-named home within the private session directory; this is isolated state,
not the user's signed-in Codex home.

Known local/MDM policy sources are detected conservatively. If `/etc/codex`
requirements/managed configuration, user-home enforced requirement files, or
macOS managed preferences are present,
the direct host fails closed rather than declaring the machine unrestricted.
The outer adapters preserve a trusted caller's `CODEX_HOME` only for this
metadata-only policy check; upstream JS gets none of it, and the helper receives
only its own private session directory. Full managed-policy import and
cloud/enterprise-policy integration are not implemented. Ordinary owner-controlled Codex preferences are not read; the
owner's explicit Nanocodex app-access policy supersedes their prior local consent
choices, not administrator restrictions. Externally owned config fails closed.
This host does not authenticate or impersonate an enterprise Codex
account. OS-enforced restrictions remain in force. Do not claim universal
enterprise compatibility from an unmanaged-machine smoke test.

## Browser request marking

The relay requires agent request headers on every browser session command by
setting `agent_request_header_enabled:true`. When the extension reports boolean
support for this setting, the relay reports the effective
`agentRequestHeaderEnabled:true` state while preserving the rest of `getInfo`.
An extension that omits that capability is not reported as supporting it.
This marks browser automation explicitly and avoids an account-dependent feature
lookup used upstream to choose the setting. It does not supply authentication,
fabricate tokens, or modify signed upstream code.

The managed launcher sets `BROWSER_USE_DISABLE_AMBIENT_NETWORK=1` to disable
upstream browser telemetry and identity fetches, alongside the direct host's
`NODE_REPL_DISABLE_ANALYTICS=1`. Browser request marking stays enabled independently
of Codex sign-in. Relay protocol checks and provisioning fixtures alone do not
establish successful browser session actions; those require an actual upstream
integration check with a compatible extension.

## Lifecycle and isolation

Catalog discovery launches only the MCP provider. The first JavaScript call
starts a native helper for that conversation and waits for its private socket.
Each conversation has independent JavaScript scope and an owner-only state
folder. Small Node watchdogs observe the owner's stdin leases, reaping
provider and native-helper process groups even if cancellation SIGKILLs the main host. Normal EOF, errors,
and signals close owned processes; no request is replayed.

Cancellation/reset never establishes that previous input had no effect. Treat
uncertain outcomes as uncertain and observe fresh state before further input.
Startup/native readiness is bounded by trusted host deadlines. Provider tool
execution deadlines remain upstream-owned; the adapter does not interpret
model-supplied `timeout_ms` or add a second execution timer.

Trusted terminal-turn events forward the upstream hidden `turn_ended` hook with
the original session/turn identity and `Stop`, `Interrupt` or `SubagentStop`.
Only a provider already used by that turn is eligible; cleanup never starts or
recovers a provider and uncertain dispatch is not retried. The unchanged browser
extension owns background tab groups and its cleanup/handoff behavior.

Deploy the updated hosted-tools broker before upgrading Hand clients. New clients
advertise `turn_lifecycle:true`; older strict brokers reject that catalog field.
The updated broker still accepts older clients and sends cleanup only to clients
that advertise support. Standalone library embeddings can call `endTurn` (JS) or
`end_turn` (Rust) explicitly; automatic terminal forwarding is wired through the
Node/browser agent hosts and their attached Hands.

## Browser and computer surfaces

The managed launcher enables `CUA_REPL_ENABLED_SURFACES=browser,computer` and
`BROWSER_USE_TINYSKY_ENABLED=1`. The signed CUA modules and Sky helper retain
their upstream APIs. Nanocodex's `direct-browser-host.mjs` relays Chrome native
messages; its immutable launcher uses the same bundled Node. The sparse bundle
continues to exclude both the official Chrome app-server proxy and
`Resources/codex`.

Full setup registers `com.openai.codexextension` in the per-user
`NativeMessagingHosts` directories of installed supported Chromium browsers.
The manifest permits only extension IDs `hehggadaopoacecdllhhajmbjkdcmajg` and
`odlomjlbamekndcpllcnffbgeohgkmjh`. Registration targets Chrome (including Beta,
Dev and Canary), Chromium, Brave, Edge and Arc when their app is present in
`/Applications` or `~/Applications`. Setup neither installs an extension nor
opens, restarts, or changes browser profiles. The compatible extension must
already be installed and enabled to use extension-backed browser APIs.

An existing official Codex or other non-Nanocodex manifest causes an actionable
conflict. Setup preserves it and does not publish a new provider receipt. Resolve
that registration's ownership explicitly before rerunning setup. Nanocodex-owned
manifests can move to a new immutable host generation. No browser installation
is needed to prepare the host assets; rerun setup after installing a browser to
register its manifest.

Automatic Windows upstream setup remains disabled pending verification of its
native helper contract. The opt-in Linux Sky host is computer-only; see
[the upstream runtime guide](upstream-provider.md#linux-native-host).

The installer creates a new sparse, attested generation without `Resources/codex`.
It does not delete or mutate an older generation that a running Hand may still be
using. Restart the Hand after installing the new generation to activate it; a TUI
reload alone is insufficient. Removing old, unused generations is separate from
switching a running process safely.

## Validation

```sh
node --test scripts/tests/direct-cua-host.test.mjs
cargo test -p nanocodex-computer
```

Live tests must use an isolated bundle with no official Codex executable and
verify both native observation/input and process cleanup. Passing arithmetic
alone is not evidence of working native CUA: the old direct-provider experiment
passed JavaScript but failed the helper's native policy lookup.

Provisioning fixtures use an isolated home and synthetic installed-browser
directories to verify registration, repeat setup, manifest conflicts, receipt
preservation and recovery. They execute the generated launchers against probe
executables to inspect the actual argv and environment. These checks do not
install anything into a live account or establish live browser/native acceptance.
