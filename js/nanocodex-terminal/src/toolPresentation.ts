import type { ToolActivity } from "nanocodex-react/agent";

type JsonRecord = Record<string, unknown>;

export type ToolPresentation = Readonly<{
  duration?: string;
  inputDetail?: Readonly<{ label: string; value: string }>;
  outputSummary?: string;
  outputDetails?: readonly Readonly<{ label: string; value: string }>[];
  previewUrl?: string;
  source?: string;
  subject?: string;
  title: string;
}>;

const TITLE_OVERRIDES: Readonly<Record<string, string>> = {
  accountInfo: "Account info",
  requestAccountConnection: "Connect account",
  exec: "Run code",
  exec_command: "Run command",
  sandbox_exec: "Run command",
  sandbox_get_process: "Check process",
  sandbox_kill_process: "Stop process",
  sandbox_preview: "Open preview",
  sandbox_start_process: "Start process",
};

const OPAQUE_MACHINE_TOOL = "opaque_machine_tool";

export function presentTool(tool: ToolActivity): ToolPresentation {
  const decodedName = decodeToolName(tool.name, tool.metadata);
  const input = parseDetail(tool.input ?? tool.arguments);
  const output = parseDetail(tool.output ?? tool.result);
  const family = decodedName.family;
  const semanticWrapper = tool.name === "exec" && tool.children.length > 0;
  const previewUrl = family === "sandbox_preview" ? safeHttpUrl(field(output, "url")) : undefined;
  const subject = semanticWrapper ? undefined : summarizeInput(family, input);
  const outputSummary = semanticWrapper ? undefined : summarizeOutput(family, output);
  const source = toolSource(decodedName.sources, family, input, semanticWrapper);
  const executionDetails = semanticWrapper ? undefined : semanticExecutionDetails(family, input, output);
  return {
    title: toolTitle(tool, family, input, output),
    ...(source ? { source } : {}),
    ...(subject ? { subject } : {}),
    ...(outputSummary ? { outputSummary } : {}),
    ...(tool.durationNs === undefined ? {} : { duration: formatDuration(tool.durationNs) }),
    ...(previewUrl ? { previewUrl } : {}),
    ...executionDetails,
  };
}

function semanticExecutionDetails(
  family: string,
  input: unknown,
  output: unknown,
): Pick<ToolPresentation, "inputDetail" | "outputDetails"> | undefined {
  if (family !== "sandbox_exec" && family !== "sandbox_get_process" && family !== "exec_command") return undefined;
  const commandKey = family === "exec_command" ? "cmd" : "command";
  const command = family === "sandbox_get_process"
    ? isRecord(output) ? stringField(output, "command") : undefined
    : isRecord(input) ? stringField(input, commandKey) : undefined;
  const outputRecord = isRecord(output) ? output : undefined;
  const stdout = outputRecord
    ? stringField(outputRecord, family === "exec_command" ? "output" : "stdout")
    : undefined;
  const stderr = outputRecord ? stringField(outputRecord, "stderr") : undefined;
  return {
    ...(command ? { inputDetail: { label: "Command", value: command } } : {}),
    ...(stdout === undefined && stderr === undefined ? {} : {
      outputDetails: [
        ...(stdout === undefined ? [] : [{ label: family === "exec_command" ? "Output" : "Stdout", value: stdout || "(empty)" }]),
        ...(stderr === undefined ? [] : [{ label: "Stderr", value: stderr || "(empty)" }]),
      ],
    }),
  };
}

function toolTitle(tool: ToolActivity, family: string, input: unknown, output: unknown): string {
  if (family === "spawn_agent") {
    const role = recordString(output, "role") ?? recordString(input, "role");
    if (role) return `${tool.status === "completed" ? "Spawned" : "Spawn"} ${compact(role)}`;
  }
  if (family === "wait_agent") return `Waiting on ${subagentTarget(input, output, true)}`;
  if (family === "send_agent_message") return `Message ${subagentTarget(input, output)}`;
  if (family === "interrupt_agent") return `Interrupt ${subagentTarget(input, output)}`;
  if (family === "close_agent") return `Close ${subagentTarget(input, output)}`;
  return TITLE_OVERRIDES[family] ?? humanize(
    family.startsWith("sandbox_") ? family.slice("sandbox_".length) : family,
  );
}

