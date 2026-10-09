# Claude project agents and forked skills

Project skills with `context: fork` may select a profile from
`.claude/agents/**/*.md` using their `agent` frontmatter. Direct delegation uses
the shared `spawn_agent` contract, with no native profile catalog or selection
arguments. Every child starts with a fresh conversation.

Permission policies must use canonical tool names. Policies containing removed
agent aliases (including saved policies and scoped `Agent(...)` rules) fail
closed with a migration error before model execution.

```yaml
---
name: reviewer
description: Review changes without modifying files
model: haiku
tools: Read, Glob, Grep, spawn_agent, wait_agent
permissionMode: plan
---
Review the requested changes and return specific findings.
```

Profiles require a name and description. Supported fields are `model`, `tools`,
`disallowedTools`, `permissionMode`, and `isolation`. Tool lists accept YAML lists
or comma-separated exact tool names. Patterns and argument specifiers are not
supported. Models accept Claude model IDs, `opus`, `sonnet`, `haiku`, `fable`, or
`inherit`. Restrictive permission modes are `default`, `manual`, `dontAsk`, and
`plan`; project files cannot enable permission bypass. Unsupported fields,
symlinks, duplicate names and invalid profiles produce catalog diagnostics.
Discovery scans at most 256 entries, returns at most 64 profiles and reads at
most 32 KiB per definition.

The host selects the model before the first child request and appends the
profile instructions to native child instructions. Tool allow/deny lists and
permission mode intersect inherited host restrictions. Descendants inherit the
restriction chain, including an explicitly selected model. `submit_result`
remains available for the registry's result protocol. Profile definitions grant
no permissions. Cross-family delegation and Workflow execution are unavailable
inside a profiled child. Read restrictions also disable aggregate project,
profile and skill discovery and automatic project-context attachments.

The admitted profile is saved with the child workspace. `send_agent_message`
can delegate another task to an owned child while preserving its profile and
model. Live child runtimes remain process-local; durable roots restore the
registry's child identities, statuses, and committed conversation boundaries.

## Isolated child worktrees

Set `isolation: worktree` in a skill-selected profile.
The host creates a separate Git worktree and branch from the parent's current
HEAD. Uncommitted parent edits are not copied. The parent's workspace and branch
do not change. The parent's EnterWorktree permission and inherited profile
restrictions must permit creation. A Git repository with an existing HEAD is
required; existing destinations or branches are never adopted.

Use `tools.wait_agent(...)` to wait for a skill child and
`tools.close_agent(...)` to close it.
Closing releases subtree workspace pins and removes owned unchanged worktrees;
dirty or committed worktrees remain. This is workspace isolation, not an OS
sandbox: exec_command retains the CLI's configured host permissions.

## Forked skills

A skill may request `context: fork`, an optional named `agent`, a Claude `model`,
and `background: true` metadata. `tools.Skill(...)` inside Code Mode expands
arguments and starts a real clean registry child, with the existing
profile/permission checks. Skill forks return immediately with the canonical
spawn receipt, regardless of the background flag; use `tools.wait_agent` to
wait. They do not copy the caller's conversation. Inline skills continue to
return expanded project guidance. `allowed-tools` remains metadata and grants no
permissions. Dynamic shell interpolation and skill-defined hooks remain
unsupported.

Model invocation always uses model provenance. Skills marked
`disable-model-invocation: true` cannot be invoked by providing different JSON.
The portable `ClaudeSkills::execute` API refuses forked execution unless an
embedding supplies a child executor; it never silently returns a forked skill
as inline guidance.

Project `.claude/settings.json` and `.claude/settings.local.json` support
`skillOverrides` values `on`, `name-only`, `user-invocable-only`, and `off`.
Local entries take precedence. `name-only` hides catalog descriptions while
allowing invocation; `user-invocable-only` excludes model discovery and calls;
`off` excludes both callers. `on` preserves the skill's frontmatter restrictions.
Settings are reread for catalog and invocation; invalid, symlinked or oversized
settings fail closed with diagnostics. Each settings file is limited to 32 KiB,
and at most 256 combined override entries are accepted. This adapter reads only
project and local settings; it does not merge user or managed settings.

Run `cargo +1.97.0 test --locked -p nanocodex-bin --test claude_skills -- --nocapture`
for the shipped CLI journeys. Only model inference is simulated. Artifacts under
`output/claude-profiles-cli/` retain exact commands, provider requests, tool
receipts, terminal output and outcomes for profile restrictions, inherited model
selection, fresh skill context, canonical spawn denial, and Git
worktree cleanup/preservation.
