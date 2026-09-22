/** Apply stable managed response policy at the provider wire boundary, including replay. */
export function responseControlsSocket(socket, controls = {}) {
  validateResponseControls(controls);
  if (!hasResponseControls(controls)) return socket;
  return new Proxy({}, {
    get(_target, property) {
      const target = socket;
      if (property === "send") return (data, ...args) => {
        const body = typeof data === "string" ? JSON.parse(data) : undefined;
        if (body?.type === "response.create") {
          applyResponseControls(body, controls);
          return target.send(JSON.stringify(body), ...args);
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
