# Loopback backend contract

Source checked: `../nanocodex-api-reference/apple/InboxCore/Sources/InboxCore/ManagedClient.swift`, `apple/NanocodexInbox/InboxModel.swift`, `js/managed/src/index.ts`, `agent-settings.ts`, `durable-events.ts`, `conversation-projects.ts`, `project-threads.ts`, and `account-auth.ts` (Cloudflare implementation lives under js/managed, not a cloudflare directory).

All routes use the existing loopback Host / same-origin guards. POST requires a JSON object and application/json. Credentials are read server-side only from the existing CLI account store. Missing credentials give 401 on account operations; GET /api/status retains its 200 connected:false health envelope. No automatic write retries. Errors are `{error: string}`.

| Local route | Input | Result / exact upstream mapping |
| --- | --- | --- |
| GET /api/capabilities | none | `capabilities` boolean map and `unsupported_reason` |
| GET /api/projects | none | Native roster plus local organization; see below |
| GET /api/threads | project_id, optional closed | Native roster plus local organization; see below |
| GET /api/thread | thread_id | GET /v1/agents/{thread_id}; `{thread_id,state: upstream JSON}` including active turn information |
| GET /api/turn | thread_id, turn_id | GET /v1/agents/{thread_id}/turns/{encoded turn_id}; `{thread_id,turn_id,turn: upstream JSON}` |
| POST /api/turns/cancel | thread_id, turn_id, optional idempotency_key | Bodyless POST /v1/agents/{thread_id}/turns/{encoded turn_id}/cancel; `{thread_id,turn_id,receipt: upstream JSON}`. Receipt may be cancelling, not yet cancelled. A supplied idempotency_key is validated and echoed locally, not forwarded: upstream cancellation is identified by turn_id. |
| POST /api/turns/steer | thread_id, turn_id, text, idempotency_key (required stable identifier; legacy message_id alias accepted) | POST /v1/agents/{thread_id}/turns/{encoded turn_id}/steer with `{input:text,message_id:idempotency_key}`; `{thread_id,turn_id,idempotency_key,message_id,receipt: upstream JSON}`. If both aliases are supplied they must match |
| GET /api/messages | thread_id, optional limit (1..256, default 256), optional before OR after | GET /v1/agents/{thread_id}/events/history with same paging parameters. Returns existing messages, has_more, latest_cursor plus first_cursor,last_cursor and message_details (same visible messages with cursor,turn_id,event_type). |
| POST /api/send | Existing text/mode/context/thread_id/project_id/idempotency_key | Existing resume and Luna run behavior preserved |

Pagination cursors are decimal strings in 0..9223372036854775807 (never JS numbers). before must be >0; after can be 0. Pages contain ascending events. Use first_cursor for before and last_cursor for after; latest_cursor is a snapshot watermark, NOT the forward continuation cursor. has_more applies to the requested direction. Page boundaries refer to all upstream events, including filtered tool events; empty visible pages can still have more history. message_details permits reconciliation across page boundaries by turn_id/event_type (a later completed final supersedes streamed assistant messages for that turn). Existing messages remain `{role,text}` for compatibility.

## Local organization

All organization responses include `metadata_scope:"local_companion"`. Names, membership, and closed state are private to this companion/account credential on this machine. They DO NOT rename/archive shared account chats, establish server child-agent relationships, or sync with mobile/other devices. Native agent IDs are preserved. Native project membership/names are incorporated; local overrides take precedence.

- POST `/api/projects/create` `{name,idempotency_key}` -> `{project_id,thread_id,name,idempotency_key,status:"created",metadata_scope}`. Creates an empty real agent via POST /v1/agents with complete Luna settings, then stores its ID as the local project root.
- POST `/api/projects/update` `{project_id,name}` -> `{project_id,name,metadata_scope}`.
- POST `/api/threads/create` `{project_id,title,idempotency_key}` -> `{project_id,thread_id,title,idempotency_key,status:"created",metadata_scope}`. Creates an empty real agent, associates it locally. No first message. Omitting both project_id and title retains standalone creation (default title Untitled conversation).
- POST `/api/threads/update` `{thread_id,title}` -> `{thread_id,title,metadata_scope}`.
- POST `/api/threads/close` or `/api/threads/restore` `{thread_id}` -> `{thread_id,closed:true|false,metadata_scope}`. This only changes local list visibility, never cancels/deletes/archives the upstream agent.
- GET `/api/projects` -> `{projects:[{id,name}],metadata_scope}`.
- GET `/api/threads?project_id=ID&closed=false` -> `{threads:[{id,title,status:"unknown",closed}],metadata_scope}`. closed defaults false; true selects only locally closed threads.

Names/titles are trimmed nonempty strings up to 160 characters. All metadata mutations validate current ownership from authenticated GET /v1/agents; foreign or deleted IDs return 404. Auth failures propagate, without local fallback. Creation first retains the stable key and normalized request; changed reuse returns 409. Accepted IDs are persisted and reused on explicit retries. Uncertain upstream responses are not automatically retried; submitting the identical key reconciles against upstream idempotent creation. If local persistence fails, the key remains reusable. A replay validates current ownership and does not overwrite later renames/close state.

Storage: ~/.local/share/nanocodex-wow/<SHA256(origin + NUL + API key)>.json, 0600, bounded to 1 MiB, atomic replacement and fsync, protected by process/thread locking and no-follow opens. Directory is 0700; keys and scope hashes never appear in API responses. Rotating credentials selects a separate local scope. Corrupt/unsafe/oversized storage fails closed with 503; never silently resets. Shared upstream rename/archive and native child creation remain unsupported. Existing turn/history contracts remain unchanged.

Capabilities: `project_create`, `project_rename`, `project_metadata`, `chat_rename`, `chat_close`, `chat_restore`, `local_chat_membership`, `standalone_chat_create`, `thread_state`, `turn_state`, `turn_cancel`, `turn_steer`, `history_pagination` are true. `child_chat_create` (native agent delegation) and `chat_archive` (shared archive) remain false. GET /api/capabilities includes metadata_scope:"local_companion". POST /api/threads/child and /api/threads/archive return 501. Compatibility aliases /api/projects/rename and /api/threads/rename use the corresponding update body.

Upstream empty creation body is exactly `{settings:{model:"gpt-5.6-luna",thinking:"low",reasoning_mode:"standard",fast_mode:false}}`; the frontend supplies neither settings nor an initial message. Upstream account calls for a local transaction are pinned to the credential used to select the metadata scope. Rename/close/restore perform only GET /v1/agents upstream. A local project can be empty of open chats when its root is closed; closing never removes the project.

Reference evidence: conversation-projects.ts initializes a SQL table only; project-threads.ts membership mutation is internal registry code. Mobile InboxModel persists its project labels in UserDefaults. This companion implements its own device-local organization on top of public empty-agent creation, without modifying API source.
