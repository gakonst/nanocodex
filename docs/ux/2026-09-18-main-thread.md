# Main Thread

Main Thread is the durable global conversation above projects. The hierarchy is:

```text
Main Thread
  Project coordinator (existing or new)
    Persistent project task thread
      Bounded in-process subagents
```

Every durable conversation remains directly accessible for user messages and steering. A project coordinator continues to use `spawn_project_thread` and `send_project_thread` for independent work. Main Thread uses `list_projects`, `route_project`, and `read_project` to reuse a project's coordinator and collect its outcomes.

## Identity and access

The account-owned registry records one Main Thread per team and stable canonical project IDs, names, and coordinator references. Main Thread relationships are separate from `project_threads`: making a project visible to Main Thread must not make sibling projects visible to project-scoped tools. The authenticated principal supplies account and team scope. Requests cannot provide a different owner or team. Connect grants cannot use global routing.

Existing conversations remain in place. Explicit coordinator registration validates the existing session and rejects children, conflicting project assignments, and Main Thread itself. Project registration does not mutate session ownership or move conversations. Personal memory uses the existing personal-memory implementation.

## Public protocol

- `GET /v1/main-thread` looks up the current Main Thread; `PUT /v1/main-thread` ensures it with a stable server-derived creation identity.
- `GET /v1/projects` lists canonical projects in the authenticated scope.
- `PUT /v1/projects/:id` accepts `{name, coordinator_agent_id?}`. Omitting the coordinator creates one when needed; an existing project reuses its coordinator. An explicit coordinator registers an existing conversation. Reassignment conflicts.
- `Agent.mainThread(options)` returns a standard managed agent handle.
- `Agent.projects.list(options)` and `Agent.projects.put(id, project, options)` expose the project protocol to JavaScript clients.

Stable route request IDs and the retained admission outbox prevent an ambiguous response from creating duplicate work. Completion notifications are internal task data, not new user instructions or expanded authorization. The coordinator's summary and routing choices remain model behavior; protocol tests verify delivery and isolation independently of model choices.

## Validation

Client contract: 54 managed-agent tests pass, including Main Thread reuse, normal conversation handles, authorization errors, project registration, malformed responses, and rejection of caller-selected team scope. The JavaScript type contract passes after building its workspace dependency. The account proxy's 14 tests pass, including unchanged authenticated forwarding and exact route matching for Main Thread and projects.

Web: the four focused API tests and integrated account typecheck pass after workspace dependency builds. The isolated browser component journey verifies repeated ensure, coordinator selection, failure, and retry. [Web navigation fixture](media/main-thread-web-navigation.png) is component evidence with a mocked API, not a deployed end-to-end run.

Desktop: the integrated Main Thread/runtime protocol suites pass all 34 tests. The isolated desktop worktree also passed its runtime build, native Debug build, and four selected native tests covering identity reuse, drafts, account-switch fencing, project navigation, and the existing queue/steering flow. The native build reused ignored prerequisite VoiceCore/Hand artifacts; it did not replace the installed application.

The package packing check requires generated `pkg-web/nanocodex.js`, absent in this source checkout, and therefore did not run to completion. Mobile: the integrated InboxCore suite completed 208 tests with five expected live-integration skips and zero failures. The production-method harness passed ensure coalescing, account reset, stale success/error/defer rejection, refresh races, selection preservation, and alias policy. The simulator test build passed in the isolated mobile worktree, but simulator startup prevented UI test execution. Backend validation is recorded in the PR once integrated checks complete. No production deployment or live-model routing claim is implied by fixture tests.
