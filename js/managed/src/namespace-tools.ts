import type { ToolMap } from "nanocodex";
import { CUA_JS_NAME, CUA_RESET_NAME } from "nanocodex-computer/contract";
import {
  createNamespaceManifest,
  createNamespaceScope,
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
  namespaceMountRoot,
  resolveNamespaceCwd,
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

export type RoutedTool = Readonly<{
  definition?: Readonly<{ description?: string; parameters?: Record<string, unknown>; [key: string]: unknown }>;
  handler(input: unknown, context: ToolContext): unknown | Promise<unknown>;
}>;

export type NamespaceMachine = Readonly<{
  id: string;
  root?: string;
  aliases?: readonly string[];
  workspace: string;
}>;

export type MachineToolResolver = (
  machineId: string,
  name: HostedMachineToolName,
  context: ToolContext,
) => RoutedTool | undefined;

export type ScreenToolResolver = (machineId: string, context: ToolContext) => RoutedTool | undefined;

type MountedHand = Readonly<{
  mountId: string;
  machineId?: string;
  root: string;
  workspace: string;
  exec?: RoutedTool;
  writeStdin?: RoutedTool;
  preview?: RoutedTool;
  cua?: RoutedTool;
  cuaReset?: RoutedTool;
  screen?: RoutedTool;
}>;

type CellBinding = Readonly<{
  scope: NamespaceScope;
  hands: ReadonlyMap<string, MountedHand>;
  aliases: ReadonlyMap<string, string>;
}>;

type ProcessBinding = Readonly<{
  ownerSessionId: string;
  providerSessionId: number;
  writeStdin: RoutedTool;
}>;

export type NamespaceCaptureFilter = (machine: NamespaceMachine) => boolean;

export type NamespaceExecutionRuntime = Readonly<{
  tools: ToolMap;
  capture(context: ToolContext, filter?: NamespaceCaptureFilter): void;
}>;

/**
 * Routes the canonical process tools by the root of their logical cwd. The
 * binding captured for a Code Mode call owns its exact attached tool handles,
 * so a disconnect or reconnect cannot retarget an admitted command.
 */
export function createNamespaceExecutionRuntime(
  machines: (context: ToolContext) => readonly NamespaceMachine[],
  resolveMachineTool: MachineToolResolver = () => undefined,
  brainExec?: RoutedTool,
  resolveScreenTool: ScreenToolResolver = () => undefined,
): NamespaceExecutionRuntime {
  const brain = Object.freeze({
    mountId: "mount:brain",
    root: "/brain",
    workspace: "/brain",
    exec: brainExec,
  }) satisfies MountedHand;
  const cells = new Map<string, CellBinding>();
  const sessions = new Map<number, ProcessBinding>();
  const computerQueues = new Map<string, Promise<unknown>>();

  const cell = (context: ToolContext, filter?: NamespaceCaptureFilter): CellBinding => {
    // Direct tools have an empty parentCallId. Pin those to their own call,
    // while nested Code Mode tools keep sharing their parent's captured lease.
    const key = `${context.sessionId}\u0000${context.parentCallId || context.callId}`;
    const retained = cells.get(key);
    if (retained !== undefined) return retained;
    const created = createCellBinding(brain, machines(context).filter(filter ?? (() => true)), resolveMachineTool, context, key, resolveScreenTool);
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

  const computerParameters = {
    type: "object",
    properties: {
      workdir: { type: "string", description: "Hand workdir from environment or mount. Its root routes this CUA call, like exec_command; it is not forwarded to the provider." },
    },
    required: ["workdir"],
    additionalProperties: true,
  };
  const computerCall = async (name: string, input: unknown, context: ToolContext): Promise<unknown> => {
    const value = record(input);
    const workdir = optionalString(value.workdir, "workdir");
    if (!workdir) throw new Error('CUA requires an explicit Hand workdir, like exec_command. First call mcp__cua_repl__js({workdir: "/<hand>"}) to read that provider’s contract, then add its arguments to each call.');
    const binding = cell(context);
    const route = routeNamespaceCwd(binding.scope, canonicalCwd(binding, workdir), "namespace.discover");
    const hand = binding.hands.get(route.mount.mountId);
    if (!hand?.cua || !hand.cuaReset) {
      throw new Error(`namespace mount ${route.mount.root} has no CUA runtime; screen-only Hands are unsupported by cua_repl. Use environment to find a Hand with an attached CUA provider.`);
    }
    const providerInput = without(value, "workdir");
    // JS with only a workdir discovers the actual provider API without executing
    // anything. Reset with only a workdir still invokes the provider's empty reset.
    if (name === CUA_JS_NAME && Object.keys(providerInput).length === 0) {
      const definitions = [hand.cua, hand.cuaReset].map((tool, index) => {
        const toolName = index === 0 ? CUA_JS_NAME : CUA_RESET_NAME;
        const definition = tool.definition;
        if (!definition || typeof definition.description !== "string"
          || !definition.parameters || typeof definition.parameters !== "object") {
          throw new Error(`Hand ${hand.root} has no discovered ${toolName} contract; reconnect its CUA provider`);
        }
        return { ...definition, name: toolName };
      });
      return { workdir: hand.root, machine_id: hand.machineId,
        tools: [CUA_JS_NAME, CUA_RESET_NAME], definitions,
        browser_selection: "For providers exposing cua.createBrowserTab, browser display names are not necessarily accepted identifiers. OpenAI's provider accepts lowercase family aliases (for example 'brave', not 'Brave Browser') or exact discovered browser IDs. Reuse an ID from current provider state; when browser/profile selection is ambiguous, inspect the provider's browser inventory first and match the requested instance. Do not guess IDs or silently retry a browser action with a different target.",
        native_app_recovery: "For native macOS providers exposing cua.getApp, app selection may launch only in the background. If its initial observation stalls, follow any required js_reset, then use supported CUA and an observed app launcher (for example its item in Finder) to open the intended app normally before selecting it again. After a transient menu or window closes, cgWindowNotFound can mean the bound window is gone; select the same app again and inspect fresh state. Do not replay input actions, modify permissions, or switch automation backends to recover.",
        routing: "Add the Hand workdir to each provider call. Nanocodex consumes workdir for routing and forwards all other arguments unchanged. Use Promise.all for different Hands; JS and reset on the same Hand are ordered." };
    }
    const tool = name === CUA_JS_NAME ? hand.cua : hand.cuaReset;
    // The router allows concurrency across Hands. Each session/Hand has one
    // queue shared by JS and reset, independent of every other Hand's queue.
    const key = `${context.sessionId}\u0000${hand.mountId}`;
    const previous = computerQueues.get(key) ?? Promise.resolve();
    const pending = previous.catch(() => {}).then(() => {
      context.signal.throwIfAborted();
      return tool.handler(providerInput, context);
    });
    computerQueues.set(key, pending);
    const cleanup = () => { if (computerQueues.get(key) === pending) computerQueues.delete(key); };
    // Cancellation releases the caller promptly, but the queue entry remains
    // until its predecessor settles so later calls cannot overtake active work.
    void pending.then(cleanup, cleanup);
    return await new Promise((resolve, reject) => {
      const abort = () => reject(context.signal.reason ?? new Error("CUA call cancelled"));
      if (context.signal.aborted) { abort(); return; }
      context.signal.addEventListener("abort", abort, { once: true });
      void pending.then(resolve, reject).finally(() => context.signal.removeEventListener("abort", abort));
    });
  };

  const tools: ToolMap = {
    [CUA_JS_NAME]: {
      description: "Use a Hand's CUA MCP provider. Set workdir on every call, just like exec_command. First call with only {workdir} to read that provider's descriptions and schemas without executing code; then add its exact arguments alongside workdir. Nanocodex strips only workdir before forwarding. Use Promise.all for different workdirs in Code Mode; JS and reset calls to the same Hand are ordered. Each cell pins its Hand connections; there is no global computer selection. /brain and screen-only Hands have no CUA provider.",
      parameters: computerParameters,
      supportsParallelToolCalls: true,
      handler: (input, context) => computerCall(CUA_JS_NAME, input, context),
      releaseSession, dispose,
    },
    [CUA_RESET_NAME]: {
      description: "Reset the CUA provider on the Hand selected by this call's workdir. Read its reset contract using mcp__cua_repl__js({workdir}) first. Pass provider reset arguments alongside workdir; only workdir is consumed by Nanocodex. A workdir-only reset forwards {}. JS and reset calls to the same Hand are ordered; other Hands run independently.",
      parameters: computerParameters,
      supportsParallelToolCalls: true,
      handler: (input, context) => computerCall(CUA_RESET_NAME, input, context),
      releaseSession, dispose,
    },
    exec_command: {
      description: "Run a command in durable /brain using bounded Just Bash by default. Use an explicit hand workdir returned by mount or environment only for native binaries, builds, or process sessions. A hand mount already represents its advertised workspace: if /laptop maps to /Users/me/repo, use /laptop for that workspace or /laptop/src for its src directory, never /laptop/Users/me/repo. No execution hand is attached by default.",
      parameters: EXEC_COMMAND_PARAMETERS,
      outputSchema: EXECUTION_OUTPUT_SCHEMA,
      supportsParallelToolCalls: true,
      handler: async (input, context) => {
        const value = record(input);
        const workdir = optionalString(value.workdir, "workdir");
        if (brainExec !== undefined && isBrainExecution(value)) {
          // Brain calls must remain independent of hand discovery/readiness,
          // and do not need to retain a per-cell native mount lease.
          return brainExec.handler({
            ...without(value, "workdir"),
            workdir: resolveNamespaceCwd(DEFAULT_CWD, workdir),
          }, context);
        }
        const binding = cell(context);
        const route = routeNamespaceCwd(binding.scope, canonicalCwd(binding, workdir));
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
          // A transport/tool error has no execution result. It does not prove that
          // the process exited; keep the original Hand binding so polling can retry.
          if (structured !== undefined && (typeof structured.exit_code === "number" || structured.exit_code === null)) {
            sessions.delete(publicSessionId);
          }
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
          canonicalCwd(binding, optionalString(value.workdir, "workdir")),
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
    capture: (context: ToolContext, filter?: NamespaceCaptureFilter) => { void cell(context, filter); },
  });
}

export function createNamespaceExecutionTools(
  machines: (context: ToolContext) => readonly NamespaceMachine[],
  resolveMachineTool: MachineToolResolver = () => undefined,
): ToolMap {
  return createNamespaceExecutionRuntime(machines, resolveMachineTool).tools;
}

export const machineMountRoot = namespaceMountRoot;

export function isBrainExecution(input: unknown): boolean {
  const value = record(input);
  const cwd = resolveNamespaceCwd(DEFAULT_CWD, optionalString(value.workdir, "workdir"));
  return cwd === DEFAULT_CWD || cwd.startsWith(`${DEFAULT_CWD}/`);
}

function createCellBinding(
  brain: MountedHand,
  sourceMachines: readonly NamespaceMachine[],
  resolveMachineTool: MachineToolResolver,
  context: ToolContext,
  key: string,
  resolveScreenTool: ScreenToolResolver,
): CellBinding {
  const hands: MountedHand[] = [brain];
  const roots = new Set([brain.root]);
  const aliases = new Map<string, string>();
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
      exec: resolveMachineTool(machine.id, "exec_command", context),
      writeStdin: resolveMachineTool(machine.id, "write_stdin", context),
      preview: resolveMachineTool(machine.id, "preview", context),
      cua: resolveMachineTool(machine.id, CUA_JS_NAME, context),
      cuaReset: resolveMachineTool(machine.id, CUA_RESET_NAME, context),
      screen: resolveScreenTool(machine.id, context),
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
  for (const machine of sourceMachines) {
    const root = machine.root ?? machineMountRoot(machine.id);
    for (const alias of machine.aliases ?? []) {
      if (alias === root) continue;
      if (roots.has(alias) || aliases.has(alias) || !/^\/[a-z0-9][a-z0-9._-]*$/.test(alias))
        throw new Error(`ambiguous or invalid namespace alias ${alias}`);
      aliases.set(alias, root);
    }
  }
  const scope = createNamespaceScope(manifest, DEFAULT_CWD);
  return Object.freeze({
    scope,
    hands: new Map(hands.map((hand) => [hand.mountId, hand])),
    aliases,
  });
}

function canonicalCwd(binding: CellBinding, workdir?: string): string {
  const cwd = resolveNamespaceCwd(DEFAULT_CWD, workdir);
  const root = `/${cwd.split("/")[1] ?? ""}`;
  const canonical = binding.aliases.get(root);
  return canonical === undefined ? cwd : canonical + cwd.slice(root.length);
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
    // Direct model calls read output; Code Mode reads structuredResult.
    // Both must expose the namespace session, never the Hand's local ID.
    output: JSON.stringify(structured),
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

/** Admit each retained VM independently, then capture all verified routes once.
 * A rejected probe or a negative receipt excludes that VM, including cached routes.
 */
export async function prepareNamespaceHostMounts<T extends Readonly<{ id: string }>>(
  mounts: readonly T[],
  probe: (mount: T) => Promise<NamespaceCaptureFilter | undefined>,
): Promise<NamespaceCaptureFilter> {
  const results = await Promise.allSettled(mounts.map(probe));
  const checks = new Map(mounts.map((mount, index) => {
    const result = results[index]!;
    return [mount.id, result.status === "fulfilled" ? result.value : undefined] as const;
  }));
  return machine => !checks.has(machine.id) || checks.get(machine.id)?.(machine) === true;
}
