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
These navigation endpoints do not migrate or reparent conversations. The existing
conversation deletion endpoint remains available. Deleting Main or a coordinator
atomically retires its canonical reference with the account tombstone and advances
an internal creation generation. A later ensure creates a fresh identity; exact
deletion retries do not advance the generation again, and deleted sessions are
never revived.

The account DO stores `main_threads` and `canonical_projects` separately from
`project_threads`. Creation keys include the team and project ID; agent creation
also scopes keys to the account. Existing conversation and direct steering endpoints
retain their behavior. Personal memory storage and scope are unchanged.

Only canonical Main exposes `list_projects`, `read_project`, and `route_project`.
Project coordinators and ordinary project conversations retain project-thread tools.
`route_project` accepts `{project_id, name, id, input}`. It freezes routing intent
and the current settings/configuration before cross-object effects. New coordinators
inherit that immutable creation snapshot, while existing coordinators retain their
own configuration. Routing reuses the coordinator and admits a stable turn through
`ProjectThreadRuns`. Reusing the request ID with different content conflicts.

`ProjectThreadRuns` delivers initial results. A separate durable ordered completion
ledger publishes internal result turns atomically with their terminal state. Only
notifications carrying a durable provenance marker written in the notification
admission transaction can publish; a caller-chosen turn ID prefix is insufficient.
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

Canonical routing tools validate live account authority before reading registry metadata.
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

## Explicit deletion and recreation

Session deletion first reserves retirement in its owner account. The account atomically
removes canonical references, advances creation generations, and tombstones the old
agent before session cleanup. Retries do not advance generations again. Persisted
owner/team scope is checked before retirement and reread before session cleanup;
registry unavailability leaves session deletion unstarted.

Projected navigation roots use their stable project-UUID identity for recreation.
Retirement leaves all conversation_projects assignments unchanged. Once the old root
is tombstoned, a fresh coordinator can register under that stable project ID; an active
root still cannot be replaced. No ensure, retry, or explicit registration revives an
old agent ID. GET discovery and role projection remain read-only, including NULL-team
roots validated through live identity.

Routing to a new canonical project freezes Main's settings and agent configuration
(including model and tool policy) before creation. The retained creation body is
keyed by canonical project ID and creation generation, so retries and other route IDs cannot substitute
later settings after an ambiguous creation response. Existing coordinators are
reused without configuration updates. Direct UI project creation keeps its default
configuration semantics. Creation uses the current checked route principal; no
capabilities or authorization are copied into the creation snapshot.

A route ID is also durably bound to its resolved coordinator before admission.
After deletion/recreation, retrying that route ID fails and requires a fresh route ID;
it cannot re-execute completed, cancelled, or retired work on the replacement.
Older admitted routes recover this binding from their durable project-run records.
Default UI ensure remains free to recreate canonical identities. Coordinator creation
snapshots remain immutable per project and generation, shared across route IDs within
that generation; a fresh route in a new generation may inherit updated Main settings.

`test/main-thread-runtime.test.ts` exercises production managed turn execution with
a scripted model transport: Main calls route_project, the new coordinator calls
spawn_project_thread, the child completes, then both coordinator and Main execute
internal completion turns. Assertions inspect actual durable terminal outputs,
inherited configuration/model settings, and fresh epoch-2 route renewal of an
epoch-1 revoked watch. SQL seeds fixture identity/configuration and that old watch;
it never fabricates terminal outcomes. Account/broker services and model responses
are fixtures, and alarm time is accelerated. This proves backend execution and
delivery, not a live provider or UI status rendering. Older project-thread-runs
cascade tests remain useful for eviction/replay boundaries but manually supply
outcomes and are not evidence of full model-driven execution.

Managed turn state `accepted` includes queued and actively executing turns; there
is no `running` state. `attempt_count` records retries rather than dispatches.
The runtime cascade regression checks completed terminal outputs through the public
turn read API instead of treating `accepted` with zero retries as dispatch failure.
