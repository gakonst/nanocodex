import {
  normalizeHostedMachines,
  type HostedMachine,
} from "../../tools/hostedMachine.mjs";

export const HOSTED_TOOLS_LEASE_MS = 60_000;
export const HOSTED_TOOL_CALL_TIMEOUT_MS = 120_000;
export const MAX_HOSTED_TOOLS_FRAME_BYTES = 256 * 1024;
export const MAX_HOSTED_TOOL_CATALOG_ENTRIES = 256;
export const MAX_HOSTED_TOOL_NAME_BYTES = 128;
export const MAX_HOSTED_TOOL_SCHEMA_BYTES = 64 * 1024;
export const MAX_HOSTED_TOOL_INPUT_BYTES = 128 * 1024;
export const MAX_HOSTED_TOOL_OUTPUT_BYTES = 128 * 1024;

const MAX_DESCRIPTION_BYTES = 8 * 1024;
const MAX_SUMMARY_BYTES = 2 * 1024;
const MAX_MESSAGE_BYTES = 2 * 1024;
const MAX_NONCE_BYTES = 128;
const MAX_OUTPUT_CONTENT_ITEMS = 64;
const MAX_OUTPUT_TOKEN_BUDGET = 1_000_000;
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const RESERVED_TOOL_NAMES = new Set(["exec", "tool_search", "wait"]);
const encoder = new TextEncoder();

export type HostedToolDefinition =
  | {
      type: "function";
      name: string;
      description: string;
      strict: boolean;
      parameters: Record<string, unknown>;
      output_schema?: Record<string, unknown>;
    }
  | {
      type: "custom";
      name: string;
      description: string;
      format: {
        type: "grammar";
        syntax: string;
        definition: string;
      };
    };

export type HostedToolCatalogEntry = {
  provider: string;
  remote_name: string;
  definition: HostedToolDefinition;
  parallel_safe: boolean;
  summary?: string;
  timeout_ms: number;
};

export type { HostedMachine } from "../../tools/hostedMachine.mjs";

export type HostedToolOutputContent =
  | { type: "input_text"; text: string }
  | { type: "input_image"; image_url: string; detail: "auto" | "low" | "high" | "original" }
  | { type: "input_audio"; audio_url: string };

export type HostedToolProcessTrace = {
  exit_code: number | null;
  session_id: number | null;
  original_token_count: number | null;
  output_bytes: number;
  wall_time_seconds: number;
};

export type HostedToolOutputWire = {
  output: string | HostedToolOutputContent[];
  success: boolean;
  structured_result: unknown | null;
  metadata: unknown | null;
  process_trace: HostedToolProcessTrace | null;
};

export type HostedToolCallOutcome =
  | { status: "completed"; output: HostedToolOutputWire }
  | { status: "unavailable"; message: string }
  | { status: "ambiguous"; message: string }
  | { status: "cancelled"; message: string };

export type HostedToolsHostFrame =
  | {
      type: "catalog";
      tools: HostedToolCatalogEntry[];
      machines?: HostedMachine[];
      attachment_id?: string;
    }
  | {
      type: "result";
      call_id: string;
      outcome: HostedToolCallOutcome;
    }
  | {
      type: "ping";
      nonce: string;
    }
  | { type: "drain" };

export type HostedToolsManagedFrame =
  | { type: "ready" }
  | {
      type: "call";
      session_id: string;
      call_id: string;
      model: string;
      name: string;
      input: Record<string, unknown> | string;
      output_token_budget: number;
      output_byte_budget: number;
      deadline_at: number;
    }
  | {
      type: "cancel";
      call_id: string;
    }
  | {
      type: "ack";
      call_id: string;
    }
  | {
      type: "pong";
      nonce: string;
    }
  | { type: "draining" };

export type HostedToolsFrame = HostedToolsHostFrame | HostedToolsManagedFrame;

const HOST_FRAME_TYPES = new Set(["catalog", "result", "ping", "drain"]);
const MANAGED_FRAME_TYPES = new Set(["ready", "call", "cancel", "ack", "pong", "draining"]);

export function parseHostedToolsHostFrame(encoded: string): HostedToolsHostFrame {
  const frame = parseHostedToolsFrame(encoded);
  if (!HOST_FRAME_TYPES.has(frame.type)) {
    throw new HostedToolsProtocolError(
      "wrong_direction",
      `${frame.type} is not a host-to-managed tools frame`,
    );
  }
  return frame as HostedToolsHostFrame;
}

