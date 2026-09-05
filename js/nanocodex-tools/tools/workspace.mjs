const DEFAULT_MAX_READ_BYTES = 1024 * 1024;
const DEFAULT_MAX_WRITE_BYTES = 4 * 1024 * 1024;
const DEFAULT_MAX_ENTRIES = 2_000;

export function createWorkspace({ backend, root = "/workspace" }) {
  if (!backend || typeof backend !== "object") {
    throw new TypeError("workspace backend is required");
  }
  for (const method of ["list", "readFile", "writeFile", "remove", "mkdir"]) {
    if (typeof backend[method] !== "function") {
      throw new TypeError(`workspace backend requires ${method}()`);
    }
  }
  const normalizedRoot = normalizeRoot(root);

  return Object.freeze({
    root: normalizedRoot,
    async list(path = ".", options = {}) {
      const relative = resolveWorkspacePath(normalizedRoot, path);
      const entries = await backend.list(relative, {
        recursive: options.recursive === true,
        maxEntries: positiveInteger(options.maxEntries, DEFAULT_MAX_ENTRIES, "maxEntries"),
      });
      return entries.map((entry) => Object.freeze({
        ...entry,
        path: displayPath(normalizedRoot, normalizeRelativePath(entry.path)),
      }));
    },
    async readFile(path) {
      return toBytes(await backend.readFile(resolveWorkspacePath(normalizedRoot, path)));
    },
    async writeFile(path, contents) {
      const relative = resolveWorkspacePath(normalizedRoot, path);
      if (!relative) throw new Error("cannot write the workspace root as a file");
      await backend.writeFile(relative, toBytes(contents));
    },
    async remove(path, options = {}) {
      const relative = resolveWorkspacePath(normalizedRoot, path);
      if (!relative) throw new Error("cannot remove the workspace root");
      await backend.remove(relative, { recursive: options.recursive === true });
    },
    async mkdir(path) {
      await backend.mkdir(resolveWorkspacePath(normalizedRoot, path));
    },
  });
}

export function tools(workspace, options = {}) {
  validateWorkspace(workspace);
  const maxReadBytes = positiveInteger(
    options.maxReadBytes,
    DEFAULT_MAX_READ_BYTES,
    "maxReadBytes",
  );
  const maxWriteBytes = positiveInteger(
    options.maxWriteBytes,
    DEFAULT_MAX_WRITE_BYTES,
    "maxWriteBytes",
  );
  const maxEntries = positiveInteger(
    options.maxEntries,
    DEFAULT_MAX_ENTRIES,
    "maxEntries",
  );
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();

  return Object.freeze({
    list_files: {
      description: "List files and directories in the kernel workspace.",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string", description: "Workspace-relative or absolute path." },
          recursive: { type: "boolean", description: "Recursively list descendants." },
        },
        additionalProperties: false,
      },
      async handler(input = {}) {
        const value = objectInput(input, "list_files");
        const entries = await workspace.list(optionalString(value.path, ".", "path"), {
          recursive: optionalBoolean(value.recursive, false, "recursive"),
          maxEntries,
        });
        return { entries, root: workspace.root };
      },
    },
    read_file: {
      description: "Read UTF-8 text from a file in the kernel workspace.",
      parameters: {
        type: "object",
        required: ["path"],
        properties: {
          path: { type: "string", description: "Workspace-relative or absolute file path." },
          offset: { type: "integer", minimum: 0, description: "Byte offset to begin reading." },
          limit: { type: "integer", minimum: 1, description: "Maximum bytes to return." },
        },
        additionalProperties: false,
      },
      async handler(input) {
        const value = objectInput(input, "read_file");
        const path = requiredString(value.path, "path");
        const offset = nonNegativeInteger(value.offset, 0, "offset");
        const limit = positiveInteger(value.limit, maxReadBytes, "limit");
        if (limit > maxReadBytes) {
          throw new RangeError(`limit exceeds the ${maxReadBytes}-byte workspace read bound`);
        }
        const contents = await workspace.readFile(path);
        const end = Math.min(contents.byteLength, offset + limit);
        return {
          path: displayPath(workspace.root, resolveWorkspacePath(workspace.root, path)),
          content: decoder.decode(contents.subarray(offset, end)),
          size: contents.byteLength,
          offset,
          truncated: end < contents.byteLength,
        };
      },
    },
    write_file: {
      description: "Create or replace a UTF-8 text file in the kernel workspace.",
      parameters: {
        type: "object",
        required: ["path", "content"],
        properties: {
          path: { type: "string", description: "Workspace-relative or absolute file path." },
          content: { type: "string", description: "Complete UTF-8 file contents." },
        },
        additionalProperties: false,
      },
      async handler(input) {
        const value = objectInput(input, "write_file");
        const path = requiredString(value.path, "path");
        const content = requiredString(value.content, "content", { allowEmpty: true });
        const contents = encoder.encode(content);
        if (contents.byteLength > maxWriteBytes) {
          throw new RangeError(`file exceeds the ${maxWriteBytes}-byte workspace write bound`);
        }
        await workspace.writeFile(path, contents);
        return {
          path: displayPath(workspace.root, resolveWorkspacePath(workspace.root, path)),
          bytesWritten: contents.byteLength,
        };
      },
    },
    make_directory: {
      description: "Create a directory and any missing parents in the kernel workspace.",
      parameters: {
        type: "object",
        required: ["path"],
        properties: {
          path: { type: "string", description: "Workspace-relative or absolute directory path." },
        },
        additionalProperties: false,
      },
      async handler(input) {
        const value = objectInput(input, "make_directory");
        const path = requiredString(value.path, "path");
        await workspace.mkdir(path);
        return { path: displayPath(workspace.root, resolveWorkspacePath(workspace.root, path)) };
      },
    },
    delete_file: {
      description: "Delete a file or directory from the kernel workspace.",
      parameters: {
        type: "object",
        required: ["path"],
        properties: {
          path: { type: "string", description: "Workspace-relative or absolute path." },
          recursive: { type: "boolean", description: "Allow deleting a non-empty directory." },
        },
        additionalProperties: false,
      },
      async handler(input) {
        const value = objectInput(input, "delete_file");
        const path = requiredString(value.path, "path");
        await workspace.remove(path, {
          recursive: optionalBoolean(value.recursive, false, "recursive"),
        });
        return { path: displayPath(workspace.root, resolveWorkspacePath(workspace.root, path)) };
      },
    },
  });
}

