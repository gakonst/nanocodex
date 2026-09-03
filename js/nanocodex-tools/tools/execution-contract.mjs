export const EXEC_COMMAND_PARAMETERS = Object.freeze({
  type: "object",
  properties: {
    cmd: { type: "string", description: "Shell command to execute." },
    justification: { type: "string", description: "User-facing approval question for `require_escalated`; omit otherwise." },
    workdir: { type: "string", description: "Working directory for the command." },
    shell: { type: "string", description: "Shell binary to launch. Defaults to the selected hand's shell." },
    login: { type: "boolean", description: "True runs with login-shell semantics when supported." },
    tty: { type: "boolean", description: "True allocates a PTY when supported." },
    yield_time_ms: { type: "number", description: "Wait before yielding output. Defaults to 10000 ms." },
    max_output_tokens: { type: "number", description: "Output token budget. Defaults to 10000 tokens." },
    prefix_rule: { type: "array", items: { type: "string" }, description: "Reusable approval prefix for `require_escalated`." },
    sandbox_permissions: { type: "string", enum: ["use_default", "require_escalated"], description: "Per-command sandbox policy request." },
  },
  required: ["cmd"],
  additionalProperties: false,
});

export const WRITE_STDIN_PARAMETERS = Object.freeze({
  type: "object",
  properties: {
    session_id: { type: "number", description: "Session identifier returned by exec_command." },
    chars: { type: "string", description: "Characters to write; omit or pass an empty string to poll." },
    yield_time_ms: { type: "number", description: "Wait before yielding more output." },
    max_output_tokens: { type: "number", description: "Output token budget." },
  },
  required: ["session_id"],
  additionalProperties: false,
});

export const EXECUTION_OUTPUT_SCHEMA = Object.freeze({
  type: "object",
  properties: {
    chunk_id: { type: "string", description: "Chunk identifier included when the response reports one." },
    wall_time_seconds: { type: "number", description: "Elapsed wall time spent waiting for output in seconds." },
    exit_code: { type: "number", description: "Process exit code when the command finished during this call." },
    session_id: { type: "number", description: "Session identifier to pass to write_stdin when the process is still running." },
    original_token_count: { type: "number", description: "Approximate token count before output truncation." },
    output: { type: "string", description: "Command output text, possibly truncated." },
  },
  required: ["wall_time_seconds", "output"],
  additionalProperties: false,
});

export const MACHINE_PREVIEW_PARAMETERS = Object.freeze({
  type: "object",
  properties: {
    port: { type: "integer", minimum: 1024, maximum: 65_535, not: { const: 3_000 } },
  },
  required: ["port"],
  additionalProperties: false,
});

export const PREVIEW_OUTPUT_SCHEMA = Object.freeze({
  type: "object",
  properties: {
    port: { type: "integer" },
    url: { type: "string" },
    persistent: { type: "boolean" },
  },
  required: ["port", "url", "persistent"],
  additionalProperties: false,
});
