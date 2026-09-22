import type { NamedTool, ToolContext } from "nanocodex";
import {
  CF_SANDBOX_PROVIDER,
  isVmFactoryName,
} from "./vm-factory-name";

const MOUNT_NAME = /^[a-z0-9](?:[a-z0-9._-]{0,61}[a-z0-9])?$/;

export const MANAGED_CLOUDFLARE_PROVIDER = CF_SANDBOX_PROVIDER;

export type ManagedMountProvider = string;

export type ManagedMountRequest = Readonly<{
  provider: ManagedMountProvider;
  name: string;
}>;

export type ManagedMountResult = Readonly<{
  id: string;
  name: string;
  provider: string;
  mount: string;
  status: "mounted";
  created: boolean;
}>;

export const MANAGED_MOUNT_PARAMETERS = Object.freeze({
  type: "object",
  properties: {
    provider: {
      type: "string",
      pattern: MOUNT_NAME.source,
      description: "Execution provider to mount: cf_sandbox or the exact vm_provider advertised by the requested computer in environment().hands.",
    },
    name: {
      type: "string",
      pattern: MOUNT_NAME.source,
      description: "Stable lowercase purpose name within this agent, such as demo or build. The returned path and display name include the host/provider automatically.",
    },
  },
  required: ["provider", "name"],
  additionalProperties: false,
} as const);

export const MANAGED_MOUNT_OUTPUT_SCHEMA = Object.freeze({
  type: "object",
  properties: {
    id: { type: "string" },
    name: { type: "string" },
    provider: { type: "string" },
    mount: { type: "string" },
    status: { type: "string", enum: ["mounted"] },
    created: { type: "boolean" },
  },
  required: ["id", "name", "provider", "mount", "status", "created"],
  additionalProperties: false,
} as const);

export function managedMountTool(
  mount: (request: ManagedMountRequest, context: ToolContext) => Promise<ManagedMountResult>,
): NamedTool {
  return {
    name: "mount",
    description: [
      "Provision and attach a sandbox provider as an execution hand when the task needs native tools.",
      "The agent begins without a sandbox; infer when one is needed instead of asking the user to request it.",
      "The operation is idempotent by name and returns a logical mount path to use as exec_command.workdir in a later Code Mode cell.",
      "After creating a mount in Code Mode, execute commands on it in a new code cell. The creating cell retains its original namespace snapshot.",
      "For a VM on a named computer, find that online computer in environment().hands and use its vm_provider. Native work on the computer itself uses its existing mount without provisioning a VM. Do not ask the user to supply an internal provider name. A configured provider may still be starting; allocation checks current readiness and capacity.",
    ].join(" "),
    parameters: MANAGED_MOUNT_PARAMETERS,
    outputSchema: MANAGED_MOUNT_OUTPUT_SCHEMA,
    handler: (input, context) => mount(parseManagedMountRequest(input), context),
  };
}

export function parseManagedMountRequest(input: unknown): ManagedMountRequest {
  if (!input || typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError("mount input must be an object");
  }
  const value = input as Record<string, unknown>;
  const unsupported = Object.keys(value).find((key) => key !== "provider" && key !== "name");
  if (unsupported !== undefined) throw new TypeError(`mount input contains unsupported field ${unsupported}`);
  const provider = managedMountProvider(value.provider);
  const name = portableName(value.name, "mount name");
  return Object.freeze({ provider, name });
}

export function managedMountRoot(name: string, id: string, provider?: string, reserved: readonly string[] = []): string {
  const parsedName = portableName(name, "mount name");
  if (!/^[a-f0-9-]{36}$/.test(id)) throw new TypeError("mount id must be a lowercase UUID");
  const suffix = id.replaceAll("-", "").slice(-8);
  if (provider !== undefined) {
    const owner = managedMountProvider(provider);
    const label = `${owner === MANAGED_CLOUDFLARE_PROVIDER ? "cloudflare" : `vm-${owner}`}-${parsedName}`;
    const stem = label.length <= 63 ? `/${label}` : `/${label.slice(0, 54).replace(/[._-]+$/, "")}-${suffix}`;
    const used = new Set(reserved);
    let root = stem;
    for (let n = 2; used.has(root); n += 1) {
      const tail = `-${n}`;
      root = `${stem.slice(0, 64 - tail.length).replace(/[._-]+$/, "")}${tail}`;
    }
    return root;
  }
  // Older persisted mounts and callers retain their UUID-derived spelling.
  const stem = parsedName.slice(0, 49).replace(/[._-]+$/, "") || "hand";
  return `/mnt-${stem}-${suffix}`;
}

function managedMountProvider(value: unknown): ManagedMountProvider {
  const provider = portableName(value, "mount provider");
  if (provider === "cloudflare") return MANAGED_CLOUDFLARE_PROVIDER;
  if (!isVmFactoryName(provider) && provider !== MANAGED_CLOUDFLARE_PROVIDER) {
    throw new TypeError(
      "mount provider must be cf_sandbox or an exact non-reserved VM factory name",
    );
  }
  return provider;
}

function portableName(value: unknown, name: string): string {
  if (typeof value !== "string" || !MOUNT_NAME.test(value)) {
    throw new TypeError(
      `${name} must be a lowercase portable identifier of 1-63 characters`,
    );
  }
  return value;
}

export function managedMountProviderResourceId(
  sessionId: string,
  mountId: string,
  providerCount: number,
): string {
  if (!/^[a-f0-9-]{36}$/.test(sessionId)) throw new TypeError("session id must be a lowercase UUID");
  if (!/^[a-f0-9-]{36}$/.test(mountId)) throw new TypeError("mount id must be a lowercase UUID");
  if (!Number.isSafeInteger(providerCount) || providerCount < 0) {
    throw new TypeError("provider count must be a non-negative safe integer");
  }
  // Keep the original hand's session-derived identity so upgraded agents
  // reattach its retained workspace. Additional hands use their mount UUID;
  // combining both UUIDs exceeds the provider's 63-character sandbox ID cap.
  return providerCount === 0 ? sessionId : mountId;
}
