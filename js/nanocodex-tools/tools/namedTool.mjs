export function namedTool(name, tool) {
  return Object.freeze({ name, ...tool });
}
