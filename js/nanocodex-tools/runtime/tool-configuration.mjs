import { toolRouterBrand, toolRouterRuntime } from "./tool-router.mjs";

export const subagentsBrand = Symbol("nanocodex.subagents");
export const defaultSubagentMaxConcurrency = 32;

const DEFAULT_SUBAGENTS = Object.freeze({
  max_concurrency: defaultSubagentMaxConcurrency,
});

export function resolveTools(configuration, { defaultSubagents = true } = {}) {
  const subagentsByDefault = defaultSubagents ? DEFAULT_SUBAGENTS : undefined;
  if (configuration === undefined) {
    return { tools: {}, subagents: subagentsByDefault };
  }
  if (!Array.isArray(configuration)) {
    if (!configuration || typeof configuration !== "object") {
      throw new TypeError("tools must be a tool map or an array of named tools");
    }
    const capabilityLike = typeof configuration.attach === "function"
      || typeof configuration.close === "function";
    if (capabilityLike
      && (!configuration[toolRouterBrand]
        || !configuration[toolRouterRuntime]
        || typeof configuration[toolRouterRuntime].execute !== "function")) {
      throw new TypeError("tools capability was not created by createTools()");
    }
    return { tools: configuration, subagents: subagentsByDefault };
  }
  const tools = {};
  let subagents = subagentsByDefault;
  let configuredSubagents = false;
  for (const entry of configuration) {
    const extension = entry?.[subagentsBrand];
    if (extension) {
      if (configuredSubagents) throw new Error("Subagents.create() may only be included once");
      configuredSubagents = true;
      subagents = Object.freeze({ max_concurrency: extension.maxConcurrency });
      continue;
    }
    if (!entry || typeof entry !== "object" || Array.isArray(entry)
        || typeof entry.name !== "string" || !entry.name.trim()) {
      throw new TypeError("tool arrays require named tools or entries from Subagents.create()");
    }
    const { name, ...tool } = entry;
    if (Object.hasOwn(tools, name)) throw new Error(`tool is already configured: ${name}`);
    tools[name] = tool;
  }
  return { tools, subagents };
}
