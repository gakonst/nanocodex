// Diagnostic classification only: never resolve or dispatch a new tool identity.
// Return local constants, never provider names, arguments or registry contents.
export function toolAliasFailure(returnedName, registry) {
  if (typeof returnedName !== "string" || returnedName.length > 512) return "unknown tool alias";
  const entries = [...registry.values()];
  const original = entry => entry.namespace ? `${entry.namespace}.${entry.name}` : entry.name;
  if (entries.filter(entry => returnedName === entry.name || returnedName === original(entry)).length > 1) {
    return "ambiguous original tool alias";
  }
  if (entries.some(entry => entry.namespace && returnedName === `${entry.namespace}.${entry.alias}`)) return "namespaced wire tool alias";
  if (entries.some(entry => entry.namespace && ["_", "__"].some(separator => returnedName === `${entry.namespace}${separator}${entry.name}`))) {
    return "flattened namespace tool alias";
  }
  if (entries.some(entry => [entry.alias, entry.name, original(entry)].some(name => name && returnedName.length > name.length
    && returnedName.length % name.length === 0 && returnedName === name.repeat(returnedName.length / name.length)))) {
    return "repeated tool alias";
  }
  if (/^tool_[0-9]+$/.test(returnedName)) return "unregistered wire tool alias";
  if (returnedName === "multi_tool_use.parallel") return "parallel wrapper tool alias";
  return "unknown tool alias";
}
