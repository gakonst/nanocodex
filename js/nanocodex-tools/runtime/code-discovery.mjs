// Self-contained so native and WASM QuickJS can embed the same factory.
// Object methods avoid bundler-injected naming helpers with keepNames enabled.
export function createCodeDiscovery(definitions = []) {
  const helpers = {
    clone(value) {
      return JSON.parse(JSON.stringify(value));
    },
    freeze(value) {
      if (value && typeof value === "object") {
        for (const entry of Object.values(value)) helpers.freeze(entry);
        Object.freeze(value);
      }
      return value;
    },
    normalize(name) {
      return [...name].map((character, index) =>
        (index === 0 ? /[A-Za-z_$]/ : /[A-Za-z0-9_$]/).test(character) ? character : "_"
      ).join("") || "_";
    },
    namespace(name) {
      return name.includes("__") ? name.split("__")[0]
        : name.includes(".") ? name.split(".")[0] : "";
    },
    requireString(value, label) {
      if (typeof value !== "string" || !value.trim()) throw new TypeError(`${label} expects a non-empty string`);
      return value;
    },
  };
  const catalog = helpers.freeze(helpers.clone(definitions).map((definition) => {
    const name = definition.tool_name ?? definition.name;
    return {
      name,
      callableName: helpers.normalize(name),
      description: definition.description ?? "",
      inputSchema: definition.input_schema ?? definition.parameters ?? null,
      outputSchema: definition.output_schema ?? null,
      kind: definition.kind ?? (definition.type === "custom" ? "freeform" : "function"),
    };
  }));
  return Object.freeze({
    searchTools(query, options = {}) {
      helpers.requireString(query, "searchTools");
      const limit = options.limit ?? 10;
      if (!Number.isInteger(limit) || limit < 1 || limit > 100) {
        throw new RangeError("searchTools limit must be an integer between 1 and 100");
      }
      const terms = query.toLowerCase().trim().split(/\s+/);
      return helpers.freeze(catalog.filter((tool) => {
        const text = `${tool.name} ${tool.callableName} ${tool.description}`.toLowerCase();
        return terms.every((term) => text.includes(term));
      }).slice(0, limit).map(({ name, callableName, description }) => ({ name, callableName, description })));
    },
    describeTool(name) {
      helpers.requireString(name, "describeTool");
      return catalog.find((tool) => tool.name === name || tool.callableName === name);
    },
    describeNamespace(name) {
      helpers.requireString(name, "describeNamespace");
      return helpers.freeze(catalog.filter((tool) => helpers.namespace(tool.name) === name));
    },
  });
}
