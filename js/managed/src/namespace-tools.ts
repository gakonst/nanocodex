import type { ToolMap } from "nanocodex";
import {
  createNamespaceManifest,
  createNamespaceScope,
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
  namespaceMountRoot,
  PREVIEW_OUTPUT_SCHEMA,
  routeNamespaceCwd,
  WRITE_STDIN_PARAMETERS,
  type HostedMachineToolName,
  type NamespaceRight,
  type NamespaceScope,
  type ToolContext,
} from "nanocodex-tools";

const TOOL_RESULT = Symbol.for("nanocodex.toolResult");
const DEFAULT_CWD = "/brain";

type RoutedTool = Readonly<{
  handler(input: unknown, context: ToolContext): unknown | Promise<unknown>;
}>;

export type NamespaceMachine = Readonly<{
  id: string;
  root?: string;
  workspace: string;
}>;

export type MachineToolResolver = (
  machineId: string,
  name: HostedMachineToolName,
) => RoutedTool | undefined;

type MountedHand = Readonly<{
  mountId: string;
  machineId?: string;
  root: string;
  workspace: string;
  exec?: RoutedTool;
  writeStdin?: RoutedTool;
  preview?: RoutedTool;
}>;

type CellBinding = Readonly<{
  scope: NamespaceScope;
  hands: ReadonlyMap<string, MountedHand>;
}>;

type ProcessBinding = Readonly<{
  ownerSessionId: string;
  providerSessionId: number;
  writeStdin: RoutedTool;
}>;

export type NamespaceExecutionRuntime = Readonly<{
  tools: ToolMap;
  capture(context: ToolContext): void;
}>;

/**
 * Routes the canonical process tools by the root of their logical cwd. The
 * binding captured for a Code Mode call owns its exact attached tool handles,
 * so a disconnect or reconnect cannot retarget an admitted command.
 */
