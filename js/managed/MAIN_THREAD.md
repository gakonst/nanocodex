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
