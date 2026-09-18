# Persistent goals

Managed Nanocodex agents support a persistent objective through `/goal` and the
`get_goal`, `create_goal`, and `update_goal` tools. Nanocodex2 exposes Goal in its
slash-command menu. Commands are interpreted by the managed service, so inspecting
or changing a goal does not require a model call.

```text
/goal Finish the migration and verify every acceptance criterion
/goal
/goal pause
/goal resume
/goal edit Finish the migration, including the compatibility tests
/goal budget 100000
/goal budget none
/goal clear
```

`/goal` and `/goal status` show the objective, status, elapsed work time, and token
usage. An unfinished goal must be explicitly edited or cleared before setting a
new objective. Editing preserves its accounting. A token budget is optional;
removing or increasing an exhausted budget does not itself resume the goal.

An active goal continues when the agent becomes idle. The persisted objective
survives reconnects and Worker restarts. Pause and clear stop automatic continuation;
resume explicitly activates the goal again. Cancellation and failed work must not
silently restart the goal.

The model can read the goal, create one when explicitly requested, and mark it
`complete`, `blocked`, or `paused`. It cannot use `update_goal` to resume work or
change the objective or budget. Completion requires checking the full objective
against current evidence. Blocking requires the same genuine impediment across
three consecutive goal turns. Pausing requires a user request. These semantic
completion and blocker audits are model instructions, not independent verification.

## Reference and implementation

The port follows `codex-rs/ext/goal` at OpenAI Codex revision
`1427825c4044d48b513c7d4ea32b84e58806a188`, including its full continuation audit.
Nanocodex owns goal storage, accounting, and continuation in the managed Worker;
it does not depend on an OpenAI Codex installation.

The legacy native `nanocodex run` runtime and the Rust local managed server do not
implement goal execution. The local managed server rejects `/goal` explicitly.
Use Nanocodex2 with the Cloudflare managed service for persistent goals.

Budgets count uncached input plus output tokens, including observed descendant
model usage. They are checked at model-response boundaries; a response already in
flight can exceed the remaining allowance. They are not a dollar spending limit.

Goal state is currently local to its managed agent. Clear the goal before using
agent durability export; exporting an agent with a retained goal is rejected to
avoid silently losing its objective and accounting. Goal command objectives are
limited to 4,000 Unicode characters and cannot include attachments; refer to files
in the objective when the full specification is larger.