export function createNamespaceExecutionRuntime(
  machines: () => readonly NamespaceMachine[],
  resolveMachineTool: MachineToolResolver = () => undefined,
): NamespaceExecutionRuntime {
  const brain = Object.freeze({
    mountId: "mount:brain",
    root: "/brain",
    workspace: "/workspace",
  }) satisfies MountedHand;
  const cells = new Map<string, CellBinding>();
  const sessions = new Map<number, ProcessBinding>();

  const cell = (context: ToolContext): CellBinding => {
    const key = `${context.sessionId}\u0000${context.parentCallId}`;
    const retained = cells.get(key);
    if (retained !== undefined) return retained;
    const created = createCellBinding(brain, machines(), resolveMachineTool, key);
    cells.set(key, created);
    return created;
  };

  const releaseSession = (ownerSessionId: string): void => {
    const cellPrefix = `${ownerSessionId}\u0000`;
    for (const key of cells.keys()) {
      if (key.startsWith(cellPrefix)) cells.delete(key);
    }
    for (const [sessionId, binding] of sessions) {
      if (binding.ownerSessionId === ownerSessionId) sessions.delete(sessionId);
    }
  };
  const dispose = (): void => {
    cells.clear();
    sessions.clear();
  };

  const tools: ToolMap = {
    exec_command: {
      description: "Run a command on the hand that owns the root of workdir. workdir is a logical namespace path returned by mount or listed by accountInfo, such as /repo-test/repo or /laptop/repo. No execution hand is attached by default.",
      parameters: EXEC_COMMAND_PARAMETERS,
      outputSchema: EXECUTION_OUTPUT_SCHEMA,
      supportsParallelToolCalls: true,
      handler: async (input, context) => {
        const value = record(input);
        const workdir = optionalString(value.workdir, "workdir");
        if (workdir === undefined) {
          throw new Error(
            "exec_command.workdir must select an attached hand; call mount when native execution is needed",
          );
        }
        const binding = cell(context);
        const route = routeNamespaceCwd(binding.scope, workdir);
        const hand = binding.hands.get(route.mount.mountId);
        if (hand?.exec === undefined) {
          throw new Error(`namespace mount ${route.mount.root} is not executable`);
        }
        const result = await hand.exec.handler({
          ...without(value, "workdir"),
          workdir: nativeWorkdir(hand.workspace, route.relativePath),
        }, context);
        const structured = executionResult(result);
        if (structured?.session_id === undefined) return result;
        if (hand.writeStdin === undefined) {
          throw new Error(`namespace mount ${route.mount.root} cannot retain process sessions`);
        }
        const providerSessionId = positiveSessionId(structured.session_id);
        const publicSessionId = reserveSessionId(sessions);
        sessions.set(publicSessionId, Object.freeze({
          ownerSessionId: context.sessionId,
          providerSessionId,
          writeStdin: hand.writeStdin,
        }));
        return replaceExecutionResult(result, { ...structured, session_id: publicSessionId });
      },
      releaseSession,
      dispose,
    },
    write_stdin: {
      description: "Write characters to or poll a session returned by exec_command. The session remains pinned to its original hand and namespace binding.",
      parameters: WRITE_STDIN_PARAMETERS,
      outputSchema: EXECUTION_OUTPUT_SCHEMA,
      handler: async (input, context) => {
        const value = record(input);
        const publicSessionId = positiveSessionId(value.session_id);
        const binding = sessions.get(publicSessionId);
        if (binding === undefined || binding.ownerSessionId !== context.sessionId) {
          throw new Error("unknown or stale namespace process session");
        }
        const result = await binding.writeStdin.handler({
          ...without(value, "session_id"),
          session_id: binding.providerSessionId,
        }, context);
        const structured = executionResult(result);
        if (structured?.session_id === undefined) {
          sessions.delete(publicSessionId);
          return result;
        }
        if (positiveSessionId(structured.session_id) !== binding.providerSessionId) {
          sessions.delete(publicSessionId);
          throw new Error("execution hand changed its bound process session");
        }
        return replaceExecutionResult(result, { ...structured, session_id: publicSessionId });
      },
      releaseSession,
      dispose,
    },
    preview: {
      description: "Expose an HTTP server from the hand that owns the logical workdir when that hand supports previews.",
      parameters: {
        type: "object",
        properties: {
          workdir: { type: "string", description: "Logical namespace path selecting the server's hand." },
          port: { type: "integer", minimum: 1024, maximum: 65_535 },
        },
        required: ["port"],
        additionalProperties: false,
      },
      outputSchema: PREVIEW_OUTPUT_SCHEMA,
      handler: async (input, context) => {
        const value = record(input);
        const binding = cell(context);
        const route = routeNamespaceCwd(
          binding.scope,
          optionalString(value.workdir, "workdir"),
          "network.preview",
        );
        const hand = binding.hands.get(route.mount.mountId);
        if (hand?.preview === undefined) {
          throw new Error(`namespace mount ${route.mount.root} does not provide preview`);
        }
        return hand.preview.handler(without(value, "workdir"), context);
      },
      releaseSession,
      dispose,
    },
  };
  return Object.freeze({
    tools,
    capture: (context: ToolContext) => { void cell(context); },
  });
}

export function createNamespaceExecutionTools(
  machines: () => readonly NamespaceMachine[],
  resolveMachineTool: MachineToolResolver = () => undefined,
): ToolMap {
  return createNamespaceExecutionRuntime(machines, resolveMachineTool).tools;
}

export const machineMountRoot = namespaceMountRoot;

