# Headless durable client

`durable_client.py` implements a real Python WebSocket transport and a drop-in
`DurableBackend` for `server.make_server`. It opens no browser, uses no clipboard,
and doesn't read or control the game. The daemon can serve the existing loopback
API for an addon bridge. No server or UI source changes are required.

## Run

Requires Python 3.10+ and `websockets>=15,<16` (tested with 15.0.1). Install that
package in the backend's virtual environment, then run:

```sh
python durable_client.py --port 17841 --thread YOUR_THREAD_ID
```

Saved subscriptions and pending prompts resume automatically on daemon restart.
Use `--thread` to subscribe to additional conversations. This process
stays in the foreground for a service supervisor; all network workers are invisible
background threads. Port 17841 avoids replacing the existing server on 17840.
The daemon has no browser/manual credential import flow. Production supervision
and addon-to-loopback wiring are outside these three files; no service was enabled
and no live game was touched.

For embedding:

```python
from durable_client import DurableBackend
from server import make_server

backend = DurableBackend()
backend.resume()
backend.subscribe(thread_id)
http = make_server(17841, backend)
try:
    http.serve_forever()
finally:
    http.server_close()
    backend.close()
```

## Credentials and persistence

The client calls the existing `Backend.credentials()` loader, including its
account-file version, origin, permissions and symlink checks. The official
`NANOCODEX_ACCOUNT_FILE` / `CODEX_HOME` account mechanism is unchanged. Keys appear
only in backend memory and the Authorization header, never a URL, route response,
log or SQLite record. WebSocket proxies and debug logging are disabled; the pinned
synchronous transport does not follow redirects. TLS uses default certificate
verification. A changed credential requires backend restart to prevent joining
one account's events with another account's state.

SQLite files live under `~/.local/share/nanocodex-wow/durable/`, scoped by a hash
of origin and credential. The directory must be 0700 and database 0600, owned by
the process user. The database contains private prompt and response content.
Event insertion, acknowledgement and cursor advancement share a FULL-synchronous
transaction. Duplicate or older cursors don't create duplicate events. The cursor
is stored as a signed 64-bit integer and exposed as a decimal string.
`backend.store.events(thread_id, after='0', limit=256)` reads committed raw events
for backend consumers. No retention pruning is implemented.

## Verified source contract

Source of truth in `/omarchy-desktop/nanocodex-api-reference`:

- `apple/InboxCore/Sources/InboxCore/ManagedClient.swift`, `stream`: Swift uses
  authenticated SSE `/v1/agents/{id}/events?cursor=...`, not WebSockets. Its idle
  check compares delivered progress to state without adopting the state cursor.
- `js/managed/src/index.ts`, agent `ws` routing, `#upgrade`,
  `#replayClientSocket`, `#dispatch`, `#submitManagedTurn`:
  authenticated GET upgrade at `/v1/agents/{id}/ws?cursor=N`. The API key needs
  `agents:read`, `agents:write`, and `tools:use`. First frame is `ready`, including
  `latest_event_cursor`, active turns, settings and capabilities. Missing cursor
  or `latest` skips historical replay, so this client always sends its committed
  numeric cursor (initially `0`). An ahead cursor returns HTTP 409. Archived
  events replay in ascending cursor order and transition to live broadcasts;
  there is no replay-complete frame.
- `js/managed/src/protocol.ts`: JSON text commands are `prompt` (`id`, `input`),
  `steer` (`id`, `input`), `cancel` (`id`), `status`, and `ping` (optional nonce).
  Server frames include `ready`, lifecycle events, nested `event`, `status`,
  `pong`, `stream_failed`, and `error`. Durable events carry string `cursor`,
  optional `created_at` and `turn_id`. Direct replay receipts may omit cursor.

The `ready` watermark gates pending submissions until historical replay catches
up. It never advances storage. An idle state probe detecting a newer durable
cursor reconnects from the committed cursor. Transport keepalives alone don't
prove event delivery. Reconnect backoff is 1–30 seconds plus up to 25% jitter;
stop is interruptible. Authentication, permission, unknown-thread, cursor-ahead,
protocol and local storage failures block the worker without resetting history.

