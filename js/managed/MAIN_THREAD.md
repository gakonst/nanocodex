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

Completion subscriptions are not portable yet: exporting an agent with an active
subscription returns `project_subscriptions_not_portable` rather than losing its
future outcomes. Registries and subscriptions are not personal-memory records.
