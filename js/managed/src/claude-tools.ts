import type { NamedTool, ToolContext, McpServers } from "nanocodex";
import type { Workspace } from "nanocodex-tools";
import type { Tool } from "../../nanocodex/runtime/claude.mjs";
import { createMcpRuntime } from "../../nanocodex/runtime/mcp-runtime.mjs";

// Codex interfaces are replaced by Claude's native tools. Shared subagents are
// installed separately by the task-tree runtime, preserving its owned handlers.
const forbidden = new Set([
  "exec", "wait", "tool_search", "exec_command", "write_stdin", "apply_patch",
  "view_image", "update_plan", "web__run", "image_gen__imagegen",
  "spawn_agent", "send_agent_message", "list_agents", "wait_agent",
  "interrupt_agent", "close_agent", "submit_result",
]);
const object = (properties: Record<string, unknown>, required: string[]) => ({ type: "object", properties, required, additionalProperties: false });
const string = { type: "string" };
const text = (content: string) => ({ content, isError: false });
function value(raw: unknown): Record<string, unknown> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new TypeError("expected tool object");
  return raw as Record<string, unknown>;
}
function path(input: Record<string, unknown>): string {
  if (typeof input.file_path !== "string" || !input.file_path.startsWith("/brain/") || input.file_path.split("/").some(p => p === ".." || p === ".") || /[\u0000-\u001f]/.test(input.file_path)) throw new Error("file_path must be a canonical /brain path");
  return input.file_path;
}
/** Explicit native Claude tools backed by existing managed capabilities. No Codex catalog. */
export async function createManagedClaudeTools(options: {
  filesystem: Workspace;
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
  const lock: { pending: Promise<void> } = { pending: Promise.resolve() };
  const fileMutation = async (operation: () => Promise<unknown>) => {
    const prior = lock.pending;
    let release!: () => void;
    lock.pending = new Promise<void>(resolve => { release = resolve; });
    await prior;
    try { return await operation(); } finally { release(); }
  };
  const tools: Tool[] = [
    // Parallel safety mirrors the Codex catalog: Bash inherits exec_command's
    // declaration (Hand commands are independent processes); file mutations,
    // generic executors and stateful tools keep the serial, ordered default.
    { name: "Bash", ...(options.bash.supportsParallelToolCalls === true ? { supportsParallelToolCalls: true } : {}), description: "Execute a bounded shell command. /brain uses durable Just Bash; workdir selects an attached Hand for native commands. Check uncertain external effects before any retry.",
      inputSchema: object({ command: string, workdir: string, timeout: { type: "integer", minimum: 1, maximum: 300000 }, max_output_tokens: { type: "integer", minimum: 1, maximum: 10000 } }, ["command"]),
      handler: (raw, context) => { const input = value(raw); if (typeof input.command !== "string") throw new Error("command required"); return options.bash.handler({ cmd: input.command, workdir: input.workdir ?? "/brain", max_output_tokens: input.max_output_tokens ?? 10000, ...(input.timeout === undefined ? {} : { yield_time_ms: input.timeout }) }, context); } },
    { name: "Read", supportsParallelToolCalls: true, description: "Read a UTF-8 text file under /brain with bounded line offsets and limits.", inputSchema: object({ file_path: string, offset: { type: "integer", minimum: 1 }, limit: { type: "integer", minimum: 1, maximum: 2000 } }, ["file_path"]),
      handler: async raw => { const input = value(raw); const bytes = await options.filesystem.readFile(path(input)); if (bytes.byteLength > 2_000_000) throw new Error("Read file exceeds 2 MB"); const lines = new TextDecoder("utf-8").decode(bytes).split("\n"); const offset = Number(input.offset ?? 1); const limit = Number(input.limit ?? 2000); if (!Number.isSafeInteger(offset) || offset < 1 || !Number.isSafeInteger(limit) || limit < 1 || limit > 2000) throw new Error("invalid read range"); return text(lines.slice(offset - 1, offset - 1 + limit).map((line, index) => `${offset + index}\t${line}`).join("\n").slice(0, 64000)); } },
    { name: "Write", description: "Write UTF-8 text to a canonical /brain file (maximum 2 MB), creating its parent directories.", inputSchema: object({ file_path: string, content: string }, ["file_path", "content"]),
      handler: raw => fileMutation(async () => { const input = value(raw); const filename = path(input); if (typeof input.content !== "string" || new TextEncoder().encode(input.content).byteLength > 2_000_000) throw new Error("invalid file content"); await options.filesystem.mkdir(filename.slice(0, filename.lastIndexOf("/"))); await options.filesystem.writeFile(filename, input.content); return text(`Wrote ${filename}`); }) },
    { name: "Edit", description: "Replace an exact string in a /brain UTF-8 file. Without replace_all the old string must occur exactly once. No patch syntax.", inputSchema: object({ file_path: string, old_string: string, new_string: string, replace_all: { type: "boolean" } }, ["file_path", "old_string", "new_string"]),
      handler: raw => fileMutation(async () => { const input = value(raw); const filename = path(input); if (typeof input.old_string !== "string" || !input.old_string || typeof input.new_string !== "string") throw new Error("invalid edit strings"); const bytes = await options.filesystem.readFile(filename); if (bytes.byteLength > 2_000_000) throw new Error("Edit file exceeds 2 MB"); const content = new TextDecoder("utf-8").decode(bytes); const pieces = content.split(input.old_string); if (pieces.length === 1 || (input.replace_all !== true && pieces.length !== 2)) throw new Error("old_string must match uniquely unless replace_all is true"); const next = input.replace_all === true ? pieces.join(input.new_string) : content.replace(input.old_string, input.new_string); if (new TextEncoder().encode(next).byteLength > 2_000_000) throw new Error("edited file exceeds 2 MB"); await options.filesystem.writeFile(filename, next); return text(`Edited ${filename}`); }) },
  ];
  if (options.poll) tools.push({ name: "BashOutput", ...(options.poll.supportsParallelToolCalls === true ? { supportsParallelToolCalls: true } : {}), description: "Read output or send ordinary input to a retained native Bash session. session_id is the exact receipt from Bash, bound to its original Hand; never send passwords or verification codes.",
    inputSchema: object({ session_id: { type: "integer", minimum: 1 }, chars: string, max_output_tokens: { type: "integer", minimum: 1, maximum: 10000 }, timeout: { type: "integer", minimum: 1, maximum: 300000 } }, ["session_id"]),
    handler: (raw, context) => { const input=value(raw); return options.poll!.handler({ session_id: input.session_id, chars: input.chars ?? "", max_output_tokens: input.max_output_tokens ?? 10000, ...(input.timeout === undefined ? {} : {yield_time_ms:input.timeout}) },context); }
  });
  // Shared account tools retain their actual permission-checked handlers; only
  // object-schema custom tools are accepted, never Responses builtins/Code Mode.
  for (const tool of options.tools) {
    if (forbidden.has(tool.name) || tool.parameters?.type !== "object") continue;
    tools.push({ name: tool.name, description: tool.description, inputSchema: tool.parameters as Record<string, unknown>, handler: tool.handler,
      ...(tool.supportsParallelToolCalls === true ? { supportsParallelToolCalls: true } : {}) });
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
    tools.push({ name: "MCPToolSearch", supportsParallelToolCalls: true, description: "Discover authorized MCP tools and their input schemas; use MCPExecute with an exact returned name.", inputSchema: object({ query: string, limit: { type: "integer", minimum: 1, maximum: 32 } }, ["query"]), handler: async (raw, context) => {
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
    tools.push({ name: "ToolSearch", supportsParallelToolCalls: true, description: "Discover currently authorized connector and Hand tools. Returns native names and input schemas. Use ToolExecute with an exact returned name.", inputSchema: object({ query: string, limit: { type: "integer", minimum: 1, maximum: 32 } }, ["query"]), handler: raw => {
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
