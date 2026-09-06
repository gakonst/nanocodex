// Context files share the host's existing workspace and persistence boundary.
export function historyNotesHost(workspace) {
  let resolved;
  const resolve = () => resolved ??= Promise.resolve(typeof workspace === "function" ? workspace() : workspace);
  return Object.freeze({
    async capability() { return Boolean(await resolve()); },
    async request(_threadId, encoded) {
      const files = await resolve();
      if (!files) throw new Error("Context workspace is unavailable");
      const operation = JSON.parse(encoded);
      let result;
      switch (operation.operation) {
        case "read":
          try { result = new TextDecoder().decode(await files.readFile(operation.path)); }
          catch (error) { if (error?.code !== "ENOENT") throw error; result = null; }
          break;
        case "write":
          await files.writeFile(operation.path, operation.contents);
          result = null;
          break;
        case "list":
          try {
            result = (await files.list(operation.path)).filter((entry) => entry.kind === "file" && entry.path.endsWith(".json"))
              .map((entry) => entry.path.slice(files.root.replace(/\/$/, "").length + 1));
          } catch (error) { if (error?.code !== "ENOENT") throw error; result = []; }
          break;
        default: throw new Error("Unknown context storage operation");
      }
      return JSON.stringify(result);
    },
  });
}
