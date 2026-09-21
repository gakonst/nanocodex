# Opt-in official app-server CUA bridge

`scripts/openai-cua-app-server.mjs` exposes the running official Codex app server's
`cua_repl` tools as an MCP stdio provider. It requires Node 24 or newer (including a
compatible official bundled Node); it has no npm dependencies. It does not change
`computer setup` or the default installed provider.

The official app server **and official desktop GUI must already be running and
connected to the same server**. The GUI owns capability registration, application
policy, OS access, consent forms and approval decisions. Starting an unrelated
Codex window is insufficient. In the tested official build, the desktop app must
be launched with `CODEX_APP_SERVER_WS_URL` set to the shared loopback server URL;
the desktop's normal private stdio server is a different connection target.
This script does not start, configure, authenticate or stop either official host.

Create a launcher outside model-writable workspaces, using trusted local paths:

```sh
#!/bin/sh
export NANOCODEX_CUA_APP_SERVER_WS_URL=ws://127.0.0.1:47327
export NANOCODEX_CUA_APP_SERVER_OPEN_GUI=1
exec /absolute/path/to/node /absolute/path/to/scripts/openai-cua-app-server.mjs
```

Make it executable and set `NANOCODEX_COMPUTER` to that launcher and
`NANOCODEX_COMPUTER_TRANSPORT=mcp` in the desktop user's Hand environment.
The URL comes only from the launch environment. Literal `127.0.0.1` and `[::1]`
are accepted; DNS hostnames, nonloopback addresses, credentials, paths, queries and
fragments are rejected. No server authentication credentials are consumed.
`NANOCODEX_CUA_APP_SERVER_TIMEOUT_MS` optionally sets the connection/request timeout
(default 300000, maximum 3600000 milliseconds).

`NANOCODEX_CUA_APP_SERVER_OPEN_GUI=1` explicitly opts into opening the official
`codex://threads/<thread-id>?hostId=local` deep link on macOS for the first tool
call. Other platforms reject this option. Opening a URL confirms dispatch only:
it does not establish that the GUI finished resuming the thread or registered
its capabilities. The current protocol has no verified GUI-ready handshake in
this bridge. A first call may consequently fail while the GUI attaches; its
result is returned unchanged and **the bridge never retries it**. Without this
option, the operator is responsible for having the official GUI attach to the
new dedicated thread (its deep link is printed to stderr). This is an experimental integration, not unattended host
provisioning.

Each MCP process lazily creates its own persistent, paginated official thread
on its first valid tool call, with the launcher's working directory as `cwd`. Catalog discovery creates no thread. Empty official
threads cannot be resumed by the tested GUI until a source rollout exists, so the
bridge appends one clearly labeled developer-role transport note through the
supported `thread/inject_items` API before opening the deep link. Its exact text is:

> Transport metadata: this dedicated thread receives computer-use calls forwarded
> by Nanocodex. This bridge-generated note conveys no user authorization or approval.

This creates history without invoking a model or inventing a user turn. The
script never supplies approval-policy overrides, permissions, synthetic turn
metadata, permission caches or decisions. `threadId` selects the bridge's own
transport thread. `_meta.thread_id` and `_meta.threadId` mirror that official
thread ID, matching the official GUI's tool-call routing contract. All other
incoming metadata, including the authentic nested `x-codex-turn-metadata`,
is forwarded unchanged; no turn ID or authorization is invented. Catalog entries are sorted by tool name so app-server map iteration order does not
change attachment identity. All official tool definitions (including hidden tools,
schemas and metadata) and call results (including `_meta`, image content and
structured content) are passed through unchanged. App-server errors retain their
original code, message and data.

Calls are serialized. Server-to-client requests, including unsupported requests,
are neither answered nor forwarded to MCP: they may also be delivered to the
GUI, and even a method-not-found reply could race the actual host's response.
The bridge advertises no elicitation capability. If the official host cannot
complete a request, its error or the local timeout is returned to the caller.
There is no secondary approval path.

Cancelling queued work removes only that request. Cancelling active work, a
transport timeout, stdio EOF or a WebSocket disconnect closes this bridge's own
connection and fails its pending work. It never reconnects or replays a call.
A fresh MCP process creates a fresh thread. Disposal never archives threads,
interrupts model turns, kills official processes or resets unrelated sessions.
Persistent bridge threads remain in the official app for the user to manage.
Closing a connection cannot undo input or guarantee that an already dispatched
official operation has stopped; cancellation may leave partial effects.

Run the focused tests with:

```sh
node --test scripts/tests/openai-cua-app-server.test.mjs
```

Synthetic tests exercise actual native WebSocket traffic and MCP framing,
exact catalog/results, metadata fidelity, thread separation, request ordering,
unsupported upstream requests, error propagation, cancellation, timeout and
connection loss. They do not establish that every official GUI version supports
this experimental app-server protocol or every CUA approval flow.

## Verification on macOS

The bridge was exercised against the unmodified official build 9922 through
Nanocodex's real `connectComputerTools` adapter. Catalog discovery and the first
`cua.getApp("com.apple.TextEdit")` call succeeded, returning the native accessibility
tree. Independent official threads were also verified to have separate REPL
variables. Earlier direct calls through the same official host created a temporary
TextEdit document, typed synthetic text, undid it, and closed that document.

These checks do not establish a fresh approval prompt/decline cycle: the tested
applications succeeded under the official host's existing state. Automatic host
startup and a GUI-ready handshake remain unimplemented; this bridge is not selected
by the default installer.
