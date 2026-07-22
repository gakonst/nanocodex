export type JsonObject = Record<string, unknown>;

export type AgentEvent = {
  protocol_version: number;
  request_id: string;
  seq: number;
  type: string;
  payload: JsonObject;
};

export type ToolStatus = "running" | "completed" | "cancelled" | "failed";

export type ToolActivity = {
  callId: string;
  name: string;
  arguments: string;
  result?: string;
  status: ToolStatus;
  durationNs?: number;
  images?: string[];
  children: ToolActivity[];
};

export type TerminalEntry =
  | { id: string; kind: "user"; text: string; promptId?: number }
  | { id: string; kind: "reasoning"; text: string; streaming: boolean }
  | { id: string; kind: "assistant"; text: string; streaming: boolean }
  | { id: string; kind: "tool"; tool: ToolActivity }
  | { id: string; kind: "error"; text: string };

export type PendingSteer = {
  id: number;
  text: string;
  state: "submitting" | "admitted";
  runGeneration: number;
};

export type PendingPrompt = { id: number; text: string };

export type TerminalState = {
  entries: TerminalEntry[];
  running: boolean;
  status: string;
  pendingTurns: number;
  queuedPrompts: PendingPrompt[];
  displayedQueuedPrompt?: number;
  pendingSteers: PendingSteer[];
  appliedSteerRuns: number[];
  runGeneration: number;
  streamedThisTurn: boolean;
  pendingRunError?: string;
  modelCalls: number;
  syntheticId: number;
};

export function initialTerminalState(status = "Ready"): TerminalState {
  return {
    entries: [],
    running: false,
    status,
    pendingTurns: 0,
    queuedPrompts: [],
    pendingSteers: [],
    appliedSteerRuns: [],
    runGeneration: 0,
    streamedThisTurn: false,
    modelCalls: 0,
    syntheticId: 0,
  };
}

export function queuePrompt(
  state: TerminalState,
  id: number,
  text: string,
): TerminalState {
  const displayImmediately = !state.running && state.queuedPrompts.length === 0;
  return {
    ...state,
    entries: displayImmediately
      ? [...state.entries, { id: `user-${id}`, kind: "user", text, promptId: id }]
      : state.entries,
    queuedPrompts: [...state.queuedPrompts, { id, text }],
    displayedQueuedPrompt: displayImmediately ? id : state.displayedQueuedPrompt,
    pendingTurns: state.pendingTurns + 1,
    status: state.running ? "Prompt queued" : "Starting",
  };
}

export function queueSteer(
  state: TerminalState,
  id: number,
  text: string,
): TerminalState {
  return {
    ...state,
    pendingSteers: [
      ...state.pendingSteers,
      { id, text, state: "submitting", runGeneration: state.runGeneration },
    ],
    status: "Submitting steer",
  };
}

export function steerAdmitted(state: TerminalState, id: number): TerminalState {
  const pendingSteers = state.pendingSteers.map((steer) =>
    steer.id === id ? { ...steer, state: "admitted" as const } : steer,
  );
  return reconcileSteers({
    ...state,
    pendingSteers,
    status: state.running ? "Steer pending" : state.status,
  });
}

export function steerQueued(
  state: TerminalState,
  id: number,
  text: string,
): TerminalState {
  return queuePrompt(removeSteer(state, id), id, text);
}

export function steerFailed(
  state: TerminalState,
  id: number,
  error: string,
): TerminalState {
  return appendError(removeSteer(state, id), error);
}

export function turnFinished(state: TerminalState, error?: string): TerminalState {
  const next = {
    ...state,
    pendingTurns: Math.max(0, state.pendingTurns - 1),
  };
  return error && error !== "the turn was cancelled" ? appendError(next, error) : next;
}

export function appendError(state: TerminalState, text: string): TerminalState {
  const syntheticId = state.syntheticId + 1;
  return {
    ...state,
    syntheticId,
    entries: [...state.entries, { id: `error-${syntheticId}`, kind: "error", text }],
  };
}

