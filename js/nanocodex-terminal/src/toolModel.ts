import type { ToolActivity } from "nanocodex-react/agent";
import { presentTool } from "./toolPresentation.js";

/** Semantic category used for icons, grouping summaries, and specialized bodies. */
export type ToolKind =
  | "command" | "code" | "read" | "write" | "edit" | "patch" | "search" | "browser"
  | "image" | "preview" | "subagent" | "mcp" | "account" | "message" | "memory" | "generic";

export type DiffLine = Readonly<{ kind: "add" | "remove" | "context" | "gap"; text: string }>;
export type FileDiff = Readonly<{
  path: string;
  operation: "add" | "update" | "delete" | "move";
  movedTo?: string;
  lines: readonly DiffLine[];
  added: number;
  removed: number;
}>;

export type ToolModel = Readonly<{
  kind: ToolKind;
  /** Short verb or label, such as "Read" or "Edit". Commands omit it. */
  label: string;
  /** Primary monospace target: path, command, query, URL. */
  target?: string;
  /** Secondary muted description. */
  detail?: string;
  source?: string;
  duration?: string;
  exitCode?: number;
  command?: Readonly<{ command: string; output?: string; stderr?: string; cwd?: string }>;
  file?: Readonly<{ path: string; content?: string; range?: string }>;
  diffs?: readonly FileDiff[];
  code?: string;
  /** One-line failure summary shown without expanding. */
  error?: string;
  inputText?: string;
  outputText?: string;
}>;

type JsonRecord = Record<string, unknown>;