export function boundedToolDetail(value: string): string {
  let readable = value;
  try { readable = JSON.stringify(JSON.parse(value), null, 2); } catch { /* Plain text stays plain. */ }
  const lines = readable.trim().split("\n");
  const output = lines.slice(0, 24).join("\n");
  const characters = [...output];
  if (characters.length > 4_000) return `${characters.slice(0, 4_000).join("")}…`;
  return lines.length > 24 ? `${output}\n…` : output;
}

function decodeToolName(name: string, metadata: unknown): { family: string; sources: string[] } {
  const sources: string[] = [];
  const metadataName = metadataString(metadata, ["tool_name", "toolName"]);
  const metadataMachine = metadataString(metadata, ["machine_name", "machineName"])
    ?? metadataString(metadata, ["machine_id", "machineId"]);
  const machineAlias = name.startsWith("user_");
  let family = metadataName ?? (machineAlias ? OPAQUE_MACHINE_TOOL : name);
  if (metadataMachine || machineAlias) {
    sources.push(metadataMachine ? `Machine ${metadataMachine}` : "Machine");
  }
  const mcp = /^mcp__(.+?)__(.+)$/.exec(family);
  if (mcp) {
    sources.push(`MCP ${humanize(mcp[1])}`);
    family = mcp[2];
  }
  if (sources.length === 0 && family.startsWith("sandbox_")) sources.push("Sandbox");
  if (family.startsWith("browser_")) {
    if (sources.length === 0) sources.push(family === "browser_execute" ? "Managed browser" : "Web client");
    family = family.slice("browser_".length);
  } else if (sources.length === 0 && family === "exec_command") sources.push("Local");
  else if (sources.length === 0 && SUBAGENT_TOOLS.has(family)) sources.push("Subagent");
  else if (sources.length === 0 && family === "accountInfo") sources.push("Account");
  else if (sources.length === 0 && family === "requestAccountConnection") sources.push("Account");
  return { family, sources };
}

function metadataString(value: unknown, keys: readonly string[]): string | undefined {
  if (!isRecord(value)) return undefined;
  for (const key of keys) {
    if (typeof value[key] === "string") return value[key];
  }
  return undefined;
}

const SUBAGENT_TOOLS = new Set([
  "close_agent",
  "interrupt_agent",
  "list_agents",
  "send_agent_message",
  "spawn_agent",
  "submit_result",
  "wait_agent",
]);

function toolSource(
  sources: string[],
  family: string,
  input: unknown,
  semanticWrapper: boolean,
): string | undefined {
  const resolved = sources.slice();
  if (semanticWrapper && resolved.length === 0) resolved.push("Code mode");
  const executionDirectory = isRecord(input)
    ? family.startsWith("sandbox_")
      ? stringField(input, "cwd")
      : family === "exec_command" || resolved[0] === "Machine" || resolved[0]?.startsWith("Machine ")
        ? stringField(input, "workdir")
        : undefined
    : undefined;
  if (executionDirectory && resolved.length > 0) {
    resolved[0] = `${resolved[0]} · ${compact(executionDirectory)}`;
  }
  return resolved.length ? resolved.join(" · ") : undefined;
}

function summarizeInput(family: string, input: unknown): string | undefined {
  if (isRecord(input)) {
    if (family === "spawn_agent") return compact(stringField(input, "task") ?? "");
    if (family === "wait_agent" || family === "interrupt_agent" || family === "close_agent") {
      return undefined;
    }
    if (family === "send_agent_message") return compact(stringField(input, "message") ?? "");
    if (family === "sandbox_preview" && typeof input.port === "number") return `Port ${input.port}`;
    const command = stringField(input, family === "exec_command" ? "cmd" : "command");
    if (command) return compact(command);
    for (const key of ["path", "file_path", "query", "url", "port", "process_id", "session_id"]) {
      const value = input[key];
      if (typeof value === "string" || typeof value === "number") return compact(String(value));
    }
    const keys = Object.keys(input);
    if (keys.length) return `${keys.length} input field${keys.length === 1 ? "" : "s"}`;
    return undefined;
  }
  return typeof input === "string" ? compact(input) : undefined;
}