export function applyAgentEvents(
  state: TerminalState,
  events: readonly AgentEvent[],
): TerminalState {
  if (events.length === 0) return state;

  let next = { ...state, entries: state.entries.slice() };
  let bufferedKind: "assistant" | "reasoning" | undefined;
  let bufferedId = "";
  let bufferedText: string[] = [];

  const flushDeltas = () => {
    if (!bufferedKind || bufferedText.length === 0) return;
    const text = bufferedText.join("");
    const tail = next.entries.at(-1);
    if (tail?.kind === bufferedKind && tail.streaming) {
      next.entries[next.entries.length - 1] = { ...tail, text: tail.text + text };
    } else {
      sealStreamingTail(next.entries);
      next.entries.push({ id: bufferedId, kind: bufferedKind, text, streaming: true });
    }
    next.streamedThisTurn ||= bufferedKind === "assistant";
    bufferedKind = undefined;
    bufferedId = "";
    bufferedText = [];
  };

  for (const event of events) {
    if (event.type === "assistant.delta" || event.type === "reasoning.summary.delta") {
      const kind = event.type === "assistant.delta" ? "assistant" : "reasoning";
      if (bufferedKind && bufferedKind !== kind) flushDeltas();
      bufferedKind = kind;
      bufferedId ||= `${kind}-${event.seq}`;
      bufferedText.push(payloadString(event.payload, "text") ?? "");
      continue;
    }
    flushDeltas();

    switch (event.type) {
      case "run.started": {
        const [prompt, ...queuedPrompts] = next.queuedPrompts;
        if (prompt && next.displayedQueuedPrompt !== prompt.id) {
          next.entries.push({
            id: `user-${prompt.id}`,
            kind: "user",
            text: prompt.text,
            promptId: prompt.id,
          });
        }
        next = {
          ...next,
          queuedPrompts,
          displayedQueuedPrompt: undefined,
          running: true,
          runGeneration: next.runGeneration + 1,
          streamedThisTurn: false,
          pendingRunError: undefined,
          status: "Thinking...",
        };
        break;
      }
      case "run.steered":
        next = reconcileSteers({
          ...next,
          appliedSteerRuns: [...next.appliedSteerRuns, next.runGeneration],
          status: "Steer applied",
        });
        break;
      case "assistant.message": {
        const text = payloadString(event.payload, "text") ?? "";
        const tail = next.entries.at(-1);
        if (tail?.kind === "assistant") {
          next.entries[next.entries.length - 1] = { ...tail, text, streaming: false };
        } else if (text) {
          next.entries.push({ id: `assistant-${event.seq}`, kind: "assistant", text, streaming: false });
        }
        break;
      }
      case "tool.call":
        applyToolCall(next.entries, event);
        next.status = `Running ${payloadString(event.payload, "tool") ?? "tool"}`;
        break;
      case "tool.result":
        applyToolResult(next.entries, event);
        next.status = "Working";
        break;
      case "model.call.completed":
        next.modelCalls += 1;
        break;
      case "run.error":
        next.pendingRunError = payloadString(event.payload, "message");
        break;
      case "run.completed":
        sealStreamingTail(next.entries);
        if (next.pendingRunError) next = appendError(next, next.pendingRunError);
        next = reconcileSteers({
          ...next,
          running: false,
          pendingRunError: undefined,
          status: "Ready",
        });
        break;
      case "run.failed": {
        sealStreamingTail(next.entries);
        const cancelled = payloadString(event.payload, "status") === "cancelled";
        if (!cancelled && next.pendingRunError) next = appendError(next, next.pendingRunError);
        next = reconcileSteers({
          ...next,
          running: false,
          pendingRunError: undefined,
          status: cancelled ? "Cancelled" : "Turn failed",
        });
        break;
      }
    }
  }
  flushDeltas();
  return next;
}