export function parseJson(value: string | undefined): unknown {
  if (value === undefined || value === "") return undefined;
  try { return JSON.parse(value); } catch { return value; }
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function str(value: unknown, ...keys: string[]): string | undefined {
  if (!isRecord(value)) return undefined;
  for (const key of keys) if (typeof value[key] === "string" && value[key]) return value[key] as string;
  return undefined;
}

function num(value: unknown, key: string): number | undefined {
  return isRecord(value) && typeof value[key] === "number" ? value[key] as number : undefined;
}

/**
 * Bounded inputs may be truncated JSON. Recover a top-level string field
 * without trusting the rest of the payload.
 */
function looseString(raw: string | undefined, key: string): string | undefined {
  if (!raw) return undefined;
  const match = new RegExp(`"${key}"\\s*:\\s*"((?:[^"\\\\]|\\\\.)*)("|…?$)`).exec(raw);
  if (!match) return undefined;
  try { return JSON.parse(`"${match[1]}"`); } catch { return match[1]!.replace(/\\n/g, "\n").replace(/\\"/g, '"'); }
}

function field(input: unknown, raw: string | undefined, ...keys: string[]): string | undefined {
  return str(input, ...keys) ?? keys.map(key => looseString(raw, key)).find(Boolean);
}

export function toolFamily(tool: ToolActivity): { family: string; server?: string } {
  const metadataName = isRecord(tool.metadata)
    ? str(tool.metadata, "tool_name", "toolName") : undefined;
  const name = metadataName ?? tool.name;
  const mcp = /^mcp__(.+?)__(.+)$/.exec(name);
  if (mcp) return { family: mcp[2]!, server: mcp[1]! };
  return { family: name };
}

/** Text returned by shell tools: optional headers followed by "Output:". */
export function parseShellText(text: string): { exitCode?: number; output: string; sessionId?: string } {
  const exit = /^Process exited with code (-?\d+)$/m.exec(text);
  const session = /^Process running with session ID (\S+)$/m.exec(text);
  const marker = text.search(/^Output:\s*$/m);
  const output = marker >= 0 ? text.slice(marker).replace(/^Output:\s*\n?/, "") : text;
  return {
    ...(exit ? { exitCode: Number(exit[1]) } : {}),
    ...(session ? { sessionId: session[1] } : {}),
    output: marker >= 0 || exit ? output : text,
  };
}

const MAX_DIFF_CELLS = 250_000;

/** Line diff for exact-string edits. Large inputs fall back to remove/add blocks. */
export function diffLines(before: string, after: string, context = 3): DiffLine[] {
  const a = before === "" ? [] : before.split("\n");
  const b = after === "" ? [] : after.split("\n");
  let raw: DiffLine[];
  if (a.length * b.length > MAX_DIFF_CELLS) {
    raw = [...a.map(text => ({ kind: "remove" as const, text })), ...b.map(text => ({ kind: "add" as const, text }))];
  } else {
    const table: Uint32Array[] = Array.from({ length: a.length + 1 }, () => new Uint32Array(b.length + 1));
    for (let i = a.length - 1; i >= 0; i--) for (let j = b.length - 1; j >= 0; j--) {
      table[i]![j] = a[i] === b[j] ? table[i + 1]![j + 1]! + 1 : Math.max(table[i + 1]![j]!, table[i]![j + 1]!);
    }
    raw = [];
    let i = 0;
    let j = 0;
    while (i < a.length || j < b.length) {
      if (i < a.length && j < b.length && a[i] === b[j]) { raw.push({ kind: "context", text: a[i]! }); i++; j++; }
      else if (j < b.length && (i >= a.length || table[i]![j + 1]! >= table[i + 1]![j]!)) raw.push({ kind: "add", text: b[j++]! });
      else raw.push({ kind: "remove", text: a[i++]! });
    }
  }
  return trimContext(raw, context);
}

function trimContext(lines: DiffLine[], context: number): DiffLine[] {
  const keep = lines.map(line => line.kind !== "context");
  const visible = lines.map((_, index) => {
    for (let offset = -context; offset <= context; offset++) if (keep[index + offset]) return true;
    return false;
  });
  const output: DiffLine[] = [];
  lines.forEach((line, index) => {
    if (visible[index]) output.push(line);
    else if (output.at(-1)?.kind !== "gap") output.push({ kind: "gap", text: "" });
  });
  return output;
}

function counted(lines: readonly DiffLine[]) {
  return {
    added: lines.filter(line => line.kind === "add").length,
    removed: lines.filter(line => line.kind === "remove").length,
  };
}

/** Parses the apply_patch envelope (and unified diffs) into per-file line changes. */
export function parsePatch(patch: string): FileDiff[] {
  const files: Array<{ path: string; operation: FileDiff["operation"]; movedTo?: string; lines: DiffLine[] }> = [];
  let current: (typeof files)[number] | undefined;
  for (const line of patch.split("\n")) {
    const header = /^\*\*\* (Add|Update|Delete) File: (.+)$/.exec(line);
    if (header) {
      current = { path: header[2]!.trim(), operation: header[1]!.toLowerCase() as FileDiff["operation"], lines: [] };
      files.push(current);
      continue;
    }
    const unified = /^\+\+\+ (?:b\/)?(.+)$/.exec(line);
    if (unified && unified[1] !== "/dev/null") {
      if (!current || current.lines.length) { current = { path: unified[1]!.trim(), operation: "update", lines: [] }; files.push(current); }
      else current.path = unified[1]!.trim();
      continue;
    }
    if (/^--- /.test(line) || /^(diff --git|index |\*\*\* (Begin|End) Patch|\*\*\* End of File)/.test(line)) continue;
    if (!current) continue;
    const move = /^\*\*\* Move to: (.+)$/.exec(line);
    if (move) { current.movedTo = move[1]!.trim(); current.operation = "move"; continue; }
    if (line.startsWith("@@")) { if (current.lines.length) current.lines.push({ kind: "gap", text: line.replace(/^@@+\s?/, "").replace(/\s?@@+.*$/, "") }); continue; }
    if (line.startsWith("+")) current.lines.push({ kind: "add", text: line.slice(1) });
    else if (line.startsWith("-")) current.lines.push({ kind: "remove", text: line.slice(1) });
    else if (line.startsWith(" ")) current.lines.push({ kind: "context", text: line.slice(1) });
    else if (line === "…") current.lines.push({ kind: "gap", text: "Patch preview truncated" });
  }
  return files.map(file => ({ ...file, ...counted(file.lines) }));
}

export function fileDiff(path: string, before: string, after: string, operation: FileDiff["operation"] = "update"): FileDiff {
  const lines = diffLines(before, after);
  return { path, operation, lines, ...counted(lines) };
}

const COMMANDS = new Set(["Bash", "BashOutput", "exec_command", "write_stdin", "sandbox_exec", "sandbox_get_process", "sandbox_start_process", "sandbox_kill_process", "shell", "ssh"]);
const READS = new Set(["Read", "read_file", "read", "view_file"]);
const WRITES = new Set(["Write", "write_file", "create_file"]);
const EDITS = new Set(["Edit", "MultiEdit", "edit_file", "str_replace"]);
const SEARCHES = new Set(["ToolSearch", "MCPToolSearch", "tool_search", "web_search", "web__run", "web_run", "search", "find_session", "find_sessions", "crm_search", "memories__search", "Grep", "Glob", "grep", "glob"]);
const IMAGES = new Set(["view_image", "image_gen__imagegen", "imagegen", "generate_image"]);
const PREVIEWS = new Set(["preview", "sandbox_preview"]);
const SUBAGENTS = new Set(["spawn_agent", "wait_agent", "send_agent_message", "interrupt_agent", "close_agent", "list_agents", "submit_result", "Task", "TaskOutput", "TaskStop"]);
const ACCOUNT = new Set(["accountInfo", "requestAccountConnection", "account_connectors", "environment", "runtimeInfo", "request_vault_intake", "vault_request", "vault_store", "request_secure_input", "request_permissions", "provider_card", "request_native_secure_input", "mount", "server_hand", "thread_sharing"]);
const MESSAGES = new Set(["email", "phone", "phone_numbers", "send_message", "whatsapp"]);

export function toolKind(tool: ToolActivity): ToolKind {
  const { family, server } = toolFamily(tool);
  if (tool.name === "exec" || family === "exec" || family === "wait") return "code";
  if (COMMANDS.has(family)) return "command";
  if (READS.has(family)) return "read";
  if (WRITES.has(family)) return "write";
  if (EDITS.has(family)) return "edit";
  if (family === "apply_patch") return "patch";
  if (SEARCHES.has(family) || /(^|_)search$/.test(family)) return "search";
  if (IMAGES.has(family)) return "image";
  if (PREVIEWS.has(family)) return "preview";
  if (SUBAGENTS.has(family)) return "subagent";
  if (family.startsWith("browser_") || family === "request_browser_login" || family.startsWith("secure_input_") || family.startsWith("cua_repl") || family.startsWith("screen")) return "browser";
  if (ACCOUNT.has(family)) return "account";
  if (MESSAGES.has(family)) return "message";
  if (family.startsWith("crm_") || family.startsWith("memories__") || /cron|goal|apps/.test(family)) return "memory";
  if (server || family === "MCPExecute" || family === "ToolExecute") return "mcp";
  return "generic";
}

function firstLine(value: string | undefined, limit = 160): string | undefined {
  const line = value?.split("\n").map(part => part.trim()).find(Boolean);
  if (!line) return undefined;
  return [...line].length > limit ? `${[...line].slice(0, limit).join("")}…` : line;
}

function outputString(output: unknown, raw: string | undefined): string | undefined {
  if (typeof output === "string") return output;
  if (Array.isArray(output)) {
    const text = output.flatMap(part => isRecord(part) && typeof part.text === "string" ? [part.text] : []);
    if (text.length) return text.join("\n");
  }
  if (isRecord(output)) return str(output, "output", "content", "text", "error", "message") ?? raw;
  return raw;
}

function lineCount(value: string | undefined): number { return value ? value.replace(/\n$/, "").split("\n").length : 0; }

function hostOf(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try { const url = new URL(value); return `${url.host}${url.pathname === "/" ? "" : url.pathname}`; } catch { return value; }
}

/** Builds the specialized presentation for one tool call. Unknown tools keep a readable generic model. */
export function modelTool(tool: ToolActivity): ToolModel {
  const presentation = presentTool(tool);
  const kind = toolKind(tool);
  const { family, server } = toolFamily(tool);
  const rawInput = tool.input ?? tool.arguments;
  const rawOutput = tool.output ?? tool.result;
  const input = parseJson(rawInput);
  const output = parseJson(rawOutput);
  const failed = tool.status === "failed";
  const outputText = outputString(output, rawOutput);
  const base = {
    kind,
    ...(presentation.source && kind !== "command" ? { source: presentation.source } : {}),
    ...(presentation.duration ? { duration: presentation.duration } : {}),
    ...(failed ? { error: firstLine(outputText) ?? "Failed" } : {}),
    ...(rawInput ? { inputText: rawInput } : {}),
    ...(rawOutput ? { outputText: rawOutput } : {}),
  };

  if (kind === "command") {
    const command = field(input, rawInput, family === "exec_command" ? "cmd" : "command", "cmd", "command")
      ?? (isRecord(output) ? str(output, "command") : undefined);
    const cwd = field(input, rawInput, "workdir", "cwd");
    let text: string | undefined;
    let stderr: string | undefined;
    let exitCode: number | undefined;
    if (isRecord(output)) {
      text = str(output, "output", "stdout");
      stderr = str(output, "stderr");
      exitCode = num(output, "exit_code");
    } else if (typeof output === "string") {
      const parsed = parseShellText(output);
      text = parsed.output;
      exitCode = parsed.exitCode;
    }
    const polling = family === "write_stdin" || family === "BashOutput" || family === "sandbox_get_process";
    const session = isRecord(input) ? input.session_id ?? input.process_id : undefined;
    const label = polling ? "Command output" : "";
    const shown = command ?? (session !== undefined ? `session ${String(session)}` : presentation.subject);
    return {
      ...base, label,
      ...(shown ? { target: shown.split("\n")[0] } : {}),
      ...(exitCode !== undefined ? { exitCode } : {}),
      ...(failed && (text || stderr) ? { error: firstLine(stderr || text) } : {}),
      ...(presentation.source ? { source: presentation.source } : {}),
      command: { command: command ?? shown ?? "", ...(text !== undefined ? { output: text } : {}), ...(stderr ? { stderr } : {}), ...(cwd ? { cwd } : {}) },
    };
  }
  if (kind === "code") {
    const code = family === "wait" ? undefined : typeof input === "string" ? input : field(input, rawInput, "code", "source", "input");
    const children = tool.children.length;
    return {
      ...base, label: family === "wait" ? "Wait for code" : "Run code",
      ...(code ? { code } : {}),
      detail: children ? `${children} tool call${children === 1 ? "" : "s"}` : firstLine(code, 100) ?? "",
    };
  }
  if (kind === "read") {
    const path = field(input, rawInput, "file_path", "path") ?? presentation.subject ?? "file";
    const offset = num(input, "offset");
    const limit = num(input, "limit");
    const range = offset !== undefined || limit !== undefined
      ? `L${offset ?? 1}${limit !== undefined ? `–${(offset ?? 1) + limit - 1}` : "+"}` : undefined;
    const content = failed ? undefined : outputText;
    return {
      ...base, label: "Read", target: path,
      ...(range ? { detail: range } : content ? { detail: `${lineCount(content)} lines` } : {}),
      file: { path, ...(content ? { content } : {}), ...(range ? { range } : {}) },
    };
  }
  if (kind === "write") {
    const path = field(input, rawInput, "file_path", "path") ?? "file";
    const content = field(input, rawInput, "content", "text") ?? "";
    const diff = fileDiff(path, "", content, "add");
    return { ...base, label: "Write", target: path, diffs: [diff], file: { path, content } };
  }
  if (kind === "edit") {
    const path = field(input, rawInput, "file_path", "path") ?? "file";
    const before = field(input, rawInput, "old_string", "old_str", "old") ?? "";
    const after = field(input, rawInput, "new_string", "new_str", "new") ?? "";
    const replaceAll = isRecord(input) && input.replace_all === true;
    return { ...base, label: "Edit", target: path, ...(replaceAll ? { detail: "all occurrences" } : {}), diffs: [fileDiff(path, before, after)] };
  }
  if (kind === "patch") {
    const patch = typeof input === "string" ? input : field(input, rawInput, "input", "patch") ?? "";
    const diffs = parsePatch(patch);
    const target = diffs.length === 1 ? diffs[0]!.path : diffs.length ? `${diffs.length} files` : undefined;
    return { ...base, label: "Patch", ...(target ? { target } : {}), diffs };
  }
  if (kind === "search") {
    const query = field(input, rawInput, "query", "q", "pattern", "search_query")
      ?? (isRecord(input) && Array.isArray(input.queries) ? input.queries.filter(item => typeof item === "string").join(", ") : undefined)
      ?? (isRecord(input) && Array.isArray(input.search_query) ? input.search_query.map(item => str(item, "q")).filter(Boolean).join(", ") : undefined)
      ?? presentation.subject;
    const results = Array.isArray(output) ? output.length
      : isRecord(output) ? (["results", "tools", "matches", "items", "records"].map(key => output[key]).find(Array.isArray) as unknown[] | undefined)?.length : undefined;
    const label = family === "web__run" || family === "web_run" || family === "web_search" ? "Web search"
      : /tool/i.test(family) ? "Search tools" : family.startsWith("crm") ? "Search CRM"
        : family.startsWith("memories") ? "Search memory" : /session/.test(family) ? "Search history" : "Search";
    return { ...base, label, ...(query ? { target: query } : {}), ...(results !== undefined ? { detail: `${results} result${results === 1 ? "" : "s"}` } : {}) };
  }
  if (kind === "browser") {
    const url = field(input, rawInput, "url");
    const action = field(input, rawInput, "action", "operation");
    const label = family === "browser_execute" ? "Browser" : `Browser ${presentation.title.replace(/^Browser\s*/i, "").toLowerCase()}`.trim();
    const target = action ? `${action}${url ? ` ${hostOf(url)}` : ""}` : hostOf(url) ?? firstLine(field(input, rawInput, "code"), 100);
    return { ...base, label: family === "browser_execute" ? "Browser" : presentation.title, ...(target ? { target } : {}), ...(label === "Browser" ? {} : {}) };
  }
  if (kind === "image") {
    const path = field(input, rawInput, "path", "prompt");
    return { ...base, label: family === "view_image" ? "View image" : "Generate image", ...(path ? { target: path } : {}) };
  }
  if (kind === "mcp") {
    const inner = field(input, rawInput, "name", "tool");
    return {
      ...base, label: server ? `${server.replace(/[_-]+/g, " ")} · ${family.replace(/[_-]+/g, " ")}` : presentation.title,
      ...(inner ? { target: inner } : presentation.subject ? { detail: presentation.subject } : {}),
    };
  }
  return {
    ...base, label: presentation.title,
    ...(presentation.subject ? { detail: presentation.subject } : {}),
    ...(!failed && presentation.outputSummary && kind !== "subagent" ? {} : {}),
  };
}
