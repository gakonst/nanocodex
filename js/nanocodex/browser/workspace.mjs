import { createWorkspace, normalizeRelativePath, tools } from "../runtime/workspace.mjs";

const WORKSPACE_DIRECTORY = "nanocodex-workspaces";

export { tools };

export async function open(options = {}) {
  const name = workspaceName(options.name ?? "default");
  const storage = options.storage ?? globalThis.navigator?.storage;
  if (!storage || typeof storage.getDirectory !== "function") {
    throw new Error("Origin Private File System storage is unavailable in this browser");
  }
  const origin = await storage.getDirectory();
  const workspaces = await origin.getDirectoryHandle(WORKSPACE_DIRECTORY, { create: true });
  const directory = await workspaces.getDirectoryHandle(encodeURIComponent(name), { create: true });
  return createWorkspace({
    root: options.root ?? "/workspace",
    backend: opfsBackend(directory),
  });
}

function opfsBackend(root) {
  return {
    async list(path, { recursive, maxEntries }) {
      const directory = await directoryHandle(root, path, false);
      const entries = [];
      await listDirectory(directory, path, recursive, maxEntries, entries);
      return entries.sort((left, right) => left.path.localeCompare(right.path));
    },
    async readFile(path) {
      const { directory, name } = await parentHandle(root, path, false);
      const handle = await directory.getFileHandle(name);
      return new Uint8Array(await (await handle.getFile()).arrayBuffer());
    },
    async writeFile(path, contents) {
      const { directory, name } = await parentHandle(root, path, true);
      const handle = await directory.getFileHandle(name, { create: true });
      const writable = await handle.createWritable();
      try {
        await writable.write(contents);
        await writable.close();
      } catch (error) {
        await writable.abort?.().catch(() => {});
        throw error;
      }
    },
    async remove(path, { recursive }) {
      const { directory, name } = await parentHandle(root, path, false);
      await directory.removeEntry(name, { recursive });
    },
    async mkdir(path) {
      await directoryHandle(root, path, true);
    },
  };
}

async function listDirectory(directory, prefix, recursive, maxEntries, output) {
  for await (const [name, handle] of directory.entries()) {
    if (output.length >= maxEntries) {
      throw new RangeError(`workspace listing exceeds ${maxEntries} entries`);
    }
    const path = prefix ? `${prefix}/${name}` : name;
    if (handle.kind === "directory") {
      output.push({ kind: "directory", path });
      if (recursive) await listDirectory(handle, path, true, maxEntries, output);
      continue;
    }
    if (handle.kind !== "file") throw new Error(`unsupported workspace entry kind: ${handle.kind}`);
    const file = await handle.getFile();
    output.push({
      kind: "file",
      modifiedAt: file.lastModified,
      path,
      size: file.size,
    });
  }
}

async function directoryHandle(root, path, create) {
  let directory = root;
  const relative = normalizeRelativePath(path);
  if (!relative) return directory;
  for (const segment of relative.split("/")) {
    directory = await directory.getDirectoryHandle(segment, { create });
  }
  return directory;
}

async function parentHandle(root, path, create) {
  const relative = normalizeRelativePath(path);
  const segments = relative.split("/");
  const name = segments.pop();
  if (!name) throw new Error("workspace file path cannot be empty");
  return {
    directory: await directoryHandle(root, segments.join("/"), create),
    name,
  };
}

function workspaceName(value) {
  if (typeof value !== "string" || !value.trim()) {
    throw new TypeError("workspace name must be a non-empty string");
  }
  if (value.length > 128) throw new RangeError("workspace name cannot exceed 128 characters");
  if (value.includes("\0")) throw new TypeError("workspace name cannot contain NUL");
  return value;
}
