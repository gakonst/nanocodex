/** Apply stable managed response policy at the provider wire boundary, including replay. */
export function responseControlsSocket(socket, controls = {}, onRequestShape, onResponseCreateSent) {
  validateResponseControls(controls);
  if (onRequestShape !== undefined && typeof onRequestShape !== "function") {
    throw new TypeError("request shape observer must be a function");
  }
  if (onResponseCreateSent !== undefined && typeof onResponseCreateSent !== "function") {
    throw new TypeError("response.create sent observer must be a function");
  }
  const controlled = hasResponseControls(controls);
  if (!controlled && onRequestShape === undefined && onResponseCreateSent === undefined) return socket;
  let remainingObservations = 32;
  return new Proxy({}, {
    get(_target, property) {
      const target = socket;
      if (property === "send") return (data, ...args) => {
        let body;
        if (typeof data === "string" && (controlled || (onRequestShape !== undefined && remainingObservations > 0) || onResponseCreateSent !== undefined)) {
          // Reuse the policy parse. Observation alone never rejects an opaque frame.
          if (controlled) body = JSON.parse(data);
          else { try { body = JSON.parse(data); } catch { /* Pass through unchanged. */ } }
        }
        if (body?.type === "response.create") {
          if (controlled) applyResponseControls(body, controls);
          const encoded = controlled ? JSON.stringify(body) : data;
          const result = target.send(encoded, ...args);
          if (onResponseCreateSent !== undefined) {
            // Every model request, not the bounded shape-sampling subset. The
            // callback receives no request body, ID, headers, or credentials.
            try {
              const observation = onResponseCreateSent();
              if (observation?.then) void Promise.resolve(observation).catch(() => {});
            } catch { /* Passive timing cannot change transport success. */ }
          }
          if (onRequestShape !== undefined && remainingObservations > 0) {
            remainingObservations -= 1;
            // Send first; diagnostics retain only fixed enums, booleans and counts.
            try {
              const observation = onRequestShape(responseRequestShape(body, encoded.length));
              if (observation?.then) void Promise.resolve(observation).catch(() => {});
            } catch { /* Observation cannot change transport success. */ }
          }
          return result;
        }
        return target.send(data, ...args);
      };
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
    set(_target, property, value) { return Reflect.set(socket, property, value, socket); },
  });
}

/** Apply the same policy to an HTTPS Responses body before provider dispatch. */
export function responseControlsBody(encoded, controls = {}) {
  validateResponseControls(controls);
  if (!hasResponseControls(controls)) return encoded;
  const body = JSON.parse(encoded);
  if (!body || typeof body !== "object" || Array.isArray(body)) throw new TypeError("response body must be an object");
  applyResponseControls(body, controls);
  return JSON.stringify(body);
}

function hasResponseControls(controls) {
  return controls.promptCacheKey !== undefined
    || controls.outputSchema !== undefined
    || controls.promptCache !== undefined;
}

function validateResponseControls(controls) {
  if (controls.promptCacheKey !== undefined && (typeof controls.promptCacheKey !== "string" || controls.promptCacheKey.length === 0 || controls.promptCacheKey.length > 64)) {
    throw new TypeError("invalid prompt cache key");
  }
  if (controls.promptCache !== undefined && !["implicit", "explicit"].includes(controls.promptCache)) {
    throw new TypeError("invalid prompt cache mode");
  }
  if (controls.outputSchema !== undefined && (!controls.outputSchema || typeof controls.outputSchema !== "object" || Array.isArray(controls.outputSchema))) {
    throw new TypeError("output schema must be an object");
  }
}

function applyResponseControls(body, controls) {
  if (controls.promptCacheKey !== undefined) body.prompt_cache_key = controls.promptCacheKey;
  if (controls.outputSchema !== undefined) body.text = {
    ...body.text, format: { type: "json_schema", name: "managed_output", strict: true, schema: controls.outputSchema },
  };
  if (controls.promptCache !== undefined) {
    body.prompt_cache_options = { mode: controls.promptCache, ttl: "30m" };
    if (controls.promptCache === "explicit") {
      // Cache the developer context without marking changing user input.
      // Continuations with no prefix intentionally do not write new cache entries.
      const developers = (Array.isArray(body.input) ? body.input : []).filter(item => item.role === "developer");
      const text = developers.flatMap(item => Array.isArray(item.content) ? item.content : [])
        .filter(part => part.type === "input_text").at(-1);
      if (text) text.prompt_cache_breakpoint = { mode: "explicit" };
    }
  }
}

// Deliberately omit instructions, tool names/schemas, input, metadata and all IDs.
// The observation describes post-policy controls; multiplex framing may omit stream.
function responseRequestShape(body, encodedLength) {
  const choose = (value, allowed) => allowed.includes(value) ? value : "other_or_absent";
  const shape = {
    model: choose(body.model, ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna"]),
    reasoning_effort: choose(body.reasoning?.effort, ["none", "minimal", "low", "medium", "high", "xhigh", "max"]),
    reasoning_context: choose(body.reasoning?.context, ["all_turns", "last_turn"]),
    service_tier: choose(body.service_tier, ["default", "auto", "priority", "flex", "fast"]),
    text_verbosity: choose(body.text?.verbosity, ["low", "medium", "high"]),
    tool_choice: choose(body.tool_choice, ["auto", "none", "required"]),
    encoded_characters: encodedLength,
    input_items: Array.isArray(body.input) ? body.input.length : 0,
    tools_count: Array.isArray(body.tools) ? body.tools.length : 0,
    cache_key_present: typeof body.prompt_cache_key === "string",
    previous_response_present: typeof body.previous_response_id === "string",
    encrypted_reasoning_included: Array.isArray(body.include) && body.include.includes("reasoning.encrypted_content"),
  };
  for (const key of ["parallel_tool_calls", "store", "stream", "generate"]) {
    if (typeof body[key] === "boolean") shape[key] = body[key];
  }
  return Object.freeze(shape);
}
