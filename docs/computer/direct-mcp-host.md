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
creates a Codex thread. Provider `CODEX_CLI_PATH` is unset, and provider analytics
are disabled so they cannot attempt to fetch Codex authentication.

## Host-owned application consent

`NANOCODEX_CUA_APP_CONSENT=allow` is a trusted host setting for blanket native
**application-access** consent. The managed launcher defaults to that policy; a trusted host can set `deny`
to disable it. The module itself denies application access without `allow`. Tool arguments cannot change it. This setting does not authorize arbitrary
external actions, financial transactions, messages, account changes, microphone
recording, or forms that collect data. The authenticated caller's authorization
and agent-level action boundaries continue to apply. The upstream kernel runs
standalone; this host does not import Codex sandbox profiles or claim to provide
a Codex-managed execution sandbox.

The adapter accepts only an empty native application-access form from the
`computer-use` connector during an active JavaScript invocation. Audio requests,
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

All other methods, including account/authentication, models, threads, execution,
and configuration writes, return method-not-found. Codex authentication material
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

## Native-only surface

The managed launcher sets `CUA_REPL_ENABLED_SURFACES=computer`. The sparse
bundle excludes the official Chrome plugin as well as `Resources/codex`; no
native-messaging manifest is installed. Dedicated Tab/DOM/browser APIs and
background tab groups are unsupported. Native UI control of a browser window
requires authorization to interact with that window; it must not be used as a
foreground workaround for unavailable background browsing. The
shipped Chrome native-messaging host contains an app-server proxy, so preserving
that surface requires a separate port, not pointing it at the policy-only shim.
Automatic Windows upstream setup is disabled pending verification of its native
helper contract. The opt-in Linux Sky host runs without the Codex CLI; see
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

### Acceptance status (2026-09-30)

The direct-host regressions, Rust transport/provisioning tests, JavaScript
attachment tests, and installed-provider catalog smoke pass. The staged bundle
passes real signature verification and contains no official CLI/Chrome host;
observed owned process trees contain only Node/node_repl/Sky. EOF and SIGKILL
cleanup have been exercised without surviving owned children.

Earlier isolated Finder accessibility observation succeeded. The latest
native-only staged Rust smoke passes persistent JS but fails initial Finder
observation with `cgWindowNotFound`; a separate own-app fixture also fails with
that native error. Its cause is not established. Native input is **not** verified.
This is a draft implementation, not full live native acceptance or an activated
Hand upgrade. Dedicated browser surfaces remain unsupported. For current Windows and Linux
support and their separate validation limits, see the
[upstream runtime guide](upstream-provider.md).
