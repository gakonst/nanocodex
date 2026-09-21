# Managed official macOS CUA host

On macOS, `computer setup` installs a signed, unmodified OpenAI application bundle
and a Nanocodex transport launcher. The launcher starts the official app server
and CUA provider **without launching the ChatGPT/Codex desktop GUI**. The upstream
native computer-use helper can run in the background; its OS permissions still
apply. The currently validated bundle is build **9922**.

## Permission handling

The official app server advertises MCP form support and applies the user's existing
Codex permission policy. Under full-access authority, codex-rs can accept standard
empty-form confirmations without a GUI. Nanocodex does not inject an approval,
change sandbox or approval settings, add a permission dialog, or cache consent.
The provider still checks app policy before issuing its confirmation request.

If upstream policy requires interactive input, the managed headless bridge declines
that unresolved request for its own CUA thread. It never accepts it on the user's
behalf, answers another client's requests, or opens an app to ask. Consequently,
headless operation does not override restricted profiles, organization policy,
macOS access controls, or upstream operations that require interactive input.

## Installation and lifecycle

The signed bundle is cached under `runtimes/openai-cua/versions`. Nanocodex's own
launcher and bridge modules live separately under `hosts/<content-hash>`.
`provider.json` selects an immutable launcher. Updating Nanocodex generates a new
host directory while reusing the verified bundle. Existing processes retain their
selected generation; restart a running Hand to pick up the new launcher. A TUI
reload alone does not restart the shared Hand. Modified assets and invalid bundle
signatures fail validation.

The dependency-free supervisor in `openai-cua-native-host.mjs` uses the bundled
Node, official CLI, and direct CUA provider. It never patches the signed bundle,
copies credentials, or changes normal Codex configuration. Official programs use
the existing Codex home and sign-in state.

A private Unix socket and `shlock` singleton identify the bundle, launcher, state
directory, and effective Codex home. The server listens on an OS-selected loopback
port; the supervisor waits for its `/readyz` response before exposing a lease.
Other configured MCP servers are disabled only in this dedicated server through
temporary CLI overrides. Only names and transport kinds are retained from CLI
metadata; their saved settings remain unchanged.

Catalog discovery creates no thread. The first tool call creates a separate
**ephemeral** official thread without injecting history or invoking a model.
There is no GUI attachment or GUI-readiness wait. Closing a bridge releases its
lease; after the final lease the supervisor stops its owned server after 60 seconds
idle, with 120 seconds startup grace. Server death fails pending work. Calls are
never replayed, and cancellation cannot undo input already delivered.

## Transport

`openai-cua-app-server.mjs` forwards MCP stdio calls to the official app server.
Provider definitions, schemas, arguments, metadata, results, images, and structured
content are preserved. Catalog entries are sorted only for stable discovery.
Authentic nested caller metadata is preserved; top-level thread routing identifies
the bridge's own thread. App-server errors retain their code, message, and data.

Calls within one bridge are ordered. Timeout, cancellation, EOF, and connection
loss close only that connection. A new MCP process gets a fresh thread. The outer
Nanocodex MCP adapter does not need to advertise elicitation: the official app
server owns that protocol with the provider.

The transport's existing standalone GUI integration remains optional for operators
who explicitly connect it to an existing official server and set
`NANOCODEX_CUA_APP_SERVER_OPEN_GUI=1`. The managed default does not use that path.
Windows uses its separate official-provider transport.

## Verification

```sh
node --test scripts/tests/openai-cua-{app-server,gui-readiness,native-host}.test.mjs
node --test js/nanocodex-computer/test/*.test.mjs
cargo test -p nanocodex-computer
NANOCODEX_TEST_CODEX_BIN=/absolute/path/to/official/codex \
  node --test scripts/tests/openai-cua-headless-upstream.test.mjs
```

Focused tests cover headless thread creation, no GUI attachment, unchanged policy
and payloads, unresolved-request isolation, cancellation, leases, and child failure.
The opt-in real-binary test uses an isolated temporary home and synthetic provider
to verify that full-access policy accepts confirmations while read-only policy
declines them, without client approval requests. Native app checks must additionally establish that the signed helper
starts with the desktop GUI closed; a synthetic MCP result alone is not that proof.
