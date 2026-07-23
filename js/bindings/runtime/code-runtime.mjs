export function createCodeRuntime(toolConfiguration = {}, extras = {}) {
  const stores = new Map();
  let nextCallId = 1;
  const definitions = [];

  for (const [name, tool] of Object.entries(toolConfiguration)) {
    if (!tool || typeof tool.handler !== "function") {
      throw new TypeError(`tool ${name} requires a handler function`);
    }
    definitions.push({
      type: "function",
      name,
      description: tool.description || "Application-defined tool.",
      strict: false,
      parameters: tool.parameters || {
        type: "object",
        additionalProperties: true,
      },
    });
  }
  Object.freeze(definitions);

  async function executeCode(source, sessionId = "default", parentCallId = "exec") {
    const startedAt = performance.now();
    const content = [];
    const stored = stores.get(sessionId) || new Map();
    stores.set(sessionId, stored);
    const nestedCalls = [];
    const tools = Object.create(null);
    for (const [name, tool] of Object.entries(toolConfiguration)) {
      tools[name] = async (input) => {
        const callId = `${parentCallId}/code-${nextCallId++}`;
        const toolStartedAt = performance.now();
        const startedAfterNs = Math.max(
          0,
          Math.round((toolStartedAt - startedAt) * 1_000_000),
        );
        try {
          const result = await tool.handler(input, { sessionId, parentCallId, callId });
          nestedCalls.push({
            call_id: callId,
            name,
            // JSON.stringify omits object fields whose value is undefined.
            // Keep zero-argument calls wire-complete for Rust's typed event.
            input: clone(input) ?? null,
            output: outputBody(result),
            success: true,
            started_after_ns: startedAfterNs,
            duration_ns: elapsedNs(toolStartedAt),
          });
          return result;
        } catch (error) {
          nestedCalls.push({
            call_id: callId,
            name,
            input: clone(input) ?? null,
            output: errorMessage(error),
            success: false,
            started_after_ns: startedAfterNs,
            duration_ns: elapsedNs(toolStartedAt),
          });
          throw error;
        }
      };
    }
    Object.freeze(tools);
    const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
    const EXIT = Symbol("exit");

    function text(value) {
      content.push({ type: "input_text", text: stringify(value) });
    }
    function image(value, detail = "auto") {
      if (typeof value === "string") {
        content.push({ type: "input_image", image_url: value, detail });
        return;
      }
      if (!value || typeof value !== "object" || typeof value.image_url !== "string") {
        throw new TypeError("image() requires an image URL or image item");
      }
      content.push({
        type: "input_image",
        image_url: value.image_url,
        detail: value.detail == null ? detail : value.detail,
      });
    }
    function generatedImage(result) {
      if (!result || typeof result !== "object" || typeof result.image_url !== "string") {
        throw new TypeError("generatedImage() requires an image generation result");
      }
      image(result.image_url, "high");
      if (typeof result.output_hint === "string" && result.output_hint) text(result.output_hint);
    }
    function store(key, value) {
      if (typeof key !== "string") throw new TypeError("store key must be a string");
      stored.set(key, clone(value));
    }
    function load(key) {
      return stored.has(key) ? clone(stored.get(key)) : undefined;
    }
    function exit() {
      throw EXIT;
    }

    try {
      const script = new AsyncFunction(
        "tools",
        "ALL_TOOLS",
        "text",
        "image",
        "generatedImage",
        "store",
        "load",
        "exit",
        "require",
        "console",
        source,
      );
      try {
        await script(
          tools,
          definitions,
          text,
          image,
          generatedImage,
          store,
          load,
          exit,
          extras.require,
          extras.console || console,
        );
      } catch (error) {
        if (error !== EXIT) throw error;
      }
      return JSON.stringify({
        output: withStatus("Script completed", startedAt, content),
        success: true,
        nested_calls: nestedCalls,
      });
    } catch (error) {
      return JSON.stringify({
        output: `Script failed\nWall time ${wallTime(startedAt)} seconds\nOutput:\n${errorMessage(error)}`,
        success: false,
        nested_calls: nestedCalls,
      });
    }
  }

  return Object.freeze({
    executeCode,
    toolDefinitions: () => JSON.stringify(definitions),
    reset() {
      stores.clear();
    },
  });
}

function outputBody(value) {
  if (Array.isArray(value) && value.every((item) => item?.type === "input_text" || item?.type === "input_image")) {
    return value;
  }
  return stringify(value);
}

function stringify(value) {
  if (typeof value === "string") return value;
  if (value === undefined) return "undefined";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function clone(value) {
  if (typeof globalThis.structuredClone === "function") return structuredClone(value);
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
}

function errorMessage(error) {
  if (error && (error.stack || error.message)) return error.stack || error.message;
  return String(error);
}

function elapsedNs(startedAt) {
  return Math.max(0, Math.round((performance.now() - startedAt) * 1_000_000));
}

function wallTime(startedAt) {
  return ((performance.now() - startedAt) / 1_000).toFixed(1);
}

function withStatus(status, startedAt, content) {
  const heading = `${status}\nWall time ${wallTime(startedAt)} seconds\nOutput:\n`;
  if (!content.length) return heading;
  return [{ type: "input_text", text: heading }, ...content];
}
