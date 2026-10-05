# Claude project agents and forked skills

The native CLI discovers project agent definitions in `.claude/agents/**/*.md`.
Call `ListAgentProfiles` for a bounded catalog and diagnostics, then invoke
`Agent` with `subagent_type` set to a catalog name. `general-purpose` creates a
fresh child without a named profile; `fork` copies the native conversation at
its safe pre-tool-batch boundary.

```yaml
---
name: reviewer
description: Review changes without modifying files
model: haiku
tools: Read, Glob, Grep, Agent, TaskOutput
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
restriction chain, including an explicitly selected model. `SubmitResult`
remains available for the registry's result protocol. Profile definitions grant
no permissions. Cross-family delegation and Workflow execution are unavailable
inside a profiled child. Read restrictions also disable aggregate project,
profile and skill discovery and automatic project-context attachments.

The admitted profile is saved with the child workspace. `Agent` resume retains
its profile, model and workspace even if the project definition changes;
profile, isolation and model overrides on resume are rejected. Task-tree agent
IDs and retained runtimes remain process-local. A saved workspace binding alone
does not restore a child runtime after process restart.

## Isolated child worktrees

Set `isolation: worktree` in a profile or pass `isolation: "worktree"` to `Agent`.
The host creates a separate Git worktree and branch from the parent's current
HEAD. Uncommitted parent edits are not copied. The parent's workspace and branch
do not change. The parent's EnterWorktree permission and inherited profile
restrictions must permit creation. A Git repository with an existing HEAD is
required; existing destinations or branches are never adopted.

Agent and TaskOutput receipts include the isolated path and branch. A completed
child retains the worktree for resume. `CloseAgent` first closes the actual
registry subtree, then releases its workspace pins and removes only owned,
unchanged worktrees. Dirty, untracked or ignored files, new commits, changed
repository identities, and other active pins cause preservation with a reason
in the cleanup receipt. This is workspace isolation, not an OS sandbox: Bash
retains the CLI's configured host permissions.

## Forked skills

A skill may request `context: fork`, an optional named `agent`, a Claude `model`,
and `background: true`. The native Skill tool expands arguments and starts a
real clean registry child, with the existing profile/permission checks. Skill
forks do not copy the caller's conversation. Inline skills continue to return
expanded project guidance. `allowed-tools` remains metadata and grants no
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
selection, fresh skill context, real resume after definition edits, and Git
worktree cleanup/preservation.
