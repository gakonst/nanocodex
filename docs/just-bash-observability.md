# Just Bash execution logs

Managed Cloudflare shell calls emit structured `managed.just_bash` events.
Managed2 emits `managed2.just_bash`. Each shell handler admission emits `queued`,
then `started` when its serial execution slot opens, then `finished` on a result
or exception. This includes input validation and lazy interpreter loading errors.
These events describe the embedded interpreter; native Hand executions have
separate telemetry.

In Cloudflare Workers Observability **Investigate**, use these structured-field
queries. Both managed Workers already retain logs with 100% head sampling;
Cloudflare's retention window and query sampling still apply.

Failed shell results:

```text
type = "managed.just_bash" AND phase = "finished" AND status = "error"
```

Search-admission refusals:

```text
type = "managed.just_bash" AND phase = "finished" AND category = "search_admission"
```

All shell calls in an affected thread, including successful fallback commands:

```text
type = "managed.just_bash" AND thread_id = "THREAD_ID"
```

Replace `THREAD_ID` with the observed opaque ID. Add `managed_turn_id` or
`parent_call_id` to narrow a retry sequence, and sort by timestamp.
Compare `tool_call_id`, `command`, `exit_code`, and `category` to find an `rg`
or `sed` failure followed by a `grep` call. A started event without a matching
finished event can indicate interrupted execution or incomplete retained logs;
it is not proof that a command never ran. For Managed2, substitute
`managed2.just_bash` for the event type and filter on `runtime_session_id`.

Managed shell events inherit canonical `thread_id` and `managed_turn_id` from
the surrounding tool invocation. `runtime_session_id` and `host_turn_id` retain
the separate interpreter context identities; `tool_call_id` and `parent_call_id`
join nested calls. Without an enclosing managed invocation, canonical IDs are
omitted rather than guessed. Managed2 currently emits runtime identities only.
Older shell events used `thread_id`/`turn_id` for runtime identities and
`parent_tool_call_id` for the parent: join historical records by `tool_call_id`
to `managed.tool.invocation` to recover their canonical thread/turn.

`exit_code` is the interpreter's actual result code, including nonzero normal
returns; it is null when the handler throws before producing a shell result.
`status` is `success` only for exit zero. A grep no-match exit of one therefore
appears as `error` with `command_exit`; it need not indicate a platform defect.
`duration_ms` measures execution including validation, initialization and
workspace refresh. `started.queue_ms` measures waiting for earlier shell calls.
`output_truncated` indicates public response truncation without retaining output.

`category` is a bounded enum: `none`, `search_admission`, `resource_limit`,
`timeout`, `cancelled`, `syntax`, `command_not_found`, `command_exit`,
`input_validation`, or `exception`. Classification of interpreter diagnostics is
best effort and bounded to the first 4096 characters; the exit code remains
authoritative. Timeout command exits of 124 are classified as timeout. Errors
whose identity is unavailable remain `exception` or `command_exit`.

`command` is an allowlisted first literal executable token; `command_scope` is
always `first_literal`. Dynamic commands, quoted executables, assignments,
control flow and unknown executables use `other`. It does not enumerate pipeline
members or commands inside shell scripts. For a search-admission failure,
`admission_command` may identify the refused `rg`/`grep`/`sed`/`awk` from its
bounded diagnostic, even when `command` names an earlier pipeline member. This
is best-effort attribution, not a general command execution trace. The first 128 source characters are
inspected without parsing the source again. Only fixed labels leave this check.

No raw source, arguments, environment, paths, URLs, stdout/stderr, error messages,
or content-derived hashes are emitted. The upstream Just Bash logger is not
enabled: its `exec`, `stdout`, and `stderr` records contain private content.
Observer exceptions and rejected observer promises are ignored so logging cannot
change a shell result. Correlation IDs come from the trusted tool context; no
model input is used to populate them. Managed2's surrounding SQLite setup/flush
phases retain their separate telemetry; shell events describe the interpreter
handler and do not claim a storage flush succeeded.

Validation:

```sh
node --test js/nanocodex/test/bash-telemetry.test.mjs
npm run build --prefix js/nanocodex-tools
node --test js/managed/test/just-bash-telemetry-journey.test.mjs
```

The workerd journey invokes the actual managed tool over HTTP with synthetic
workspace data and saves events, expected/observed statuses, and runtime logs in
ignored `output/just-bash-telemetry/`. It covers a no-match failure, `sed`/`grep`
retry, search admission refusal, invalid input, and recovery, and checks that
content sent to or returned from the tool is absent from telemetry. It is a
managed shell boundary check, not a whole model/session integration test.
