import { isVmFactoryName } from "./vm-factory-name";

export const VM_HOST_PROTOCOL_VERSION = 1 as const;
export const VM_HOST_LEASE_MS = 60_000;
export const MAX_VM_HOST_MESSAGE_BYTES = 256 * 1024;

const MAX_VMS = 64;
const MAX_CPUS = 64;
const MIN_MEMORY_MIB = 128;
const MAX_MEMORY_MIB = 262_144;
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const IDENTIFIER = /^[A-Za-z0-9._:-]{1,123}$/;
const encoder = new TextEncoder();

export type VmShape = {
  cpus: number;
  memory_mib: number;
};

export type VmHostAllocation = {
  allocation_id: string;
  generation: number;
  machine_id: string;
  state: "ready";
};

export type VmHostCommand =
  | {
    type: "attach";
    protocol_version: typeof VM_HOST_PROTOCOL_VERSION;
    host_id: string;
    factory_name: string;
    max_vms: number;
    vm: VmShape;
  }
  | {
    type: "ping";
    lease_id: string;
    epoch: number;
    nonce?: string;
  }
  | {
    type: "provisioned";
    lease_id: string;
    epoch: number;
    allocation_id: string;
    generation: number;
    machine_id: string;
  }
  | {
    type: "released";
    lease_id: string;
    epoch: number;
    allocation_id: string;
    generation: number;
    machine_id: string;
  }
  | {
    type: "reconcile";
    lease_id: string;
    epoch: number;
    allocations: VmHostAllocation[];
  };

