# Controlling a running TUI

Both `nanocodex` (native) and `nanocodex2` (managed) expose a versioned JSONL
control socket on macOS and Linux. This implements the integration requested in
[#342](https://github.com/gakonst/nanocodex/issues/342). Commands go to the running
application and its existing agent owner. They do not create a second agent.

## Discovery and SSH

```sh
nanocodex tui list --json
nanocodex tui connect INSTANCE_UUID --stdio
# The same commands work with nanocodex2, without a separate account login.
ssh -T my-host nanocodex tui list --json
ssh -T my-host nanocodex tui connect INSTANCE_UUID --stdio
```

`connect` authenticates locally, emits the server's hello, and relays JSONL in
both directions. Keep stdin open while subscribing. One-shot clients may close
stdin after sending requests; pending replies drain before the connection closes.
It never puts the token on
the command line. SSH authenticates the remote user; no TCP listener is opened.

Registration files live at
`$CODEX_HOME/nanocodex/tui/instances/<instance-id>.json`, defaulting to
`~/.codex/nanocodex/tui/instances`. The TUI directories are mode 0700 and files
are mode 0600. Each file contains:

- `protocol_version: 1`, random `instance_id`, `pid`, `started_at_unix_ms`;
- `backend` (`native` or `managed`), private `socket_path`, random `auth_token`;
- `active_session_id`, decimal-string `active_generation`, and `conversation`.

`conversation` contains `session_id`, `root_session_id`, `parent_session_id`,
`origin`, `role`, and `rollout_path`. Unknown or unavailable values are null.
The file is replaced atomically when the composer target or its descriptor
changes. Clean shutdown removes it. A crash can leave a stale file: treat a
successful authenticated hello as the liveness check, never the PID alone.
`list` omits tokens and may include stale registrations.

The socket lives in its own private temporary directory. Both peers must have
the same Unix UID. Direct socket clients send this first, within five seconds:

```json
{"protocol_version":1,"instance_id":"INSTANCE_UUID","auth_token":"TOKEN_FROM_PRIVATE_FILE"}
```

Wrong identity, version, credentials, or oversized frames close the connection.
Do not copy registration tokens into logs. Set `NANOCODEX_TUI_CONTROL=off` before
starting the TUI to disable registration and the socket. An unsafe registration
directory is rejected rather than made public or silently reused.

## Requests and state

Requests contain `id`, `method`, and optional `params`. Replies contain the same
`id` and a `result`. Notifications contain `type`, `instance_id`, decimal-string
`seq`, `active_generation`, and `data`. The initial hello is:

```json
{"type":"hello","protocol_version":1,"snapshot":{"instance_id":"...","active_session_id":"...","active_generation":"1","seq":"42","state":{},"conversations":{},"active_turns":{},"snapshot_token":"...","live_history_truncated":false,"capabilities":{}}}
```

Use `state.get` for a fresh snapshot. `state` includes connection readiness,
execution (`idle`/`running`), `ui_blocked`, composer text/cursor/attachment
metadata, open menu, settings, `draft_revision`, and `settings_revision`.
Managed state also includes `active_turn_ids`, `managed_cursor`, and local shell
activity. Native `active_turns` maps session IDs to canonical running turn IDs.

`state.changed` announces revisions; fetch state to read the current draft.
Draft text is deliberately absent from the replay journal. `conversation.active_changed`
announces the new composer target and generation, including switching away and
back to the same session. A branch-preview menu does not change the composer
target. `conversations` contains known roots, branches, side conversations, and
subagents. Ancestry does not determine which conversation accepts input.

Neither TUI currently implements an interactive question queue. State reports
`questions.supported: false` and capabilities report `questions_read: false`;
clients must not interpret this as an empty, supported queue.

| Method | Parameters / result |
| --- | --- |
| `state.get` | Complete snapshot, including a live-history snapshot token. |
| `models.list` | Canonical model IDs and each model's supported effort IDs. |
| `settings.set` | Target fields below, `expected_settings_revision`, and exactly one of `settings.model` or `settings.effort`. |
| `prompt` | Target fields and `input.text`. Literal text, including slash-prefixed text. Rejects if busy. |
| `steer` | Target fields, `expected_turn_id`, `input.text`. Never falls back to starting a turn. |
| `cancel` | Target fields and `expected_turn_id`. Cancels only that turn. |
| `request.get` | `request_id`: reads this process's retained command disposition. |
| `command.status` | Managed only: `expected_session_id`, `expected_turn_id`, `request_id`, for durable steer/cancel receipts. |
| `events.subscribe` | Decimal-string `after_seq`, exclusive. Streams replay then live notifications. |
| `history.live` | `snapshot_token`, optional `offset` (default 0) and `limit` (default 32, max 128). Returns `records`, `next_offset`, `has_more`. |
| `history.pending` | Page the private event journal at `boundary` from `snapshot.pending_history`, with optional `cursor`, `order` (`newest` default or `oldest`), and `limit` (max 16). An optional live `snapshot_token` also pins the boundary. |
| `history.pending.read` | Read a chunked journal record using `boundary`, `record_id`, and optional byte `offset`. |
| `history.list` | `expected_session_id`, optional `limit`. Native: newest-first byte `cursor`, optional fixed `boundary`, `order` (`newest` or `oldest`), max 16 records. Reads known main/archived/side conversations without changing focus. Managed: existing `before` durable cursor, max 256 records. |
| `history.read` | Native: `expected_session_id`, `boundary`, `record_id`, and optional byte `offset` to retrieve an oversized rollout record. |

Native model selection follows the existing backend restriction: before the
first accepted turn. Effort changes use the existing backend setter. Native
settings target the selected main conversation; side panes report
`settings.mutable: false` and reject settings changes. Managed settings follow
the service's validation. Both use settings revisions to reject stale writes.

## Admission and draft preservation

For every mutation, take these target fields from a recent snapshot:

```json
{"id":"client-request-001","method":"prompt","params":{"expected_instance_id":"INSTANCE_UUID","expected_session_id":"SESSION_ID","expected_active_generation":"1","input":{"text":"Explain this function"}}}
```

For steering or cancelling add `expected_turn_id`. For settings add
`expected_settings_revision`. IDs should be unique and use 1–128 characters
from `[A-Za-z0-9._:-]`, compatible with the managed service.

External input has its own submitted-item identity. It never consumes,
replaces, clears, or appends to the keyboard draft, cursor, pasted content, or
attachments. The protocol has no draft-writing method. Modal editors,
confirmation dialogs, loading sessions, and stale targets reject mutations.
External editors keep state readable and reject mutations while
open; lifecycle events queued by the UI drain when it closes.

Command dispositions are:

- `accepted`: the backend acknowledged admission. Prompt receipts contain a
  canonical turn ID. Cancellation acceptance means the request was admitted,
  not that all work has already stopped; observe the terminal event.
- `rejected`: no admission through this request; `code` explains why.
- `pending`: the owner has not resolved admission yet. Query `request.get`.
- `unknown`: delivery cannot be established. Reconcile history/receipts; do not
  blindly repeat the action under a new ID.

Once queued for the owner, repeating the same ID and payload returns the retained
receipt without dispatching again. Changing its method or payload returns
`request_id_conflict`. Closing a socket does not abandon its command. Resolution
also emits `request.resolved`. Receipts are never evicted during the process;
when capacity is exhausted, new mutations return `request_capacity`.

A new process has a new instance ID. Native deduplication and settings receipts
are process-scoped. Never reinterpret an old request as rejected after a restart.
Managed prompts also use the service's existing durable submission ID. Managed
steer/cancel send an `idempotency-key`; the service persists dispatch intent
before effects, binds it to turn/payload/authority, and retains the result. An
interruption between dispatch and recording the result remains explicitly
unknown and is never redispatched. These receipts belong to that managed agent's
storage; they are not a claim of exactly-once effects or portable history.

## Events, history, and reconnects

`agent.event` carries the canonical native event; `managed.event` carries the
managed event (including its durable `cursor`) plus the owning `session_id`.
The managed event's numeric `agent_id`, when present, still identifies a
subagent. Consumers can use lifecycle, assistant delta/final, tool call/result,
and usage payloads without interpreting terminal bytes.

Native events carry `payload.turn_id`, also exposed by `Turn.id()` and written
in rollout turn records. Assistant deltas/finals carry `item_id` matching saved
response items. A provider that omits IDs receives one stable synthetic ID per
output index and model attempt. Retries receive separate identities. Managed
clients use the outer durable turn ID/cursor and nested canonical event. The
outer socket sequence is independent of both native event sequences and managed
cursors. Native `input.accepted` events contain `session_id`, `turn_id`, `item_id`,
`kind` (`prompt` or `steer`), optional caller `request_id`, and the complete ordered
`input` (text or multimodal attachment descriptors). They describe admitted input,
not drafts or confirmation that a steer has already been consumed by the model.
The same fields are persisted as `event_msg` / `input_accepted` rollout records,
including inputs accepted before a cancellation or failure. Use these as semantic
user transcript rows when present; legacy `user_message` and model-context user
items remain for compatibility and should not be rendered as additional inputs.
External prompt and
steer request IDs are correlated with those records. Managed acceptance already
contains prompt input; nested `input.accepted` steering events are also durable.

Native `history.committed` follows successful rollout flushing independently of
whether the turn completed, failed, or was cancelled. It includes a decimal-string
`boundary`: the exclusive byte offset of committed records. Snapshots retain
these watermarks in `committed_history`, keyed by session. Read pages at that
boundary to reconcile exactly the saved prefix; a partial or failed write never
advances the writer's committed boundary.

1. Authenticate and save the hello snapshot's `seq` and `snapshot_token`.
2. Page `history.live` with that token for a compact semantic view. For complete
   recovery, page `history.pending` with `boundary` from `pending_history` and
   `order: "oldest"`. It contains the complete semantic event journal through that
   snapshot, including unfinished turns and inputs omitted from the compact view.
   Byte boundaries remain readable for the process lifetime even after compact
   snapshot tokens expire.
3. Subscribe after the saved `seq`; events produced while reading pages are
   replayed before live delivery. Persist the last fully applied sequence.
4. On reconnect to the same instance, subscribe after that sequence. Retain the
   journal byte boundary too: it can become the next forward page cursor to
   recover only the missing suffix if in-memory replay has expired.
5. On `replay_gap`, streaming pauses and the notification includes a fresh
   snapshot/token. Rebuild from its journal boundary, fetch committed history as
   needed, then explicitly subscribe after its `seq`. Managed outer acceptance,
   completion, failure, and cancellation determine root active-turn state; nested
   runtime terminals cannot override that authoritative state. A new instance
   requires a fresh attach and committed-history reconciliation.

Replay is bounded to 4,096 frames or 16 MiB. Semantic live history retains up to
512 records or 16 MiB, with four immutable snapshots; old tokens return
`snapshot_expired`. `live_history_truncated` explicitly reports dropped live
history. This flag concerns only the compact view: the private disk-backed
journal retains full semantic events for this process, including oversized
records, and is removed when its owner closes. Raw provider `api.event` frames
are excluded because semantic events already carry the transcript. Disk I/O
failures appear in `pending_history.error`; clients must not claim complete
recovery in that case. Disk usage grows with this process's event history.

Both journal and native rollout pages return `boundary`, `next_cursor`,
`has_more`, and records with byte-range `record_id`, `start`, `end`, and `bytes`.
Small records include `value`; large records include `chunked: true`. Fetch large
records with the corresponding `.read` method, preserving the same boundary.
Chunks return `encoding: "utf8-bytes"`, an array of bytes, `next_offset`,
`total_bytes`, and `has_more`. Concatenate the bytes before UTF-8/JSON decoding.
This works entirely over the authenticated socket or SSH relay. Reverse paging
seeks from the supplied cursor rather than rescanning preceding history.

Frames are bounded to 1 MiB. There are at most 32 socket clients and 32 queued
owner commands. Each connection permits 16 in-flight requests (64 across the
server); two slots at each level are reserved for cancellation. Replies may
arrive out of order and are correlated by request ID. Event delivery and request
reading continue while operations are pending, with one serialized writer.
Concurrent non-cancel mutations are rejected as `command_pending`; history reads
do not block cancellation. Writers have a 10-second timeout. Mutation receipts have a
65,536-entry limit and a 16 MiB admission budget. Slow consumers cannot block the
agent; reconnects outside retention receive a gap, never silently incomplete
replay. Managed reconnects deduplicate source cursors before accumulating deltas.

## Rollout lineage

New native `session_meta` records add `root_session_id`, `conversation_role`,
and `origin_kind` while preserving existing `source`, parent, and fork fields.
Roles are `root`, `branch`, `side_conversation`, and `subagent`. `/btw` uses a
side-conversation fork; ordinary and historical forks remain branches. Resume
preserves recorded root identity. Older rollouts remain readable; without recorded
root metadata, new descendants use the resumed session as their lineage root. Managed
sessions have remote history, so their local `rollout_path` is null; subagent
parent identity is null when the service event does not provide it.

Implementation boundaries: [`nanocodex-tui-control`](../bin/nanocodex/tui-control)
owns transport, replay, and command receipts; the two TUI adapters own UI policy;
the agent core owns canonical identities and lineage; managed service receipts
live in [`command-receipts.ts`](../js/managed/src/command-receipts.ts).