export function parseHostedToolsManagedFrame(encoded: string): HostedToolsManagedFrame {
  const frame = parseHostedToolsFrame(encoded);
  if (!MANAGED_FRAME_TYPES.has(frame.type)) {
    throw new HostedToolsProtocolError(
      "wrong_direction",
      `${frame.type} is not a managed-to-host tools frame`,
    );
  }
  return frame as HostedToolsManagedFrame;
}

export function parseHostedToolsFrame(encoded: string): HostedToolsFrame {
  if (encoder.encode(encoded).byteLength > MAX_HOSTED_TOOLS_FRAME_BYTES) {
    throw new HostedToolsProtocolError(
      "message_too_large",
      `Hosted Tools frames are limited to ${MAX_HOSTED_TOOLS_FRAME_BYTES} bytes`,
    );
  }
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new HostedToolsProtocolError("invalid_json", "Hosted Tools frames must be JSON objects");
  }
  const frame = objectValue(value, "Hosted Tools frame");
  switch (frame.type) {
    case "catalog":
      return parseCatalog(frame);
    case "result":
      return parseResult(frame);
    case "ping":
      return parsePing(frame);
    case "drain":
      exactKeys(frame, ["type"]);
      return { type: "drain" };
    case "ready":
      exactKeys(frame, ["type"]);
      return { type: "ready" };
    case "call":
      return parseCall(frame);
    case "cancel":
      return parseCancel(frame);
    case "ack":
      return parseAck(frame);
    case "pong":
      return parsePong(frame);
    case "draining":
      exactKeys(frame, ["type"]);
      return { type: "draining" };
    default:
      throw new HostedToolsProtocolError("unknown_message", "unsupported Hosted Tools frame type");
  }
}

function parseCatalog(
  frame: Record<string, unknown>,
): Extract<HostedToolsHostFrame, { type: "catalog" }> {
  exactKeys(frame, ["type", "tools", "machines", "attachment_id"]);
  if (!Array.isArray(frame.tools) || frame.tools.length > MAX_HOSTED_TOOL_CATALOG_ENTRIES) {
    throw new HostedToolsProtocolError(
      "invalid_catalog",
      `tools must be an array of at most ${MAX_HOSTED_TOOL_CATALOG_ENTRIES} entries`,
    );
  }
  const tools = frame.tools.map((entry, index) => catalogEntry(entry, index));
  const machines = machineCatalog(frame.machines);
  const attachmentId = frame.attachment_id === undefined
    ? undefined
    : sourceIdentifier(frame.attachment_id, "attachment_id");
  if (machines !== undefined && machines.length > 0
    && (machines.length !== 1 || attachmentId !== machines[0]?.id)) {
    throw new HostedToolsProtocolError(
      "invalid_catalog",
      "a machine catalog requires one machine whose id equals attachment_id",
    );
  }
  const names = new Set<string>();
  const identities = new Set<string>();
  for (const tool of tools) {
    if (names.has(tool.definition.name)) {
      throw new HostedToolsProtocolError("invalid_catalog", `duplicate tool name: ${tool.definition.name}`);
    }
    names.add(tool.definition.name);
    const identity = `${tool.provider}\u0000${tool.remote_name}`;
    if (identities.has(identity)) {
      throw new HostedToolsProtocolError(
        "invalid_catalog",
        `duplicate provider tool identity: ${tool.provider}:${tool.remote_name}`,
      );
    }
    identities.add(identity);
  }
  return {
    type: "catalog",
    tools,
    ...(machines === undefined ? {} : { machines }),
    ...(attachmentId === undefined ? {} : { attachment_id: attachmentId }),
  };
}

function machineCatalog(value: unknown): HostedMachine[] | undefined {
  if (value === undefined) return undefined;
  try {
    return [...normalizeHostedMachines(value as readonly HostedMachine[])];
  } catch (error) {
    throw new HostedToolsProtocolError(
      "invalid_catalog",
      error instanceof Error ? error.message : String(error),
    );
  }
}

function parseResult(frame: Record<string, unknown>): Extract<HostedToolsHostFrame, { type: "result" }> {
  exactKeys(frame, ["type", "call_id", "outcome"]);
  return {
    type: "result",
    call_id: identifier(frame.call_id, "call_id"),
    outcome: callOutcome(frame.outcome),
  };
}

function parsePing(frame: Record<string, unknown>): Extract<HostedToolsHostFrame, { type: "ping" }> {
  exactKeys(frame, ["type", "nonce"]);
  return {
    type: "ping",
    nonce: boundedText(frame.nonce, 0, MAX_NONCE_BYTES, "nonce"),
  };
}

