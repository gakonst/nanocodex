import { claudeFiles } from "./claude-files";
import type { NamespaceExecutionRuntime } from "./namespace-tools";
import type { NamedTool, ToolContext, McpServers } from "nanocodex";
import { resolveNamespaceCwd, type Workspace } from "nanocodex-tools";
import type { Tool } from "../../nanocodex/runtime/claude.mjs";
import { claudeBashSchema } from "./claude-native-schemas";
import { createMcpRuntime } from "../../nanocodex/runtime/mcp-runtime.mjs";

const forbidden = new Set(["exec", "wait", "tool_search", "exec_command", "write_stdin", "apply_patch", "view_image", "update_plan", "web__run", "image_gen__imagegen", "spawn_agent", "send_agent_message", "list_agents", "wait_agent", "interrupt_agent", "close_agent", "submit_result"]);
const object = (properties: Record<string, unknown>, required: string[]) => ({ type: "object", properties, required, additionalProperties: false });
const string = { type: "string" };
const text = (content: string) => ({ content, isError: false });
function value(raw: unknown): Record<string, unknown> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new TypeError("expected tool object");
  return raw as Record<string, unknown>;
}
/** Explicit native Claude tools backed by existing managed capabilities. No Codex catalog. */
export async function createManagedClaudeTools(options: {
  filesystem: Workspace;
  namespace?: Pick<NamespaceExecutionRuntime, "workspaceTool">;
  nativeBash?: ReturnType<typeof import("./claude-bash").claudeBashJobs>;
  boardTools?: Tool[];
  prepareFilesystem?: () => Promise<void>;
  bash: NamedTool;
  poll?: NamedTool;
  allowedNames?: readonly string[];
  tools: readonly NamedTool[];
  mcp: McpServers;
  loadServers?: () => Promise<McpServers>;
  catalogProvider?: (serverName: string) => string | undefined;
  providers?: readonly { definitions(): readonly { name?: string; description?: string; parameters?: unknown }[]; resolve(name: string): { handler(input: unknown, context: ToolContext): unknown } | undefined }[];
  authorize(context: ToolContext): void;
}) {
  const tools: Tool[] = [
    { name: "Bash", description: "Execute a bounded shell command in durable /brain. Select an authorized Hand with a leading cd /HAND && command. Background execution requires a native Hand and returns a bash task_id for TaskOutput/TaskStop. Subsequent commands retain the selected Hand/cwd; explicit cd /brain resets selection. Inspect uncertain external effects before any retry.",
      inputSchema: claudeBashSchema,
      handler: (raw, context) => {
        const input = value(raw);
        if (typeof input.command !== "string") throw new Error("command required");
        for (const key of Object.keys(input)) if (!Object.hasOwn(claudeBashSchema.properties, key)) throw new Error(`unsupported Bash option: ${key}`);
        for (const key of ["run_in_background", "dangerouslyDisableSandbox"]) if (input[key] !== undefined && typeof input[key] !== "boolean") throw new Error(`${key} must be boolean`);
        if (input.description !== undefined && typeof input.description !== "string") throw new Error("description must be a string");
        if (input.dangerouslyDisableSandbox === true) throw new Error("managed Bash cannot override host authority or sandbox policy");
        if (!options.nativeBash && input.timeout !== undefined && (typeof input.timeout !== "number" || !Number.isFinite(input.timeout) || input.timeout < 1 || input.timeout > 300000)) throw new Error("managed Bash timeout must be between 1 and 300000 ms");
        const parsedCd = input.command.match(/^\s*cd\s+(?:'([^'\n]+)'|"([^"$`\n]+)"|(\/[^\s;&|<>$`]+))\s*&&\s*([\s\S]+)$/);
        const cd = parsedCd && (parsedCd[1] ?? parsedCd[2] ?? parsedCd[3]!).startsWith("/") ? parsedCd : null;
        const workdir = resolveNamespaceCwd("/brain", cd ? cd[1] ?? cd[2] ?? cd[3]! : "/brain");
        if (cd && (workdir === "/brain" || workdir.startsWith("/brain/"))) options.nativeBash?.reset(context);
        if (options.nativeBash && ((cd && workdir !== "/brain" && !workdir.startsWith("/brain/")) || (!cd && options.nativeBash.selected(context)))) return options.nativeBash.run({...input, command: cd ? cd[4] : input.command}, context, cd ? workdir : undefined);
        if (input.timeout !== undefined && (typeof input.timeout !== "number" || !Number.isFinite(input.timeout) || input.timeout < 1 || input.timeout > 300000)) throw new Error("Brain Bash timeout must be between 1 and 300000 ms");
        if (input.run_in_background === true && (workdir === "/brain" || workdir.startsWith("/brain/"))) throw new Error("durable Brain Bash does not retain background processes; select an authorized Hand");
        return options.bash.handler({ cmd: cd ? cd[4] : input.command, workdir, max_output_tokens: 10000,
          ...(input.run_in_background === true ? { yield_time_ms: 1 } : input.timeout === undefined ? {} : { yield_time_ms: input.timeout }) }, context);
      } },
    ...claudeFiles(options),
    ...(options.boardTools ?? []),
    ...(options.nativeBash?.tools ?? []),
  ];
  if (options.poll && !options.nativeBash) tools.push({ name: "BashOutput", description: "Read output or send ordinary input to a retained native Bash session. session_id is the exact receipt from Bash, bound to its original Hand; never send passwords or verification codes.",
    inputSchema: object({ session_id: { type: "integer", minimum: 1 }, chars: string, max_output_tokens: { type: "integer", minimum: 1, maximum: 10000 }, timeout: { type: "integer", minimum: 1, maximum: 300000 } }, ["session_id"]),
    handler: (raw, context) => { const input=value(raw); return options.poll!.handler({ session_id: input.session_id, chars: input.chars ?? "", max_output_tokens: input.max_output_tokens ?? 10000, ...(input.timeout === undefined ? {} : {yield_time_ms:input.timeout}) },context); }
  });
  // Shared account tools retain their actual permission-checked handlers; only
  // object-schema custom tools are accepted, never Responses builtins/Code Mode.
  for (const tool of options.tools) {
    if (tools.some(native => native.name === tool.name) || forbidden.has(tool.name) || tool.parameters?.type !== "object") continue;
    tools.push({ name: tool.name, description: tool.description, inputSchema: tool.parameters as Record<string, unknown>, handler: tool.handler });
  }
  // web__run is a credential-bearing Codex search service, not a Claude/public
  // search capability. Do not rename it or borrow OpenAI credentials here.
  // Alternate Claude capabilities are prepared even for GPT-only turns. Keep
  // their fixed schemas available without opening a second set of MCP clients.
  let mcp: ReturnType<typeof createMcpRuntime> | undefined;
  let closed = false;
  let closing: Promise<void> | undefined;
  const assertMcpOpen = (context: ToolContext) => {
    options.authorize(context);
    context.signal.throwIfAborted();
    if (closed) throw new Error("Claude MCP tools are closed");
  };
  const getMcp = async (context: ToolContext) => {
    assertMcpOpen(context);
    const runtime = await (mcp ??= createMcpRuntime(options.mcp, { loadServers: options.loadServers, catalogProvider: options.catalogProvider }));
    assertMcpOpen(context);
    return runtime;
  };
  const close = () => {
    closed = true;
    // Join only factory creation, not discovery: close aborts pending server
    // initialization. Retain the same promise so repeated shutdowns coalesce.
    return closing ??= (async () => {
      const runtime = await mcp?.catch(() => undefined);
      await runtime?.close();
    })();
  };
  if ((Object.keys(options.mcp).length || options.loadServers !== undefined) && (options.allowedNames === undefined
    || options.allowedNames.some(name => name === "MCPToolSearch" || name === "MCPExecute"))) {
    tools.push({ name: "MCPToolSearch", description: "Discover authorized MCP tools and their input schemas; use MCPExecute with an exact returned name.", inputSchema: object({ query: string, limit: { type: "integer", minimum: 1, maximum: 32 } }, ["query"]), handler: async (raw, context) => {
      const input = value(raw);
      const runtime = await getMcp(context);
      await runtime.settled();
      assertMcpOpen(context);
      return runtime.search(input);
    } });
    tools.push({ name: "MCPExecute", description: "Call an exact discovered MCP tool with its schema arguments. Availability and account authority are checked at execution.", inputSchema: object({ name: string, arguments: { type: "object", additionalProperties: true } }, ["name", "arguments"]), handler: async (raw, context) => {
      const input = value(raw);
      if (typeof input.name !== "string" || input.name === "tool_search") throw new Error("invalid MCP name");
      const runtime = await getMcp(context);
      let tool = runtime.resolve(input.name);
      if (!tool) {
        await runtime.settled();
        assertMcpOpen(context);
        tool = runtime.resolve(input.name);
      }
      assertMcpOpen(context);
      if (!tool) throw new Error("MCP tool unavailable");
      return tool.handler(input.arguments, context);
    } });
  }
  if (options.providers?.length) {
    // Snapshot schemas and resolvers together. Reject even normalized-name
    // collisions; selecting the first handler could dispatch a different
    // capability from the schema the model discovered.
    const capabilities = () => {
      const entries = new Map<string, { definition: ReturnType<NonNullable<typeof options.providers>[number]["definitions"]>[number]; provider: NonNullable<typeof options.providers>[number] }>();
      const normalized = new Set<string>();
      for (const provider of options.providers!) for (const definition of provider.definitions()) {
        if (typeof definition.name !== "string" || forbidden.has(definition.name)) continue;
        const key = definition.name.replace(/[^A-Za-z0-9_-]/g, "_");
        if (!key || normalized.has(key)) throw new Error("ambiguous tool capability name");
        normalized.add(key); entries.set(definition.name, { definition, provider });
      }
      return entries;
    };
    tools.push({ name: "ToolSearch", description: "Discover currently authorized connector and Hand tools. Returns native names and input schemas. Use ToolExecute with an exact returned name.", inputSchema: object({ query: string, limit: { type: "integer", minimum: 1, maximum: 32 } }, ["query"]), handler: raw => {
      const input = value(raw); if (typeof input.query !== "string") throw new Error("query required");
      const words = input.query.toLowerCase().split(/\s+/).filter(Boolean);
      const limit = Number(input.limit ?? 8); if (!Number.isSafeInteger(limit) || limit < 1 || limit > 32) throw new Error("invalid limit");
      return text(JSON.stringify(Array.from(capabilities().values(), entry => entry.definition).filter(definition =>
        typeof definition.name === "string" && !forbidden.has(definition.name)
        && words.some(word => (definition.name + " " + definition.description).toLowerCase().includes(word)))
        .slice(0, limit).map(definition => ({ name: definition.name, description: definition.description, input_schema: definition.parameters }))));
    } });
    tools.push({ name: "ToolExecute", description: "Call an exact discovered authorized connector or Hand tool using its input_schema arguments. Current capability and account authority are rechecked.", inputSchema: object({ name: string, arguments: { type: "object", additionalProperties: true } }, ["name", "arguments"]), handler: (raw, context) => {
      const input = value(raw); if (typeof input.name !== "string" || forbidden.has(input.name)) throw new Error("invalid tool name");
      const entry = capabilities().get(input.name);
      const tool = entry?.provider.resolve(input.name);
      if (!tool) throw new Error("tool unavailable");
      return tool.handler(input.arguments, context);
    } });
  }
  // Every tool is an explicit host capability and rechecks the current authority.
  return { tools: tools.filter(tool => options.allowedNames === undefined || options.allowedNames.includes(tool.name)).map(tool => ({ ...tool, handler: async (input: unknown, context: ToolContext) => {
    options.authorize(context);
    context.signal.throwIfAborted();
    if (["Read", "Write", "Edit"].includes(tool.name)) {
      await options.prepareFilesystem?.();
      options.authorize(context);
      context.signal.throwIfAborted();
    }
    return tool.handler(input, context);
  } })), close };
}
