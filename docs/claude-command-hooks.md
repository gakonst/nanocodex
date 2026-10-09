# Native Claude CLI command hooks

Pass `--claude --claude-hooks /absolute/path/settings.json` to explicitly enable
synchronous local command hooks. The CLI does not discover command hooks from
repository or home settings. The selected configuration also applies to native
Claude children. Commands execute with the current user's local permissions;
choose a settings file you trust.

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "^(exec_command|Write)$",
      "hooks": [{"type": "command", "command": "/absolute/path/check-tool", "timeout": 10}]
    }],
    "PostToolUse": [{
      "matcher": "exec_command",
      "hooks": [{"type": "command", "command": "/absolute/path/record-result"}]
    }],
    "PostToolUseFailure": [{
      "hooks": [{"type": "command", "command": "/absolute/path/record-failure"}]
    }]
  }
}
```

Matchers use Rust regular expressions against the native tool name; empty or
`*` matches every tool. Matching hooks run sequentially in configuration order.
For process calls, match `exec_command` or `write_stdin`; command rewrites use
`tool_input.cmd`. Hook executable configuration still uses `command`.
Unsupported events, non-command hook types, invalid matchers and invalid timeouts
reject the configuration before model inference.

Each command receives one JSON object on stdin followed by a newline. Fields are
`hook_event_name`, `session_id`, `turn_id`, `tool_use_id`, `tool_name`, `tool_input`,
`cwd`, `model`, and nullable `instruction_revision`. Post events add
`tool_response` and `is_error`; failure events also include `error`. The invocation
identity is stable across that tool's pre/post hooks. Hooks wrap native client
tools; provider-side server tools cannot be intercepted.

A successful command may produce empty stdout or a JSON object. Pre hooks can
return `{"continue":false,"stopReason":"reason"}` or
`{"decision":"block","reason":"reason"}` to deny execution. The native structured
form supports `hookSpecificOutput` with `hookEventName:"PreToolUse"`,
`permissionDecision` (`allow`, `deny`, or `ask`),
`permissionDecisionReason`, and object-valued `updatedInput`. `ask` blocks because
this host has no hook approval dialog. Updated inputs pass to subsequent hooks
and normal tool validation; hooks do not grant new capabilities.

Nonzero exits, malformed decision fields, malformed JSON, output overflow and
timeouts fail a pre hook and prevent execution. Exit 2 is reported as a blocking
hook exit, including bounded stderr. A post hook failure or blocking decision
appends an error to the original tool receipt, retaining its result and reminding
the caller that completed effects have not been undone. Failure hooks observe
handler failures, not denials or failures that prevent the handler from running.
Plan mode denies blocked model tool calls before configured hooks run. Allowed
inspection, context/skill loading, task-board and interaction calls still run their
configured hooks, including after a planning session is restored. Trusted hook
commands can have effects of their own. Plan mode is a dispatch policy, not OS
isolation or rollback of earlier effects.

Lifecycle commands also run at these native boundaries:

| Event | Boundary and matcher | Result handling |
| --- | --- | --- |
| `SessionStart` | First admitted root prompt; `startup` or `resume` | Observation and additional context |
| `UserPromptSubmit` | Before the submitted prompt reaches inference; no matcher value | Blocks on exit 2, any command error, `decision:block`, or `continue:false` |
| `Stop` | Completed root assistant response; no matcher value | Exit 2 or `decision:block` requests one more model round; `continue:false` stops |
| `PreCompact` | Before a summary request; `manual` or `auto` | Command failure or blocking decision prevents compaction |
| `PostCompact` | After a valid summary replaces history; `manual` or `auto` | Observation; failures preserve the summary |
| `StopFailure` | A Messages runtime/provider failure; `api_error` | Observation; preserves the original error |
| `SubagentStart` | First admitted child prompt; child profile/type | Observation and additional context |
| `SubagentStop` | Completed child assistant response; child profile/type | Same bounded continuation as `Stop` |
| `SessionEnd` | Explicit runtime shutdown after a started session; `other` | Observation; does not permanently close the durable conversation |

Embeddings opt in with `ClaudeToolHooks::handles_lifecycle(event)` and implement
`lifecycle`. Tool-only policies default to no lifecycle handling. The CLI checks
actual configured event matchers before admitting a durable effect, so absent
lifecycle configuration creates no lifecycle intent, outcome, or shutdown writes.

Lifecycle stdin uses `event_id` in place of tool identity and includes the event's
fields: `source`, `prompt`, `stop_hook_active`/`last_assistant_message`,
`trigger`/`custom_instructions` or `compact_summary`, `error`/`error_details`,
`agent_id`/`agent_type`, or `reason`. Session/turn identity, model, cwd and instruction
revision remain present. `event_id` is stable for durable reconciliation.
`SessionStart`, `UserPromptSubmit` and `SubagentStart` accept string
`hookSpecificOutput.additionalContext` with the matching `hookEventName`.
A second blocking Stop decision ends with an explicit error rather than looping
indefinitely; completed assistant content remains retained. Observational errors
remain diagnostics and never replace model output, summaries or tool receipts.

Asynchronous hooks, prompt/agent hook types, interactive hook approval, transcript
paths and unlisted lifecycle events are unsupported. Successful output fields
outside the documented subset are ignored.

Permission rules are checked before hooks and against the final rewritten input.
An `allow` hook decision cannot override a deny rule; a policy approval prompt
concerns the exact final call. Hook-requested `ask` remains unsupported.

Commands run through `/bin/sh -c` without a login shell, with the invoking session’s current workspace cwd,
`CLAUDE_PROJECT_DIR`, and `PATH=/usr/bin:/bin`. Other inherited environment variables
are cleared. Use absolute executable paths where needed. Settings and serialized
stdin are limited to 1 MiB each (stdin has one additional newline); stdout and
stderr are independently limited to 64 KiB. Timeout defaults to 60 seconds and
must be greater than zero and at most 600 seconds. The limit covers stdin writes,
process exit and pipe draining. Unix process groups are killed on completion,
failure, timeout or cancellation, including descendants holding pipes open.
This is process cleanup, not an isolation boundary for hostile hook executables.

Hooks run inside the admitted durable tool effect. Replaying a committed receipt
or completed request does not rerun its hooks. A crash before receipt commit
still requires reconciling hook side effects using invocation identity; arbitrary
shell effects are not exactly-once transactions.

Lifecycle commands additionally commit a started marker before calling the hook.
Recovery with a marker but no completed outcome reports uncertainty and does not
invoke the command again. This can conservatively skip a command if the process
died immediately before dispatch. Prompt and compaction gates then fail closed;
observational events preserve completed content. A completed request replays zero
hooks, including the shutdown receipt for each actually started runtime.

Run the shipped CLI acceptance journey with:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo +1.97.0 test -p nanocodex-bin --test claude_hooks -- --nocapture
```

The journey uses only a synthetic Messages provider; hook commands, tool effects,
HTTP/SSE and SQLite are real. Inspect commands, stdin logs, provider requests,
stdout/stderr and outcomes under `output/claude-hooks-cli/` and
`output/claude-lifecycle-cli/`. The lifecycle journey also kills the CLI during a
real hook process and checks that SQLite recovery reports an unknown outcome
without executing that hook again. Public manual-compaction and native-fork
coverage runs with `cargo +1.97.0 test -p nanocodex-claude --test lifecycle_hooks`;
its request and callback trace is `output/lifecycle-public-runtime.json`.