function parseCall(frame: Record<string, unknown>): Extract<HostedToolsManagedFrame, { type: "call" }> {
  exactKeys(frame, [
    "type", "session_id", "call_id", "model", "name", "input", "output_token_budget",
    "output_byte_budget", "deadline_at",
  ]);
  const input = typeof frame.input === "string"
    ? frame.input
    : objectValue(frame.input, "call input");
  boundedJson(input, MAX_HOSTED_TOOL_INPUT_BYTES, "input_too_large", "call input");
  return {
    type: "call",
    session_id: identifier(frame.session_id, "session_id"),
    call_id: identifier(frame.call_id, "call_id"),
    model: identifier(frame.model, "model"),
    name: toolName(frame.name),
    input,
    output_token_budget: boundedInteger(
      frame.output_token_budget,
      1,
      MAX_OUTPUT_TOKEN_BUDGET,
      "output_token_budget",
    ),
    output_byte_budget: boundedInteger(
      frame.output_byte_budget,
      1,
      MAX_HOSTED_TOOL_OUTPUT_BYTES,
      "output_byte_budget",
    ),
    deadline_at: timestamp(frame.deadline_at, "deadline_at"),
  };
}

function parseCancel(frame: Record<string, unknown>): Extract<HostedToolsManagedFrame, { type: "cancel" }> {
  exactKeys(frame, ["type", "call_id"]);
  return {
    type: "cancel",
    call_id: identifier(frame.call_id, "call_id"),
  };
}

function parseAck(
  frame: Record<string, unknown>,
): Extract<HostedToolsManagedFrame, { type: "ack" }> {
  exactKeys(frame, ["type", "call_id"]);
  return {
    type: "ack",
    call_id: identifier(frame.call_id, "call_id"),
  };
}

function parsePong(frame: Record<string, unknown>): Extract<HostedToolsManagedFrame, { type: "pong" }> {
  exactKeys(frame, ["type", "nonce"]);
  return {
    type: "pong",
    nonce: boundedText(frame.nonce, 0, MAX_NONCE_BYTES, "nonce"),
  };
}

function catalogEntry(value: unknown, index: number): HostedToolCatalogEntry {
  const entry = objectValue(value, `tools[${index}]`);
  exactKeys(entry, ["provider", "remote_name", "definition", "parallel_safe", "summary", "timeout_ms"]);
  if (typeof entry.parallel_safe !== "boolean") {
    throw new HostedToolsProtocolError("invalid_catalog", `tools[${index}].parallel_safe must be boolean`);
  }
  const definition = toolDefinition(entry.definition, index);
  return {
    provider: identifier(entry.provider, `tools[${index}].provider`),
    remote_name: identifier(entry.remote_name, `tools[${index}].remote_name`),
    definition,
    parallel_safe: entry.parallel_safe,
    ...(entry.summary === undefined
      ? {}
      : { summary: boundedText(entry.summary, 1, MAX_SUMMARY_BYTES, `tools[${index}].summary`) }),
    timeout_ms: boundedInteger(
      entry.timeout_ms,
      1,
      HOSTED_TOOL_CALL_TIMEOUT_MS,
      `tools[${index}].timeout_ms`,
    ),
  };
}

function toolDefinition(value: unknown, index: number): HostedToolDefinition {
  const definition = objectValue(value, `tools[${index}].definition`);
  const name = toolName(definition.name);
  const description = boundedText(
    definition.description,
    1,
    MAX_DESCRIPTION_BYTES,
    `tools[${index}].definition.description`,
  );
  if (definition.type === "function") {
    exactKeys(definition, ["type", "name", "description", "strict", "parameters", "output_schema"]);
    if (typeof definition.strict !== "boolean") {
      throw new HostedToolsProtocolError(
        "invalid_catalog",
        `tools[${index}].definition.strict must be boolean`,
      );
    }
    const parameters = objectValue(definition.parameters, `tools[${index}].definition.parameters`);
    if (!isObjectInputSchema(parameters)) {
      throw new HostedToolsProtocolError(
        "invalid_schema",
        `tools[${index}].definition.parameters must be an object JSON Schema`,
      );
    }
    boundedJson(
      parameters,
      MAX_HOSTED_TOOL_SCHEMA_BYTES,
      "schema_too_large",
      `tools[${index}].definition.parameters`,
    );
    const outputSchema = definition.output_schema === undefined
      ? undefined
      : objectValue(definition.output_schema, `tools[${index}].definition.output_schema`);
    if (outputSchema !== undefined) {
      boundedJson(
        outputSchema,
        MAX_HOSTED_TOOL_SCHEMA_BYTES,
        "schema_too_large",
        `tools[${index}].definition.output_schema`,
      );
    }
    return {
      type: "function",
      name,
      description,
      strict: definition.strict,
      parameters,
      ...(outputSchema === undefined ? {} : { output_schema: outputSchema }),
    };
  }
  if (definition.type === "custom") {
    exactKeys(definition, ["type", "name", "description", "format"]);
    const format = objectValue(definition.format, `tools[${index}].definition.format`);
    exactKeys(format, ["type", "syntax", "definition"]);
    if (format.type !== "grammar") {
      throw new HostedToolsProtocolError("invalid_schema", "custom tool format must be grammar");
    }
    const syntax = identifier(format.syntax, `tools[${index}].definition.format.syntax`);
    const grammar = boundedText(
      format.definition,
      1,
      MAX_HOSTED_TOOL_SCHEMA_BYTES,
      `tools[${index}].definition.format.definition`,
    );
    return {
      type: "custom",
      name,
      description,
      format: { type: "grammar", syntax, definition: grammar },
    };
  }
  throw new HostedToolsProtocolError(
    "invalid_catalog",
    `tools[${index}].definition.type must be function or custom`,
  );
}

