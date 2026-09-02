import { utf8ByteLength } from "../runtime/utf8.mjs";

const MAX_MACHINES = 32;
const MAX_MACHINE_CAPABILITIES = 64;
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const CAPABILITY = /^[a-z0-9][a-z0-9._:-]{0,63}$/;
const normalizedSnapshots = new WeakSet();

/** Normalizes the non-secret machines represented by one attached tool host. */
export function normalizeHostedMachines(machines = []) {
  if (normalizedSnapshots.has(machines)) return machines;
  if (!Array.isArray(machines) || machines.length > MAX_MACHINES) {
    throw new RangeError(`tool attachments describe at most ${MAX_MACHINES} machines`);
  }
  const ids = new Set();
  const normalized = Object.freeze(machines.map((machine, index) => {
    if (!machine || typeof machine !== "object" || Array.isArray(machine)) {
      throw new TypeError(`machines[${index}] must be an object`);
    }
    const allowed = new Set(["id", "name", "workspace", "capabilities"]);
    for (const key of Object.keys(machine)) {
      if (!allowed.has(key)) throw new TypeError(`machines[${index}] contains unsupported field ${key}`);
    }
    if (typeof machine.id !== "string" || !IDENTIFIER.test(machine.id)) {
      throw new TypeError(`machines[${index}].id must be a safe identifier`);
    }
    if (ids.has(machine.id)) throw new TypeError(`duplicate machine id: ${machine.id}`);
    ids.add(machine.id);
    if (typeof machine.name !== "string" || !machine.name.trim() || utf8ByteLength(machine.name) > 128) {
      throw new TypeError(`machines[${index}].name must be 1-128 UTF-8 bytes`);
    }
    if (typeof machine.workspace !== "string" || !machine.workspace.trim()
      || utf8ByteLength(machine.workspace) > 1024 || machine.workspace.includes("\0")) {
      throw new TypeError(`machines[${index}].workspace must be 1-1024 UTF-8 bytes`);
    }
    if (!Array.isArray(machine.capabilities)
      || machine.capabilities.length > MAX_MACHINE_CAPABILITIES
      || machine.capabilities.some((capability) => (
        typeof capability !== "string" || !CAPABILITY.test(capability)
      ))) {
      throw new TypeError(`machines[${index}].capabilities must be safe identifiers`);
    }
    const capabilities = [...new Set(machine.capabilities)];
    if (capabilities.length !== machine.capabilities.length) {
      throw new TypeError(`machines[${index}].capabilities must be unique`);
    }
    return Object.freeze({
      id: machine.id,
      name: machine.name.trim(),
      workspace: machine.workspace,
      capabilities: Object.freeze(capabilities),
    });
  }));
  normalizedSnapshots.add(normalized);
  return normalized;
}
