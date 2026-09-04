import type { ManagedEvent } from "nanocodex/managed";

export type Entry = {
  id: string;
  turnId: string;
  kind: "user" | "assistant" | "reasoning" | "tool" | "error" | "status";
  text: string;
  name?: string;
  output?: string;
  status?: string;
  agent?: string;
  streaming?: boolean;
};
export function timeline(events: readonly ManagedEvent[]): Entry[] {
  const entries: Entry[] = [];
  const seen = new Set<string>();
  for (const envelope of events) {
    if (seen.has(envelope.cursor)) continue;
    seen.add(envelope.cursor);
    const data = envelope.data;
    const turnId = envelope.turnId ?? ("id" in data ? String(data.id) : "");
    const id = `${turnId}:${envelope.cursor}`;
    if (data.type === "turn_accepted")
      entries.push({ id, turnId, kind: "user", text: promptText(data.input) });
    if (data.type === "turn_completed") {
      const final = [...entries]
        .reverse()
        .find((entry) => entry.turnId === turnId && entry.kind === "assistant");
      if (final?.text !== data.final_message && data.final_message)
        entries.push({
          id,
          turnId,
          kind: "assistant",
          text: data.final_message,
        });
      for (const entry of entries)
        if (entry.turnId === turnId) entry.streaming = false;
    }
    if (data.type === "turn_failed" || data.type === "turn_cancelled") {
      entries.push({
        id,
        turnId,
        kind: "error",
        text: data.type === "turn_cancelled" ? "Stopped by you." : data.error,
      });
      for (const entry of entries)
        if (entry.turnId === turnId) {
          entry.streaming = false;
          if (entry.status === "running") entry.status = "cancelled";
        }
    }
    if (data.type !== "event" || !data.event || typeof data.event !== "object")
      continue;
    const event = data.event as {
      type: string;
      payload?: Record<string, unknown>;
    };
    const p = event.payload ?? {};
    const agent = data.agent_id == null ? undefined : String(data.agent_id);
    if (
      event.type === "assistant.delta" ||
      event.type === "reasoning.summary.delta"
    ) {
      const kind = event.type === "assistant.delta" ? "assistant" : "reasoning";
      const tail = entries.at(-1);
      if (
        tail?.kind === kind &&
        tail.turnId === turnId &&
        tail.agent === agent &&
        tail.streaming
      )
        tail.text += string(p.text);
      else
        entries.push({
          id,
          turnId,
          kind,
          text: string(p.text),
          streaming: true,
          agent,
        });
    }
    if (event.type === "assistant.message") {
      const tail = entries.at(-1);
      if (
        tail?.kind === "assistant" &&
        tail.turnId === turnId &&
        tail.agent === agent &&
        tail.streaming
      ) {
        tail.text = string(p.text);
        tail.streaming = false;
      } else if (p.text)
        entries.push({
          id,
          turnId,
          kind: "assistant",
          text: string(p.text),
          agent,
        });
    }
    if (event.type === "run.steered")
      entries.push({ id, turnId, kind: "status", text: "Direction updated" });
    if (event.type === "tool.call")
      entries.push({
        id: `${turnId}:tool:${p.call_id}`,
        turnId,
        kind: "tool",
        name: string(p.tool),
        text: detail(p.arguments),
        status: "running",
        agent,
      });
    if (event.type === "tool.result") {
      const entry = entries.find(
        (entry) => entry.id === `${turnId}:tool:${p.call_id}`,
      );
      if (entry) {
        entry.output = detail(p.structured_result ?? p.result);
        entry.status = string(p.status);
      }
    }
    if (
      event.type === "run.error" &&
      !/^the turn was cancel(?:l)?ed\.?$/i.test(string(p.message))
    )
      entries.push({ id, turnId, kind: "error", text: string(p.message) });
  }
  return entries;
}
export function detail(value: unknown): string {
  if (typeof value === "string") {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return value == null ? "" : JSON.stringify(value, null, 2);
}
export function promptText(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value))
    return value.map((item) => item?.text ?? "[attachment]").join("\n");
  return "";
}
function string(value: unknown) {
  return typeof value === "string" ? value : "";
}

export type DiscoveredHand = {
  id: string;
  name: string;
  mount: string;
  kind: string;
  capabilities: string[];
};
/** Inventory is reported by the real accountInfo tool in this thread. */
export function discoveredHands(entries: Entry[]): DiscoveredHand[] {
  for (const entry of [...entries].reverse()) {
    if (entry.kind !== "tool" || !entry.output || entry.status !== "completed")
      continue;
    try {
      const parsed = JSON.parse(entry.output);
      const value =
        typeof parsed?.output === "string" ? JSON.parse(parsed.output) : parsed;
      if (!Array.isArray(value?.machines)) continue;
      return value.machines.filter(
        (machine: DiscoveredHand) =>
          typeof machine.id === "string" && typeof machine.mount === "string",
      );
    } catch {
      /* Tool output can be ordinary text. */
    }
  }
  return [];
}