function isObjectInputSchema(schema: Record<string, unknown>): boolean {
  if (schema.type === "object") return true;
  return Array.isArray(schema.oneOf)
    && schema.oneOf.length > 0
    && schema.oneOf.every((branch) => {
      const candidate = branch as Record<string, unknown> | null;
      return candidate !== null
        && typeof candidate === "object"
        && !Array.isArray(candidate)
        && candidate.type === "object";
    });
}

function callOutcome(value: unknown): HostedToolCallOutcome {
  const outcome = objectValue(value, "result outcome");
  switch (outcome.status) {
    case "completed":
      exactKeys(outcome, ["status", "output"]);
      return { status: "completed", output: toolOutput(outcome.output) };
    case "unavailable":
    case "ambiguous":
    case "cancelled":
      exactKeys(outcome, ["status", "message"]);
      return {
        status: outcome.status,
        message: boundedText(outcome.message, 1, MAX_MESSAGE_BYTES, "outcome message"),
      };
    default:
      throw new HostedToolsProtocolError(
        "invalid_outcome",
        "result outcome must be completed, unavailable, ambiguous, or cancelled",
      );
  }
}

function toolOutput(value: unknown): HostedToolOutputWire {
  const output = objectValue(value, "completed output");
  exactKeys(output, ["output", "success", "structured_result", "metadata", "process_trace"]);
  if (typeof output.success !== "boolean"
    || !Object.hasOwn(output, "structured_result")
    || !Object.hasOwn(output, "metadata")
    || !Object.hasOwn(output, "process_trace")) {
    throw new HostedToolsProtocolError(
      "invalid_output",
      "completed output requires success and all nullable output metadata fields",
    );
  }
  const body = typeof output.output === "string"
    ? output.output
    : outputContent(output.output);
  const wire: HostedToolOutputWire = {
    output: body,
    success: output.success,
    structured_result: output.structured_result,
    metadata: output.metadata,
    process_trace: output.process_trace === null ? null : processTrace(output.process_trace),
  };
  boundedJson(wire, MAX_HOSTED_TOOL_OUTPUT_BYTES, "output_too_large", "completed output");
  return wire;
}

function outputContent(value: unknown): HostedToolOutputContent[] {
  if (!Array.isArray(value) || value.length > MAX_OUTPUT_CONTENT_ITEMS) {
    throw new HostedToolsProtocolError(
      "invalid_output",
      `completed output content must have at most ${MAX_OUTPUT_CONTENT_ITEMS} items`,
    );
  }
  return value.map((item, index) => {
    const content = objectValue(item, `completed output content[${index}]`);
    if (content.type === "input_text") {
      exactKeys(content, ["type", "text"]);
      return {
        type: "input_text" as const,
        text: boundedText(content.text, 0, MAX_HOSTED_TOOL_OUTPUT_BYTES, `content[${index}].text`),
      };
    }
    if (content.type === "input_image") {
      exactKeys(content, ["type", "image_url", "detail"]);
      if (content.detail !== "auto" && content.detail !== "low"
        && content.detail !== "high" && content.detail !== "original") {
        throw new HostedToolsProtocolError("invalid_output", `content[${index}].detail is invalid`);
      }
      return {
        type: "input_image" as const,
        image_url: boundedText(
          content.image_url,
          1,
          MAX_HOSTED_TOOL_OUTPUT_BYTES,
          `content[${index}].image_url`,
        ),
        detail: content.detail,
      };
    }
    if (content.type === "input_audio") {
      exactKeys(content, ["type", "audio_url"]);
      return {
        type: "input_audio" as const,
        audio_url: boundedText(
          content.audio_url,
          1,
          MAX_HOSTED_TOOL_OUTPUT_BYTES,
          `content[${index}].audio_url`,
        ),
      };
    }
    throw new HostedToolsProtocolError("invalid_output", `content[${index}].type is invalid`);
  });
}