export type VmHostServerMessage =
  | {
    type: "lease";
    protocol_version: typeof VM_HOST_PROTOCOL_VERSION;
    lease_id: string;
    epoch: number;
    expires_at: number;
    max_vms: number;
    vm: VmShape;
  }
  | {
    type: "pong";
    lease_id: string;
    epoch: number;
    expires_at: number;
    nonce?: string;
  }
  | {
    type: "provision";
    lease_id: string;
    epoch: number;
    allocation_id: string;
    generation: number;
    slot: number;
    machine_id: string;
    tool_attachment: {
      url: string;
      bearer: string;
    };
  }
  | {
    type: "release";
    lease_id: string;
    epoch: number;
    allocation_id: string;
    generation: number;
    machine_id: string;
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

export function matchesVmHostLease(
  holder: { hostId?: string; leaseId?: string; epoch?: number },
  state: {
    host_id: string;
    lease_id: string;
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

export function parseVmHostCommand(encoded: string): VmHostCommand {
  if (encoder.encode(encoded).byteLength > MAX_VM_HOST_MESSAGE_BYTES) {
    throw new VmHostProtocolError(
      "message_too_large",
      `VM-host messages are limited to ${MAX_VM_HOST_MESSAGE_BYTES} bytes`,
    );
  }
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new VmHostProtocolError("invalid_json", "VM-host messages must be JSON objects");
  }
  const command = objectValue(value, "VM-host message");
  if (command.type === "attach") {
    exactKeys(command, ["type", "protocol_version", "host_id", "factory_name", "max_vms", "vm"]);
    if (command.protocol_version !== VM_HOST_PROTOCOL_VERSION) {
      throw new VmHostProtocolError("unsupported_version", "unsupported VM-host protocol version");
    }
    return {
      type: "attach",
      protocol_version: VM_HOST_PROTOCOL_VERSION,
      host_id: uuid(command.host_id, "host_id"),
      factory_name: factoryName(command.factory_name),
      max_vms: boundedInteger(command.max_vms, 1, MAX_VMS, "max_vms"),
      vm: vmShape(command.vm),
    };
  }
  if (command.type === "ping") {
    exactKeys(command, ["type", "lease_id", "epoch", "nonce"]);
    return {
      type: "ping",
      lease_id: uuid(command.lease_id, "lease_id"),
      epoch: positiveInteger(command.epoch, "epoch"),
      ...optionalNonce(command.nonce),
    };
  }
  if (command.type === "provisioned" || command.type === "released") {
    exactKeys(command, [
      "type",
      "lease_id",
      "epoch",
      "allocation_id",
      "generation",
      "machine_id",
    ]);
    return {
      type: command.type,
      lease_id: uuid(command.lease_id, "lease_id"),
      epoch: positiveInteger(command.epoch, "epoch"),
      allocation_id: uuid(command.allocation_id, "allocation_id"),
      generation: positiveInteger(command.generation, "generation"),
      machine_id: identifier(command.machine_id, "machine_id"),
    };
  }
  if (command.type === "reconcile") {
    exactKeys(command, ["type", "lease_id", "epoch", "allocations"]);
    if (!Array.isArray(command.allocations) || command.allocations.length > MAX_VMS) {
      throw new VmHostProtocolError(
        "invalid_allocations",
        `reconcile allocations must be an array of at most ${MAX_VMS} VMs`,
      );
    }
    const allocations = command.allocations.map((value, index) => allocation(value, index));
    const allocationIds = new Set<string>();
    const machineIds = new Set<string>();
    for (const item of allocations) {
      if (allocationIds.has(item.allocation_id) || machineIds.has(item.machine_id)) {
        throw new VmHostProtocolError(
          "duplicate_allocation",
          "reconcile allocations must have unique allocation_id and machine_id values",
        );
      }
      allocationIds.add(item.allocation_id);
      machineIds.add(item.machine_id);
    }
    return {
      type: "reconcile",
      lease_id: uuid(command.lease_id, "lease_id"),
      epoch: positiveInteger(command.epoch, "epoch"),
      allocations,
    };
  }
  throw new VmHostProtocolError("unknown_command", "unsupported VM-host command");
}

function vmShape(value: unknown): VmShape {
  const shape = objectValue(value, "vm");
  exactKeys(shape, ["cpus", "memory_mib"]);
  return {
    cpus: boundedInteger(shape.cpus, 1, MAX_CPUS, "vm.cpus"),
    memory_mib: boundedInteger(
      shape.memory_mib,
      MIN_MEMORY_MIB,
      MAX_MEMORY_MIB,
      "vm.memory_mib",
    ),
  };
}

function allocation(value: unknown, index: number): VmHostAllocation {
  const item = objectValue(value, `allocations[${index}]`);
  exactKeys(item, ["allocation_id", "generation", "machine_id", "state"]);
  if (item.state !== "ready") {
    throw new VmHostProtocolError(
      "invalid_allocation_state",
      `allocations[${index}].state must be ready`,
    );
  }
  return {
    allocation_id: uuid(item.allocation_id, `allocations[${index}].allocation_id`),
    generation: positiveInteger(item.generation, `allocations[${index}].generation`),
    machine_id: identifier(item.machine_id, `allocations[${index}].machine_id`),
    state: "ready",
  };
}

function optionalNonce(value: unknown): { nonce?: string } {
  if (value === undefined) return {};
  if (typeof value !== "string" || value.length > 128) {
    throw new VmHostProtocolError("invalid_nonce", "ping nonce must be at most 128 characters");
  }
  return { nonce: value };
}

function uuid(value: unknown, name: string): string {
  if (typeof value !== "string" || !UUID_V4.test(value)) {
    throw new VmHostProtocolError("invalid_identity", `${name} must be a lowercase UUID v4`);
  }
  return value;
}

function identifier(value: unknown, name: string): string {
  if (typeof value !== "string" || !IDENTIFIER.test(value)) {
    throw new VmHostProtocolError(
      "invalid_identifier",
      `${name} must be 1-123 letters, numbers, dots, underscores, colons, or hyphens`,
    );
  }
  return value;
}

function factoryName(value: unknown): string {
  if (!isVmFactoryName(value)) {
    throw new VmHostProtocolError(
      "invalid_factory_name",
      "factory_name must be a non-reserved lowercase portable identifier of 1-63 characters",
    );
  }
  return value;
}

function positiveInteger(value: unknown, name: string): number {
  return boundedInteger(value, 1, Number.MAX_SAFE_INTEGER, name);
}

function boundedInteger(value: unknown, minimum: number, maximum: number, name: string): number {
  if (!Number.isSafeInteger(value) || Number(value) < minimum || Number(value) > maximum) {
    throw new VmHostProtocolError(
      "invalid_integer",
      `${name} must be an integer from ${minimum} through ${maximum}`,
    );
  }
  return Number(value);
}

function objectValue(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new VmHostProtocolError("invalid_message", `${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const fields = new Set(allowed);
  const unsupported = Object.keys(value).find((key) => !fields.has(key));
  if (unsupported !== undefined) {
    throw new VmHostProtocolError(
      "invalid_message",
      `VM-host message has unsupported field ${unsupported}`,
    );
  }
}

export class VmHostProtocolError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}
