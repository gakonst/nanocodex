/** Apply stable managed response policy at the provider wire boundary, including replay. */
export function responseControlsSocket(socket, controls = {}) {
  if (controls.promptCache !== undefined && !["implicit", "explicit"].includes(controls.promptCache)) {
    throw new TypeError("invalid prompt cache mode");
  }
  if (controls.outputSchema !== undefined && (!controls.outputSchema || typeof controls.outputSchema !== "object" || Array.isArray(controls.outputSchema))) {
    throw new TypeError("output schema must be an object");
  }
  return new Proxy({}, {
    get(_target, property) {
      const target = socket;
      if (property === "send") return (data, ...args) => {
        const body = typeof data === "string" ? JSON.parse(data) : undefined;
        if (body?.type === "response.create") {
          if (controls.outputSchema !== undefined) body.text = {
            ...body.text, format: { type: "json_schema", name: "managed_output", strict: true, schema: controls.outputSchema },
          };
          if (controls.promptCache !== undefined) {
            body.prompt_cache_options = { mode: controls.promptCache, ttl: "30m" };
            if (controls.promptCache === "explicit") {
              // A stable developer prefix is the only automatic write boundary.
              // Continuations with no prefix intentionally do not write new cache entries.
              const developers = (Array.isArray(body.input) ? body.input : []).filter(item => item.role === "developer");
              const text = developers.flatMap(item => Array.isArray(item.content) ? item.content : [])
                .filter(part => part.type === "input_text").at(-1);
              if (text) text.prompt_cache_breakpoint = { mode: "explicit" };
            }
          }
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