function processTrace(value: unknown): HostedToolProcessTrace {
  const trace = objectValue(value, "process_trace");
  exactKeys(trace, [
    "exit_code", "session_id", "original_token_count", "output_bytes", "wall_time_seconds",
  ]);
  const exitCode = nullableInteger(trace.exit_code, -2_147_483_648, 2_147_483_647, "exit_code");
  const sessionId = nullableInteger(trace.session_id, 0, Number.MAX_SAFE_INTEGER, "process session_id");
  const originalTokenCount = nullableInteger(
    trace.original_token_count,
    0,
    Number.MAX_SAFE_INTEGER,
    "original_token_count",
  );
  if (typeof trace.wall_time_seconds !== "number"
    || !Number.isFinite(trace.wall_time_seconds)
    || trace.wall_time_seconds < 0) {
    throw new HostedToolsProtocolError(
      "invalid_output",
      "wall_time_seconds must be a finite non-negative number",
    );
  }
  return {
    exit_code: exitCode,
    session_id: sessionId,
    original_token_count: originalTokenCount,
    output_bytes: boundedInteger(trace.output_bytes, 0, Number.MAX_SAFE_INTEGER, "output_bytes"),
    wall_time_seconds: trace.wall_time_seconds,
  };
}

function identifier(value: unknown, name: string): string {
  if (typeof value !== "string" || !IDENTIFIER.test(value)) {
    throw new HostedToolsProtocolError(
      "invalid_identifier",
      `${name} must be 1-${MAX_HOSTED_TOOL_NAME_BYTES} safe ASCII bytes`,
    );
  }
  return value;
}

function sourceIdentifier(value: unknown, name: string): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$/.test(value)) {
    throw new HostedToolsProtocolError(
      "invalid_identifier",
      `${name} must be 1-123 safe ASCII bytes`,
    );
  }
  return value;
}

function toolName(value: unknown): string {
  const name = identifier(value, "tool name");
  if (RESERVED_TOOL_NAMES.has(name)) {
    throw new HostedToolsProtocolError("invalid_identifier", "tool name is reserved");
  }
  return name;
}

function timestamp(value: unknown, name: string): number {
  return boundedInteger(value, 1, Number.MAX_SAFE_INTEGER, name);
}

function nullableInteger(value: unknown, minimum: number, maximum: number, name: string): number | null {
  return value === null ? null : boundedInteger(value, minimum, maximum, name);
}

function boundedInteger(value: unknown, minimum: number, maximum: number, name: string): number {
  if (!Number.isSafeInteger(value) || Number(value) < minimum || Number(value) > maximum) {
    throw new HostedToolsProtocolError(
      "invalid_integer",
      `${name} must be an integer from ${minimum} through ${maximum}`,
    );
  }
  return Number(value);
}

function boundedText(value: unknown, minimum: number, maximum: number, name: string): string {
  if (typeof value !== "string") {
    throw new HostedToolsProtocolError("invalid_string", `${name} must be text`);
  }
  const bytes = encoder.encode(value).byteLength;
  if (bytes < minimum || bytes > maximum) {
    throw new HostedToolsProtocolError(
      "invalid_string",
      `${name} must be ${minimum}-${maximum} UTF-8 bytes`,
    );
  }
  return value;
}

function boundedJson(value: unknown, maximum: number, code: string, name: string): void {
  const bytes = encoder.encode(JSON.stringify(value)).byteLength;
  if (bytes > maximum) {
    throw new HostedToolsProtocolError(code, `${name} is limited to ${maximum} encoded bytes`);
  }
}

function objectValue(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new HostedToolsProtocolError("invalid_message", `${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const fields = new Set(allowed);
  if (Object.keys(value).some((key) => !fields.has(key))) {
    throw new HostedToolsProtocolError("invalid_message", "Hosted Tools frame has unsupported fields");
  }
}

export class HostedToolsProtocolError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}