function createCellBinding(
  brain: MountedHand,
  sourceMachines: readonly NamespaceMachine[],
  resolveMachineTool: MachineToolResolver,
  key: string,
): CellBinding {
  const hands: MountedHand[] = [brain];
  const roots = new Set([brain.root]);
  const keyHash = stableHash(key);
  for (const machine of sourceMachines) {
    const root = machine.root ?? machineMountRoot(machine.id);
    if (roots.has(root)) throw new Error(`duplicate namespace mount root ${root}`);
    roots.add(root);
    hands.push(Object.freeze({
      mountId: `mount:user:${machine.id}`,
      machineId: machine.id,
      root,
      workspace: machine.workspace,
      exec: resolveMachineTool(machine.id, "exec_command"),
      writeStdin: resolveMachineTool(machine.id, "write_stdin"),
      preview: resolveMachineTool(machine.id, "preview"),
    }));
  }
  const manifest = createNamespaceManifest({
    manifestId: `manifest:${keyHash}:${stableHash(hands.map(({ mountId }) => mountId).join("\u0000"))}`,
    mounts: hands.map((hand) => ({
      root: hand.root,
      mountId: hand.mountId,
      handId: hand.machineId === undefined ? "hand:brain" : `hand:${hand.machineId}`,
      exportId: hand.machineId === undefined ? "export:brain-workspace" : `export:${hand.machineId}`,
      generation: `cell:${keyHash}:${stableHash(hand.mountId)}`,
      rights: handRights(hand),
    })),
  });
  const scope = createNamespaceScope(manifest, DEFAULT_CWD);
  return Object.freeze({
    scope,
    hands: new Map(hands.map((hand) => [hand.mountId, hand])),
  });
}

function handRights(hand: MountedHand): readonly NamespaceRight[] {
  const rights: NamespaceRight[] = ["namespace.discover"];
  if (hand.exec !== undefined) rights.push("process.exec");
  if (hand.writeStdin !== undefined) rights.push("process.stdin");
  if (hand.preview !== undefined) rights.push("network.preview");
  return rights;
}

function nativeWorkdir(workspace: string, relativePath: string): string {
  if (typeof workspace !== "string" || workspace.length === 0 || workspace.includes("\0")) {
    throw new Error("execution hand published an invalid workspace root");
  }
  if (relativePath === "/") return workspace;
  return `${workspace.replace(/\/$/, "")}/${relativePath.slice(1)}`;
}

function executionResult(value: unknown): Record<string, unknown> | undefined {
  const result = isToolResult(value) ? value.structuredResult : value;
  return result !== null && typeof result === "object" && !Array.isArray(result)
    ? result as Record<string, unknown>
    : undefined;
}

function replaceExecutionResult(original: unknown, structured: Record<string, unknown>): unknown {
  if (!isToolResult(original)) return structured;
  return Object.freeze({
    [TOOL_RESULT]: true,
    metadata: original.metadata,
    output: original.output,
    structuredResult: structured,
    success: original.success,
    value: structured,
  });
}

function isToolResult(value: unknown): value is Readonly<{
  metadata: unknown;
  output: unknown;
  structuredResult: unknown;
  success: boolean;
}> {
  return Boolean((value as Record<PropertyKey, unknown> | null)?.[TOOL_RESULT]);
}

function reserveSessionId(sessions: ReadonlyMap<number, unknown>): number {
  for (let attempt = 0; attempt < 64; attempt += 1) {
    const bytes = crypto.getRandomValues(new Uint32Array(1));
    const candidate = (bytes[0]! & 0x7fff_ffff) || 1;
    if (!sessions.has(candidate)) return candidate;
  }
  throw new Error("could not allocate a namespace process session");
}

function positiveSessionId(value: unknown): number {
  if (!Number.isSafeInteger(value) || Number(value) <= 0) {
    throw new TypeError("session_id must be a positive safe integer");
  }
  return Number(value);
}

function optionalString(value: unknown, name: string): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
  return value;
}

function without(value: Record<string, unknown>, key: string): Record<string, unknown> {
  const { [key]: _, ...rest } = value;
  return rest;
}

function record(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("tool input must be an object");
  }
  return value as Record<string, unknown>;
}

function stableHash(value: string): string {
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(value)) {
    hash ^= BigInt(byte);
    hash = BigInt.asUintN(64, hash * 0x100000001b3n);
  }
  return hash.toString(16).padStart(16, "0");
}
