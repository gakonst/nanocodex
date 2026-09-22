// Nanocodex extension to Codex's known-tool object: missing names reject locally.
// Keep this factory self-contained: QuickJS and Rust embed its source verbatim.
// Object methods avoid bundler-injected naming helpers when keepNames is enabled.
export function createCodeTools(names, invoke) {
  const helpers = {
    // Observe discarded calls without changing the returned promise's outcome.
    observe(promise) {
      promise.catch(() => {});
      return promise;
    },
    async dispatch(name, input) {
      return invoke(name, input);
    },
    async unavailable(name) {
      const error = new Error(`TOOL_NOT_AVAILABLE: Tool "${name}" is not available in this Code Mode execution. Check ALL_TOOLS for callable names. A tool exposed directly by the host may need to be called through its direct tool entry instead.`);
      error.code = "TOOL_NOT_AVAILABLE";
      error.tool = name;
      throw error;
    },
  };
  const target = Object.create(null);
  for (const name of names) {
    Object.defineProperty(target, name, {
      enumerable: true,
      value(input) {
        return helpers.observe(helpers.dispatch(name, input));
      },
    });
  }
  Object.freeze(target);
  return new Proxy(target, {
    get(target, name) {
      if (Object.prototype.hasOwnProperty.call(target, name)) return target[name];
      // An absent then must not make the facade a thenable. Symbols are ordinary
      // absent properties, including inspection and iteration protocols.
      if (typeof name !== "string" || name === "then") return undefined;
      return {
        missing() {
          return helpers.observe(helpers.unavailable(name));
        },
      }.missing;
    },
  });
}