function summarizeOutput(family: string, output: unknown): string | undefined {
  if (Array.isArray(output)) {
    const textParts = output.filter((part) => isRecord(part)
      && ["input_text", "output_text", "text"].includes(String(part.type))
      && typeof part.text === "string");
    return compact(textParts.length === output.length
      ? textParts.map((part) => part.text).join("\n")
      : stringify(output));
  }
  if (isRecord(output)) {
    if (SUBAGENT_TOOLS.has(family)) {
      const summary = summarizeSubagentOutput(family, output);
      if (summary) return summary;
    }
    if (family === "sandbox_get_process") {
      if (output.found === false) return "Not found";
      const parts = executionSummaryParts(output);
      const status = stringField(output, "status");
      if (status) parts.unshift(humanize(status));
      if (parts.length) return parts.join(" · ");
    }
    if (family === "sandbox_kill_process") {
      if (output.found === false) return "Not found";
      const status = stringField(output, "status");
      if (status) return humanize(status);
    }
    if (family === "sandbox_exec" || family === "exec_command") {
      const parts = executionSummaryParts(output);
      if (family === "exec_command") addLineCount(parts, output, "output");
      if (parts.length) return parts.join(" · ");
    }
    if (family === "sandbox_start_process") {
      const parts: string[] = [];
      const processId = stringField(output, "process_id");
      const pid = numberField(output, "pid");
      const status = stringField(output, "status");
      const port = numberField(output, "ready_port");
      if (pid !== undefined) parts.push(`PID ${pid}`);
      else if (processId) parts.push(`Process ${compact(processId)}`);
      if (status) parts.push(humanize(status));
      if (port !== undefined) parts.push(`Port ${port} ready`);
      if (parts.length) return parts.join(" · ");
    }
    if (family === "sandbox_preview" && safeHttpUrl(field(output, "url"))) return "Preview ready";
    if (family === "accountInfo") {
      const parts: string[] = [];
      const status = stringField(output, "status");
      if (status) parts.push(humanize(status));
      const connectorAccounts = recordField(output, "connectorAccounts");
      const connectorCount = connectorAccounts
        ? Object.values(connectorAccounts).reduce<number>(
          (count, value) => count + (Array.isArray(value) ? value.length : 0),
          0,
        )
        : arrayField(output, "authenticated")?.length;
      if (connectorCount !== undefined) parts.push(counted(connectorCount, "connector"));
      const machines = arrayField(output, "machines");
      if (machines) parts.push(counted(machines.length, "machine"));
      const vault = arrayField(output, "vault");
      if (vault) parts.push(counted(vault.length, "Vault item"));
      if (parts.length) return parts.join(" · ");
    }
    return compact(stringify(output));
  }
  if (typeof output === "string") return compact(output);
  if (output === undefined || output === null) return undefined;
  return compact(String(output));
}

function executionSummaryParts(output: JsonRecord): string[] {
  const parts: string[] = [];
  const exitCode = numberField(output, "exit_code");
  if (exitCode !== undefined) parts.push(`Exit ${exitCode}`);
  addLineCount(parts, output, "stdout");
  addLineCount(parts, output, "stderr");
  return parts;
}

function summarizeSubagentOutput(family: string, output: JsonRecord): string | undefined {
  if (family === "spawn_agent") {
    const parts: string[] = [];
    const id = numberField(output, "agent_id");
    const state = nestedState(output.status);
    if (id !== undefined) parts.push(`Agent ${id}`);
    if (state) parts.push(humanize(state));
    return parts.length ? parts.join(" · ") : undefined;
  }
  if (family === "wait_agent") {
    if (output.timed_out === true) return "Timed out";
    const agents = arrayField(output, "agents");
    if (!agents?.length) return undefined;
    return agents.slice(0, 3).flatMap((agent) => {
      if (!isRecord(agent)) return [];
      const id = numberField(agent, "agent_id");
      const role = stringField(agent, "role");
      const state = nestedState(agent.status);
      const identity = role ? compact(role) : id === undefined ? "Agent" : `Agent ${id}`;
      return [`${identity}${role && id !== undefined ? ` (${id})` : ""}${state ? ` · ${humanize(state)}` : ""}`];
    }).join("; ");
  }
  const state = nestedState(output.status);
  if (state) return humanize(state);
  if (output.accepted === true) return "Accepted";
  return undefined;
}