A WebSocket `prompt.id` is both the explicit turn ID and request key. Repeating
that ID with identical input reconciles the same retained turn; changing input
conflicts. The client commits intent before transmission and retransmits pending
prompts with exactly the same ID/input after lost acknowledgements. It waits for
a lifecycle receipt before acknowledging local intent. This is idempotent
submission with deduplicated event storage, not a claim that all tool side effects
are exactly-once. Generic server errors lack request correlation, so they block
the worker and retain intent for review instead of guessing which request failed.

## Existing routes

- `POST /api/send` for an existing thread uses the durable WebSocket outbox and
  requires `idempotency_key`. It immediately returns `status: queued`, the stable
  turn ID, and `transport: websocket`. Queued means local persistence, not remote
  acceptance. Consumers must observe lifecycle events or existing turn/history
  routes to confirm acceptance and completion.
- New-conversation send still uses the existing REST `/v1/agent-runs` route, then
  subscribes to its events. This inherits its original explicit-reconciliation
  behavior; new-agent creation is not added to the WebSocket outbox.
- `GET /api/messages` and `/api/thread` preserve their existing REST responses and
  subscribe the thread. Existing history pagination remains authoritative.
- Cancel and steer retain the existing REST implementation. The WS steering
  command has no `message_id`, so it is intentionally not replayed as a durable
  command. REST steering retains the existing stable `message_id` behavior.
- Organization, model policy, voice and other routes inherit `Backend` unchanged.
  `/api/status` adds safe `durable_streams` states. `connected` retains its existing
  account-REST meaning; a stream's `connected` means replay reached its opening
  watermark, not that queued work has completed.

## Validation and blockers

Run `python -m unittest discover -s tests -p test_durable_client.py -v` in the
project with the dependency installed. Tests use actual local TCP WebSocket
handshakes and frames with synthetic credentials, including lost-ack reconnect,
replay gating, idle recovery, atomic rollback, restart persistence, deduplication,
conflicting IDs, blocked errors, account separation, automatic restart recovery, temporary DNS recovery and storage permissions.
These are local contract tests, not a live Nanocodex demonstration.

On September 18, 2026, the existing safe endpoint
`http://127.0.0.1:17840/api/status` reported `connected: false` with the existing
CLI-account-store sign-in error. No account file was inspected in command output.
Live authenticated WebSocket operation remains unverified until the official
backend account store is provisioned. No key was requested through chat or addon
UI. Addon integration must also handle the new `queued` admission state; the
assigned scope excludes changing its UI or the existing server.

Validation result: 12 focused tests and all 68 Python tests passed using the
isolated `a temporary virtual environment` environment. A temporary
daemon on port 17849 served `/api/status` with HTTP 200, `connected: false`,
`auth: cli-account-store`, and no active streams; it was stopped after that check.
No long-running daemon was left behind.

## Live evidence and UI handoff — September 18, 2026

User-provided live evidence: the selected Classic Beta character **Nanocodex
Ironside** returned **Character not found** on EnterWorld. No restart or logout
was performed. The parent retains screen ownership. This backend task performs
no screen or game actions.

The reported `/api/status` remains `connected: false`, `auth: cli-account-store`,
with the existing `nanocodex2 login` message. A usable key and authenticated
roundtrip have NOT been demonstrated.

UI integrator `d2c29004-7926-842c-8ff8-d0abdd01c5eb` owns `Core.lua`, `Bridge.lua`
and `Projects.lua`. Integration contract:

- Route the daemon consumer to its configured loopback port (default 17841);
  the existing server on 17840 was not replaced.
- Existing-thread sends require a stable `idempotency_key`. Preserve it and the
  exact input across uncertain submissions. The returned turn ID equals this key.
- Render `status: queued` as pending local delivery, never accepted or completed.
  Confirm remote lifecycle through turn/history responses or backend events.
- `durable_streams[thread_id]` reports stream state; neither stream `connected`
  nor account `connected` proves an individual turn completed.
- EventStore access is a Python backend interface, not an implemented HTTP event
  route. Existing `/api/messages` and `/api/turn` retain their REST contracts.
- New-thread creation, steering, cancellation and organization retain the existing
  REST behavior. There is no automatic WebSocket steering replay.

A direct handoff using `send_project_thread` was attempted but the runtime rejected
it with “only directly delegated threads can receive follow-ups.” This sibling
thread cannot message that integrator through that tool. The parent must relay
this persisted contract; delivery to the integrator is not confirmed.
