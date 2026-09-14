import type { HostedToolsCatalogCandidate } from "nanocodex-tools/hosted";

export type ScreenAction = {
  action: "observe" | "click" | "type" | "key" | "scroll" | "drag" | "release";
  x?: number; y?: number; endX?: number; endY?: number; button?: number;
  text?: string; key?: number; modifiers?: number[];
  deltaX?: number; deltaY?: number; durationMs?: number;
};
export type AgentScreenResult = {
  status: "ok" | "busy" | "invalid" | "unavailable" | "cancelled";
  jpeg?: string; width?: number; height?: number;
};
export type ScreenTool = HostedToolsCatalogCandidate & { route_token: string };
export type ScreenTarget = { machine_id: string; machine_name: string; id: string; name: string;
  kind: string; generation: string; width: number; height: number; controllable: boolean; agent_tools?: boolean };

export function screenTool(target: ScreenTarget): ScreenTool {
  // Stable discovery name, immutable invocation route. Re-publication never
  // silently redirects a tool admitted against a previous sharing session.
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(JSON.stringify([target.machine_id, target.id]))) {
    hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
  }
  return {
    provider: "screens", remote_name: target.id, parallel_safe: false, timeout_ms: 10_000,
    route_token: "screen:v1:" + JSON.stringify([target.machine_id, target.id, target.generation]),
    summary: `See and control ${target.machine_name} · ${target.name} (${target.kind}).`,
    definition: { type: "function", name: "screen_" + hash.toString(16), strict: false, defer_loading: true,
      description: `Live screen of ${target.machine_name} · ${target.name} (${target.kind}, ${target.width}×${target.height}). `
        + "Observe returns a current screenshot. Input actions return a screenshot after applying input. "
        + "In Code Mode, emit the returned image_url with image(result) to see it; use text(result) for errors. "
        + "Coordinates x/y/endX/endY are normalized from 0 to 1 across the whole image. "
        + "Human takeover has priority: busy means stop sending input until the human releases control. "
        + "Use key with USB HID usage (Return 40, Escape 41, Backspace 42, Tab 43, Home 74); "
        + "modifiers are held only for that key (Control 224, Shift 225, Alt 226, Command 227). "
        + "Paired iPhone supports click, drag, scroll, text, Return, Backspace, and Home. "
        + "Do not retry ambiguous input automatically; observe its effect first.",
      parameters: { type: "object", additionalProperties: false, required: ["action"], properties: {
        action: { type: "string", enum: ["observe", "click", "type", "key", "scroll", "drag", "release"] },
        x: { type: "number", minimum: 0, maximum: 1 }, y: { type: "number", minimum: 0, maximum: 1 },
        endX: { type: "number", minimum: 0, maximum: 1 }, endY: { type: "number", minimum: 0, maximum: 1 },
        button: { type: "integer", minimum: 0, maximum: 2, description: "0 left/tap, 1 right/long press, 2 middle" },
        text: { type: "string", maxLength: 4096 }, key: { type: "integer", minimum: 4, maximum: 231 },
        modifiers: { type: "array", maxItems: 4, uniqueItems: true, items: { type: "integer", minimum: 224, maximum: 231 } },
        deltaX: { type: "number", minimum: -4096, maximum: 4096 }, deltaY: { type: "number", minimum: -4096, maximum: 4096 },
        durationMs: { type: "integer", minimum: 50, maximum: 1500 },
      } },
      output_schema: { type: "object", properties: { status: { type: "string" }, message: { type: "string" },
        image_url: { type: "string" }, detail: { type: "string" }, width: { type: "integer" }, height: { type: "integer" },
        machine_id: { type: "string" }, surface_id: { type: "string" } }, required: ["status", "message", "machine_id", "surface_id"] },
    },
  };
}

export function screenAction(value: unknown): ScreenAction {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid screen action");
  const v = value as Record<string, unknown>;
  const fields: Record<ScreenAction["action"], string[]> = {
    observe: [], release: [], click: ["x", "y", "button"], type: ["text"], key: ["key", "modifiers"],
    scroll: ["x", "y", "deltaX", "deltaY"], drag: ["x", "y", "endX", "endY", "durationMs"],
  };
  if (typeof v.action !== "string" || !Object.hasOwn(fields, v.action)
    || Object.keys(v).some(key => key !== "action" && !fields[v.action as ScreenAction["action"]].includes(key))) throw new Error("Invalid screen action");
  const number = (key: string, min: number, max: number) => typeof v[key] === "number" && Number.isFinite(v[key]) && v[key] >= min && v[key] <= max;
  if (["click", "scroll", "drag"].includes(v.action) && (!number("x", 0, 1) || !number("y", 0, 1))) throw new Error("Invalid point");
  if (v.action === "click" && v.button !== undefined && (!Number.isInteger(v.button) || !number("button", 0, 2))) throw new Error("Invalid button");
  if (v.action === "type" && (typeof v.text !== "string" || !v.text || v.text.includes("\0") || new TextEncoder().encode(v.text).length > 4096)) throw new Error("Invalid text");
  if (v.action === "key" && (!Number.isInteger(v.key) || !number("key", 4, 231)
    || (v.modifiers !== undefined && (!Array.isArray(v.modifiers) || v.modifiers.length > 4
      || new Set(v.modifiers).size !== v.modifiers.length || v.modifiers.some(key => !Number.isInteger(key) || key < 224 || key > 231))))) throw new Error("Invalid key");
  if (v.action === "scroll" && (!number("deltaX", -4096, 4096) || !number("deltaY", -4096, 4096))) throw new Error("Invalid scroll");
  if (v.action === "drag" && (!number("endX", 0, 1) || !number("endY", 0, 1)
    || (v.durationMs !== undefined && (!Number.isInteger(v.durationMs) || !number("durationMs", 50, 1500))))) throw new Error("Invalid drag");
  return v as ScreenAction;
}

export function screenResult(result: AgentScreenResult, target: ScreenTarget) {
  const messages = { ok: "Screen action completed.", busy: "A human or another agent controls this screen. Stop input until they release control.",
    invalid: "Unsupported or invalid screen action.", unavailable: "Screen outcome is unknown. Observe before considering another input action.",
    cancelled: "Screen action was interrupted. Observe before considering another input action." };
  const value = { status: result.status, message: messages[result.status], machine_id: target.machine_id, surface_id: target.id,
    ...(result.jpeg ? { image_url: "data:image/jpeg;base64," + result.jpeg, detail: "original", width: result.width, height: result.height } : {}) };
  return { output: [
    { type: "input_text", text: messages[result.status] },
    ...(result.jpeg ? [{ type: "input_image", image_url: "data:image/jpeg;base64," + result.jpeg, detail: "original" }] : []),
  ], structured_result: value, success: result.status === "ok",
  metadata: { machine_id: target.machine_id, machine_name: target.machine_name, tool_name: "screen" }, value };
}