export function pendingCount(state: TerminalState): number {
  return state.pendingSteers.length + state.queuedPrompts.length;
}

function reconcileSteers(state: TerminalState): TerminalState {
  const pendingSteers = state.pendingSteers.slice();
  const appliedSteerRuns = state.appliedSteerRuns.slice();
  const entries = state.entries.slice();
  let applied = 0;
  while (appliedSteerRuns.length > 0) {
    const generation = appliedSteerRuns[0];
    const index = pendingSteers.findIndex(
      (steer) => steer.runGeneration === generation && steer.state === "admitted",
    );
    if (index < 0) break;
    const [steer] = pendingSteers.splice(index, 1);
    if (!steer) break;
    entries.push({ id: `steer-${steer.id}`, kind: "user", text: steer.text });
    appliedSteerRuns.shift();
    applied += 1;
  }
  if (!state.running) {
    const waiting = new Set(appliedSteerRuns);
    return {
      ...state,
      entries,
      pendingSteers: pendingSteers.filter((steer) => waiting.has(steer.runGeneration)),
      appliedSteerRuns,
      status: applied ? "Steer applied" : state.status,
    };
  }
  return { ...state, entries, pendingSteers, appliedSteerRuns };
}

function removeSteer(state: TerminalState, id: number): TerminalState {
  return { ...state, pendingSteers: state.pendingSteers.filter((steer) => steer.id !== id) };
}

function applyToolCall(entries: TerminalEntry[], event: AgentEvent): void {
  const callId = payloadString(event.payload, "call_id") ?? `tool-${event.seq}`;
  const name = payloadString(event.payload, "tool") ?? "tool";
  const tool: ToolActivity = {
    callId,
    name,
    arguments: summarizeToolArguments(name, event.payload.arguments),
    status: "running",
    children: [],
  };
  const parentId = callId.split("/code-")[0];
  if (parentId !== callId) {
    let parentIndex = -1;
    for (let index = entries.length - 1; index >= 0; index -= 1) {
      const entry = entries[index];
      if (entry?.kind === "tool" && entry.tool.callId === parentId) {
        parentIndex = index;
        break;
      }
    }
    const parent = entries[parentIndex];
    if (parent?.kind === "tool") {
      entries[parentIndex] = {
        ...parent,
        tool: { ...parent.tool, children: [...parent.tool.children, tool] },
      };
      return;
    }
  }
  entries.push({ id: `tool-${callId}`, kind: "tool", tool });
}

function applyToolResult(entries: TerminalEntry[], event: AgentEvent): void {
  const callId = payloadString(event.payload, "call_id");
  if (!callId) return;
  const statusValue = payloadString(event.payload, "status");
  const status: ToolStatus =
    statusValue === "cancelled" ? "cancelled" : statusValue === "completed" ? "completed" : "failed";
  for (let index = entries.length - 1; index >= 0; index -= 1) {
    const entry = entries[index];
    if (entry?.kind !== "tool") continue;
    if (entry.tool.callId === callId) {
      entries[index] = {
        ...entry,
        tool: completedTool(entry.tool, event, status),
      };
      return;
    }
    const childIndex = entry.tool.children.findIndex((child) => child.callId === callId);
    if (childIndex >= 0) {
      const children = entry.tool.children.slice();
      children[childIndex] = completedTool(children[childIndex]!, event, status);
      entries[index] = { ...entry, tool: { ...entry.tool, children } };
      return;
    }
  }
}

function completedTool(
  tool: ToolActivity,
  event: AgentEvent,
  status: ToolStatus,
): ToolActivity {
  const images = extractImageUrls(event.payload.result);
  return {
    ...tool,
    status,
    durationNs: payloadNumber(event.payload, "duration_ns"),
    ...(images ? { images } : {}),
    result: summarizeToolResult(tool.name, event.payload.result, status),
  };
}