export function normalizeRelativePath(path) {
  if (typeof path !== "string") throw new TypeError("workspace path must be a string");
  if (path.includes("\0")) throw new TypeError("workspace path cannot contain NUL");
  if (path.includes("\\")) throw new TypeError("workspace paths must use forward slashes");
  const segments = [];
  for (const segment of path.split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") throw new Error("workspace path cannot escape its root");
    segments.push(segment);
  }
  return segments.join("/");
}

export function resolveWorkspacePath(root, path) {
  const normalizedRoot = normalizeRoot(root);
  if (typeof path !== "string") throw new TypeError("workspace path must be a string");
  if (path.startsWith("/")) {
    if (path !== normalizedRoot && !path.startsWith(`${normalizedRoot}/`)) {
      throw new Error(`workspace path must stay within ${normalizedRoot}`);
    }
    path = path.slice(normalizedRoot.length);
  }
  return normalizeRelativePath(path);
}

function normalizeRoot(root) {
  if (typeof root !== "string" || !root.startsWith("/")) {
    throw new TypeError("workspace root must be an absolute path");
  }
  const normalized = `/${normalizeRelativePath(root)}`;
  if (normalized === "/") throw new TypeError("workspace root cannot be the filesystem root");
  return normalized;
}

function displayPath(root, relative) {
  return relative ? `${root}/${relative}` : root;
}

function toBytes(value) {
  if (typeof value === "string") return new TextEncoder().encode(value);
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new TypeError("workspace file contents must be a string or byte array");
}

function validateWorkspace(workspace) {
  if (!workspace || typeof workspace !== "object" || typeof workspace.root !== "string") {
    throw new TypeError("a workspace handle is required");
  }
  for (const method of ["list", "readFile", "writeFile", "remove", "mkdir"]) {
    if (typeof workspace[method] !== "function") {
      throw new TypeError(`workspace handle requires ${method}()`);
    }
  }
}

function objectInput(value, tool) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${tool} input must be an object`);
  }
  return value;
}

function requiredString(value, name, { allowEmpty = false } = {}) {
  if (typeof value !== "string" || (!allowEmpty && !value)) {
    throw new TypeError(`${name} must be ${allowEmpty ? "a string" : "a non-empty string"}`);
  }
  return value;
}

function optionalString(value, fallback, name) {
  return value === undefined ? fallback : requiredString(value, name);
}

function optionalBoolean(value, fallback, name) {
  if (value === undefined) return fallback;
  if (typeof value !== "boolean") throw new TypeError(`${name} must be a boolean`);
  return value;
}

function nonNegativeInteger(value, fallback, name) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`${name} must be a non-negative integer`);
  }
  return value;
}

function positiveInteger(value, fallback, name) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new TypeError(`${name} must be a positive integer`);
  }
  return value;
}
