# Main Thread runtime contract

All endpoints use the authenticated account and active team. Connect grants cannot
access them. Mutations require `agents:write`; all reads require `agents:read`.
The existing same-origin mutation policy applies.

| Endpoint | Request | Response |
| --- | --- | --- |
| `GET /v1/main-thread` | — | `{agent_id}`; 404 before initialization |
| `PUT /v1/main-thread` | Empty body or `{}` | `{agent_id}`; ensures the canonical Main |
| `GET /v1/projects` | — | `{data: [{id, name, coordinator_agent_id}]}` |
| `PUT /v1/projects/:id` | `{name, coordinator_agent_id?}` | `{id, name, coordinator_agent_id}` |

Project IDs contain 1–64 ASCII letters, digits, underscores or hyphens. Names are
trimmed, nonempty and limited to 160 characters. PUT creates or reuses a coordinator;
PUT on an existing ID renames it. A different coordinator for an existing ID is a
409 conflict. Explicit coordinator registration requires a live owned session in
the same team, verifies its current identity through the session RPC, and rejects
project children, Main, and coordinators already registered elsewhere. Existing
project roots may be registered without changing their conversations or children.
For legacy registry rows with a NULL team, attachment retries leave metadata
unchanged. Successful explicit registration first verifies the live session through
RPC and then stamps only the registry team column. Foreign owners and conflicting
non-NULL teams are rejected; session membership is never modified.
No endpoint migrates, deletes, or reparents conversations.

The account DO stores `main_threads` and `canonical_projects` separately from
`project_threads`. Creation keys include the team and project ID; agent creation
also scopes keys to the account. Existing conversation and direct steering endpoints
retain their behavior. Personal memory storage and scope are unchanged.

Only canonical Main exposes `list_projects`, `read_project`, and `route_project`.
Project coordinators and ordinary project conversations retain project-thread tools.
`route_project` accepts `{project_id, name, id, input}`. It freezes routing intent
before cross-object effects, reuses the coordinator, and admits a stable turn through
`ProjectThreadRuns`. Reusing the request ID with different content conflicts.

`ProjectThreadRuns` delivers initial results. A separate durable ordered completion
ledger publishes internal project-result turns atomically with their terminal state.
Parent subscriptions retain cursors and authorization epochs, survive eviction, and
admit notifications idempotently. This propagates late results from nested project
threads to coordinators and then Main, including after an initial response has
finished. Notifications reference exact turns and require reading the actual result;
they do not grant new authority or authorize restarting cancelled work. Revoked
subscriptions cannot resume through a stale retry. A fresh route under a newer
authorization epoch can establish a new subscription.

Completion watches are bounded to 128 active children per parent and eight due
children per alarm. Empty or failed polls back off from 30 seconds to five minutes;
new results or explicit admissions reset the delay. A caught-up subscription becomes
idle when both its local admission outbox and the child's entire admitted subtree
(including descendant watches and internal result turns) are quiet. Idle watches
schedule no alarms and release the durability export guard. New explicitly delegated
work resumes the retained cursor. A generation fence prevents an old idle observation
from retiring a newer admission; idle retirement never revives a revoked watch.

Exporting while a subscription is still active returns
`project_subscriptions_not_portable` rather than losing future outcomes. This guard
ends once the admitted subtree settles; a historical watch does not block export
forever. Registries and subscriptions are not personal-memory records.

Runtime system and startup context explicitly identify the session as Main, a
canonical project coordinator, a persistent project task, or an ordinary conversation.
Project roles include scoped project/root/parent identifiers, never project titles.
Main spawn preflight runs before deriving or creating any child agent.

A deleted Main retains its reserved canonical identity. Both GET and PUT
`/v1/main-thread` return 410 `main_thread_deleted`; ensure deliberately cannot
recreate or silently bind another conversation. No session membership is changed.

Retained completion grants are revalidated against the live account record and
organization membership before registry/feed reads and again at internal turn
admission. Matching cached session epochs alone is insufficient. Both initial
project-run delivery and recursive completion delivery require agents:read,
agents:write, and tools:use in both retained and current authority; a retained
capability cannot exceed the current grant. Revocation retires the subscription;
authorization-service 429/5xx responses preserve bounded retries. The private feed
RPC checks identity only; callers must perform this live authorization boundary.

Completion notification bodies contain stable project/agent/turn identifiers and
terminal state, without mutable project names or thread titles. A rename between
admission and acknowledgement replay cannot change the idempotent input hash.

## Protected identity deletion

The authoritative session DELETE reserves deletion in the owner account registry before
writing a session deletion marker or stopping its runtime. The account rejects Main,
explicit canonical coordinators, and server navigation self-roots with HTTP 409
`canonical_agent_deletion_forbidden`, including roots with NULL legacy team metadata.
Stored roles remain protected even when discovery filters them out; this guard does not
adopt metadata, migrate membership, replace deterministic identities, or delete content.
The session derives owner/team from its persisted identity, and a conflicting non-NULL
registry team returns 404. Registry failure leaves local deletion unstarted.

For ordinary conversations, the same synchronous account decision writes the existing
registry tombstone. Canonical registration rechecks active registry state after identity
validation, so either registration wins and deletion is rejected, or deletion wins and
registration is rejected. Session scope is reread after reservation before cleanup starts.
Navigation group membership alone does not prevent deletion; only its self-root is protected.