function extractImageUrls(value: unknown): string[] | undefined {
  const decoded = decodeJsonString(value);
  if (!Array.isArray(decoded)) return undefined;
  const images = decoded.flatMap((item) => {
    if (!isObject(item) || item.type !== "input_image" || typeof item.image_url !== "string") return [];
    return [item.image_url];
  });
  return images.length ? images : undefined;
}

function sealStreamingTail(entries: TerminalEntry[]): void {
  const tail = entries.at(-1);
  if (tail && (tail.kind === "assistant" || tail.kind === "reasoning") && tail.streaming) {
    entries[entries.length - 1] = { ...tail, streaming: false };
  }
}

function payloadString(payload: JsonObject, key: string): string | undefined {
  return typeof payload[key] === "string" ? payload[key] : undefined;
}

function payloadNumber(payload: JsonObject, key: string): number | undefined {
  return typeof payload[key] === "number" ? payload[key] : undefined;
}

function summarizeToolArguments(tool: string, value: unknown): string {
  if (tool === "exec" && typeof value === "string") return boundedMultiline(value);
  if (isObject(value)) {
    if (tool === "write_stdin" && value.session_id !== undefined) return `session ${String(value.session_id)}`;
    const preferred =
      tool === "exec_command" ? value.cmd :
      tool === "view_image" ? value.path :
      tool === "read_file" ? value.path ?? value.file_path :
      tool === "wait" ? value.cell_id : undefined;
    if (typeof preferred === "string") {
      return tool === "exec_command" && preferred.includes("\n")
        ? boundedMultiline(preferred)
        : compact(preferred);
    }
  }
  if (tool === "apply_patch" && typeof value === "string") {
    const lines = value.split("\n");
    const files = lines.flatMap((line) => {
      const prefix = ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
        .find((candidate) => line.startsWith(candidate));
      return prefix ? [line.slice(prefix.length)] : [];
    });
    if (files.length) {
      const added = lines.filter((line) => line.startsWith("+")).length;
      const removed = lines.filter((line) => line.startsWith("-")).length;
      return compact(`${files.join(", ")} (+${added} -${removed})`);
    }
  }
  return compact(formatValue(value));
}

function summarizeToolResult(tool: string, value: unknown, status: ToolStatus): string | undefined {
  if (tool === "exec_command") {
    const decoded = decodeJsonString(value);
    if (isObject(decoded)) {
      const parts: string[] = [];
      if (typeof decoded.exit_code === "number") parts.push(`exit ${decoded.exit_code}`);
      if (typeof decoded.output === "string") {
        const lines = decoded.output ? decoded.output.split("\n").length : 0;
        if (lines) parts.push(`${lines} line${lines === 1 ? "" : "s"}`);
      }
      if (parts.length) return parts.join(" · ");
    }
  }
  if (tool === "apply_patch" && typeof value === "string" && value.includes("Success")) return "applied";
  return status === "failed" || status === "cancelled" ? compact(formatValue(value)) : undefined;
}

function decodeJsonString(value: unknown): unknown {
  if (typeof value !== "string") return value;
  try { return JSON.parse(value); } catch { return value; }
}

function compact(value: string): string {
  const normalized = value.split(/\s+/).filter(Boolean).join(" ");
  return [...normalized].length <= 180 ? normalized : `${[...normalized].slice(0, 180).join("")}…`;
}

function boundedMultiline(value: string): string {
  const lines = value.trim().split("\n");
  let output = "";
  let characters = 0;
  for (let index = 0; index < lines.length; index += 1) {
    if (index >= 24) return `${output}\n…`;
    if (index) output += "\n";
    for (const character of lines[index]!) {
      if (characters >= 4_000) return `${output}…`;
      output += character;
      characters += 1;
    }
  }
  return output;
}

function formatValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined) return "";
  try { return JSON.stringify(value); } catch { return String(value); }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
