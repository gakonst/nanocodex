export const DEVICE_HOST_PROTOCOL_VERSION = 1 as const;
export const DEVICE_HOST_LEASE_MS = 60_000;
export const DEVICE_TOOL_CALL_TIMEOUT_MS = 120_000;
export const MAX_DEVICE_HOST_MESSAGE_BYTES = 256 * 1024;

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const IDENTIFIER = /^[A-Za-z0-9._:-]{1,128}$/;
const encoder = new TextEncoder();

export type DeviceHostCommand =
  | {
    type: "attach";
    protocol_version: typeof DEVICE_HOST_PROTOCOL_VERSION;
    host_id: string;
    catalog_version: number;
  }
  | {
    type: "ping";
    lease_id: string;
    epoch: number;
    nonce?: string;
  }
  | {
    type: "device_tool_result";
    lease_id: string;
    epoch: number;
    call_id: string;
    success: boolean;
    output: unknown;
  };

export type DeviceHostServerMessage =
  | {
    type: "lease";
    protocol_version: typeof DEVICE_HOST_PROTOCOL_VERSION;
    lease_id: string;
    epoch: number;
    expires_at: number;
    catalog_version: number;
  }
  | {
    type: "pong";
    lease_id: string;
    epoch: number;
    expires_at: number;
    nonce?: string;
  }
  | {
    type: "device_tool_call";
    lease_id: string;
    epoch: number;
    call_id: string;
    tool: "phone";
    operation: string;
    arguments: Record<string, unknown>;
  }
  | {
    type: "ack";
    lease_id: string;
    epoch: number;
    call_id: string;
    state: "completed";
  }
  | {
    type: "fenced";
    epoch: number;
    reason: string;
  }
  | {
    type: "error";
    code: string;
    message: string;
  };

export type DeviceToolInput = {
  operation: string;
  arguments: Record<string, unknown>;
};

export type DeviceToolOutput =
  | { ok: true; status: "completed"; output: unknown }
  | { ok: false; status: "failed"; output: unknown }
  | { ok: false; status: "unavailable"; message: string }
  | { ok: false; status: "ambiguous"; message: string };

export function matchesDeviceHostLease(
  holder: { hostId?: string; leaseId?: string; epoch?: number },
  state: {
    host_id: string | null;
    lease_id: string | null;
    epoch: number;
    lease_expires_at: number;
  },
  now: number,
): boolean {
  return holder.hostId !== undefined
    && holder.hostId === state.host_id
    && holder.leaseId !== undefined
    && holder.leaseId === state.lease_id
    && holder.epoch !== undefined
    && holder.epoch === state.epoch
    && state.lease_expires_at >= now;
}

export function parseDeviceHostCommand(encoded: string): DeviceHostCommand {
  if (encoder.encode(encoded).byteLength > MAX_DEVICE_HOST_MESSAGE_BYTES) {
    throw new DeviceHostProtocolError(
      "message_too_large",
      `device-host messages are limited to ${MAX_DEVICE_HOST_MESSAGE_BYTES} bytes`,
    );
  }
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new DeviceHostProtocolError("invalid_json", "device-host messages must be JSON objects");
  }
  const command = objectValue(value, "device-host message");
  if (command.type === "attach") {
    exactKeys(command, ["type", "protocol_version", "host_id", "catalog_version"]);
    if (command.protocol_version !== DEVICE_HOST_PROTOCOL_VERSION) {
      throw new DeviceHostProtocolError("unsupported_version", "unsupported device-host protocol version");
    }
    return {
      type: "attach",
      protocol_version: DEVICE_HOST_PROTOCOL_VERSION,
      host_id: uuid(command.host_id, "host_id"),
      catalog_version: boundedInteger(command.catalog_version, 1, 2_147_483_647, "catalog_version"),
    };
  }
  if (command.type === "ping") {
    exactKeys(command, ["type", "lease_id", "epoch", "nonce"]);
    if (command.nonce !== undefined
      && (typeof command.nonce !== "string" || command.nonce.length > 128)) {
      throw new DeviceHostProtocolError("invalid_nonce", "ping nonce must be at most 128 characters");
    }
    return {
      type: "ping",
      lease_id: uuid(command.lease_id, "lease_id"),
      epoch: boundedInteger(command.epoch, 1, Number.MAX_SAFE_INTEGER, "epoch"),
      ...(command.nonce === undefined ? {} : { nonce: command.nonce }),
    };
  }
  if (command.type === "device_tool_result") {
    exactKeys(command, ["type", "lease_id", "epoch", "call_id", "success", "output"]);
    if (typeof command.success !== "boolean" || !Object.hasOwn(command, "output")) {
      throw new DeviceHostProtocolError(
        "invalid_result",
        "device_tool_result requires boolean success and an output value",
      );
    }
    return {
      type: "device_tool_result",
      lease_id: uuid(command.lease_id, "lease_id"),
      epoch: boundedInteger(command.epoch, 1, Number.MAX_SAFE_INTEGER, "epoch"),
      call_id: identifier(command.call_id, "call_id"),
      success: command.success,
      output: command.output,
    };
  }
  throw new DeviceHostProtocolError("unknown_command", "unsupported device-host command");
}

export function parseDeviceToolInput(value: unknown): DeviceToolInput {
  const input = objectValue(value, "phone input");
  exactKeys(input, ["operation", "arguments"]);
  const operation = identifier(input.operation, "operation");
  const args = input.arguments === undefined ? {} : objectValue(input.arguments, "phone arguments");
  const encoded = JSON.stringify(args);
  if (encoder.encode(encoded).byteLength > MAX_DEVICE_HOST_MESSAGE_BYTES / 2) {
    throw new DeviceHostProtocolError(
      "arguments_too_large",
      `phone arguments are limited to ${MAX_DEVICE_HOST_MESSAGE_BYTES / 2} bytes`,
    );
  }
  return { operation, arguments: args };
}

export function deviceToolResult(success: boolean, output: unknown): DeviceToolOutput {
  return success
    ? { ok: true, status: "completed", output }
    : { ok: false, status: "failed", output };
}

export function deviceToolUnavailable(message = "No Android device host is currently attached."): DeviceToolOutput {
  return { ok: false, status: "unavailable", message };
}

export function deviceToolAmbiguous(message: string): DeviceToolOutput {
  return { ok: false, status: "ambiguous", message };
}

function uuid(value: unknown, name: string): string {
  if (typeof value !== "string" || !UUID_V4.test(value)) {
    throw new DeviceHostProtocolError("invalid_identity", `${name} must be a lowercase UUID v4`);
  }
  return value;
}

function identifier(value: unknown, name: string): string {
  if (typeof value !== "string" || !IDENTIFIER.test(value)) {
    throw new DeviceHostProtocolError(
      "invalid_identifier",
      `${name} must be 1-128 letters, numbers, dots, underscores, colons, or hyphens`,
    );
  }
  return value;
}

function boundedInteger(value: unknown, minimum: number, maximum: number, name: string): number {
  if (!Number.isSafeInteger(value) || Number(value) < minimum || Number(value) > maximum) {
    throw new DeviceHostProtocolError(
      "invalid_integer",
      `${name} must be an integer from ${minimum} through ${maximum}`,
    );
  }
  return Number(value);
}

function objectValue(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new DeviceHostProtocolError("invalid_message", `${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const fields = new Set(allowed);
  if (Object.keys(value).some((key) => !fields.has(key))) {
    throw new DeviceHostProtocolError("invalid_message", "device-host message has unsupported fields");
  }
}

export class DeviceHostProtocolError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}

export class DeviceHostAmbiguousError extends Error {}