function subagentTarget(input: unknown, output: unknown, allowMany = false): string {
  if (isRecord(output)) {
    const role = stringField(output, "role");
    const id = numberField(output, "agent_id");
    if (role) return `${compact(role)}${id === undefined ? "" : ` (${id})`}`;
    const agents = arrayField(output, "agents");
    const first = agents?.find(isRecord);
    if (first) {
      const firstRole = stringField(first, "role");
      const firstId = numberField(first, "agent_id");
      if (firstRole) return `${compact(firstRole)}${firstId === undefined ? "" : ` (${firstId})`}`;
    }
  }
  if (isRecord(input)) {
    const id = numberField(input, "agent_id");
    if (id !== undefined) return `agent ${id}`;
    const ids = arrayField(input, "agent_ids")?.filter((value): value is number => typeof value === "number");
    if (ids?.length) return allowMany ? `agents ${ids.join(", ")}` : `agent ${ids[0]}`;
  }
  return allowMany ? "agents" : "agent";
}

function nestedState(value: unknown): string | undefined {
  return isRecord(value) && typeof value.state === "string" ? value.state : undefined;
}

function recordString(value: unknown, key: string): string | undefined {
  return isRecord(value) ? stringField(value, key) : undefined;
}

function addLineCount(parts: string[], output: JsonRecord, key: string): void {
  const value = stringField(output, key);
  if (!value) return;
  const lines = value.split("\n").length;
  parts.push(`${lines} ${key} line${lines === 1 ? "" : "s"}`);
}

function formatDuration(nanoseconds: number): string {
  if (nanoseconds < 1_000) return `${Math.max(0, Math.round(nanoseconds))} ns`;
  if (nanoseconds < 1_000_000) return `${formatNumber(nanoseconds / 1_000)} µs`;
  if (nanoseconds < 1_000_000_000) return `${formatNumber(nanoseconds / 1_000_000)} ms`;
  return `${formatNumber(nanoseconds / 1_000_000_000)} s`;
}

function formatNumber(value: number): string {
  return value >= 100 ? value.toFixed(0) : value >= 10 ? value.toFixed(1) : value.toFixed(2);
}

function parseDetail(value: string | undefined): unknown {
  if (value === undefined || value === "") return undefined;
  try { return JSON.parse(value); } catch { return value; }
}

function humanize(value: string): string {
  if (value === OPAQUE_MACHINE_TOOL) return "Machine tool";
  const words = value
    .replace(/([a-z\d])([A-Z])/g, "$1 $2")
    .replace(/[_-]+/g, " ")
    .trim()
    .toLowerCase();
  return words ? `${words[0].toUpperCase()}${words.slice(1)}` : "Tool";
}

function compact(value: string): string {
  const normalized = value.split(/\s+/).filter(Boolean).join(" ");
  return [...normalized].length <= 140 ? normalized : `${[...normalized].slice(0, 140).join("")}…`;
}

function stringify(value: unknown): string {
  try { return JSON.stringify(value); } catch { return String(value); }
}

function counted(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function safeHttpUrl(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  try {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function field(value: unknown, key: string): unknown {
  return isRecord(value) ? value[key] : undefined;
}

function stringField(value: JsonRecord, key: string): string | undefined {
  return typeof value[key] === "string" ? value[key] : undefined;
}

function numberField(value: JsonRecord, key: string): number | undefined {
  return typeof value[key] === "number" ? value[key] : undefined;
}

function recordField(value: JsonRecord, key: string): JsonRecord | undefined {
  return isRecord(value[key]) ? value[key] : undefined;
}

function arrayField(value: JsonRecord, key: string): unknown[] | undefined {
  return Array.isArray(value[key]) ? value[key] : undefined;
}
