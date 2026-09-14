import { resolveNamespaceCwd, type Workspace, type WorkspaceEntry } from "nanocodex-tools";

const ROOT = "/brain";

/** The same prefix that Sandbox mounts at /brain; no copy or hand is needed. */
export function createBrainWorkspace(bucket: R2Bucket, resourceId: string): Workspace {
  if (!/^[A-Za-z0-9._:-]{1,256}$/.test(resourceId)) {
    throw new TypeError("brain workspace has an invalid resource id");
  }
  const prefix = `brains/${resourceId}/`;
  // Just Bash refreshes the complete namespace before each command. Reuse that
  // snapshot for metadata probes during the command; Git otherwise turns every
  // stat/mkdir into repeated R2 HEAD/LIST calls. File contents stay in R2.
  let snapshot: Map<string, WorkspaceEntry> | undefined;
  let markers = new Set<string>();
  const resolve = (path: string): string => {
    const absolute = resolveNamespaceCwd(ROOT, path);
    if (absolute !== ROOT && !absolute.startsWith(`${ROOT}/`)) {
      throw error("EPERM", "workspace path must stay within /brain");
    }
    return absolute;
  };
  const key = (path: string): string => `${prefix}${path.slice(ROOT.length + 1)}`;
  const stat = async (path: string): Promise<"file" | "directory" | undefined> => {
    if (path === ROOT) return "directory";
    if (snapshot) return snapshot.get(path)?.kind;
    if (await bucket.head(key(path))) return "file";
    const page = await bucket.list({ prefix: `${key(path)}/`, limit: 1 });
    return page.objects.length ? "directory" : undefined;
  };
  const ancestors = async (path: string): Promise<void> => {
    let current = ROOT;
    for (const segment of path.slice(ROOT.length + 1).split("/").slice(0, -1)) {
      current += `/${segment}`;
      if (snapshot ? snapshot.get(current)?.kind === "file" : await bucket.head(key(current))) {
        throw error("ENOTDIR", `${current} is not a directory`);
      }
    }
  };
  const mkdir = async (path: string): Promise<void> => {
    const absolute = resolve(path);
    if (absolute === ROOT) return;
    await ancestors(absolute);
    if (await stat(absolute) === "file") throw error("EEXIST", `${absolute} is a file`);
    // Keep empty ancestors after their last child is removed. Trailing-slash
    // markers are also understood by the native s3fs mount.
    let current = ROOT;
    for (const segment of absolute.slice(ROOT.length + 1).split("/")) {
      current += `/${segment}`;
      if (snapshot ? !markers.has(current) : !await bucket.head(`${key(current)}/`)) {
        await bucket.put(`${key(current)}/`, new Uint8Array(), {
          httpMetadata: { contentType: "application/x-directory" },
        });
      }
      markers.add(current);
      snapshot?.set(current, { path: current, kind: "directory" });
    }
  };
  const list: Workspace["list"] = async (path = ".", options = {}) => {
    const absolute = resolve(path);
    const refresh = absolute === ROOT && options.recursive === true;
    if (refresh) {
      snapshot = undefined;
      markers = new Set();
    }
    await ancestors(absolute);
    const kind = await stat(absolute);
    if (kind !== "directory") throw error(kind === "file" ? "ENOTDIR" : "ENOENT", `${absolute} is not a directory`);
    const maximum = options.maxEntries ?? Infinity;
    if (options.maxEntries !== undefined && (!Number.isSafeInteger(maximum) || maximum < 1)) {
      throw new RangeError("workspace listing limit must be a positive integer");
    }
    if (snapshot) {
      const children = [...snapshot.values()].filter((entry) => {
        if (!entry.path.startsWith(`${absolute}/`)) return false;
        return options.recursive || !entry.path.slice(absolute.length + 1).includes("/");
      });
      if (children.length > maximum) throw new RangeError(`workspace listing exceeds ${maximum} entries`);
      return children.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
    }
    const entries = new Map<string, WorkspaceEntry>();
    const directoryPrefix = absolute === ROOT ? prefix : `${key(absolute)}/`;
    let cursor: string | undefined;
    do {
      const page = await bucket.list({
        prefix: directoryPrefix,
        ...(options.recursive ? {} : { delimiter: "/" }),
        limit: Math.min(1_000, maximum + 1),
        cursor,
      });
      const add = (objectKey: string, kind: "file" | "directory", object?: R2Object) => {
        const relative = objectKey.slice(prefix.length).replace(/\/$/, "");
        const candidate = `${ROOT}/${relative}`;
        if (!relative || candidate === absolute) return;
        // Reject foreign/noncanonical native names instead of aliasing them.
        if (resolve(candidate) !== candidate) throw error("EINVAL", "noncanonical brain workspace entry");
        entries.set(candidate, {
          path: candidate, kind,
          ...(object === undefined ? {} : { size: object.size, modifiedAt: object.uploaded.getTime() }),
        });
        if (options.recursive) {
          let parent = candidate.slice(0, candidate.lastIndexOf("/"));
          while (parent !== absolute && parent.startsWith(`${absolute}/`)) {
            if (!entries.has(parent)) entries.set(parent, { path: parent, kind: "directory" });
            parent = parent.slice(0, parent.lastIndexOf("/"));
          }
        }
        if (entries.size > maximum) throw new RangeError(`workspace listing exceeds ${maximum} entries`);
      };
      for (const object of page.objects) {
        const directory = object.key.endsWith("/");
        if (directory) markers.add(`${ROOT}/${object.key.slice(prefix.length, -1)}`);
        add(object.key, directory ? "directory" : "file", object);
      }
      for (const directory of page.delimitedPrefixes) add(directory, "directory");
      cursor = page.truncated ? page.cursor : undefined;
    } while (cursor !== undefined);
    if (refresh) snapshot = new Map(entries);
    return [...entries.values()].sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  };
  return Object.freeze({
    root: ROOT,
    list,
    mkdir,
    async readFile(path) {
      const absolute = resolve(path);
      if (absolute === ROOT) throw error("EISDIR", "cannot read the workspace root as a file");
      await ancestors(absolute);
      if (snapshot) {
        const kind = snapshot.get(absolute)?.kind;
        if (kind === "directory") throw error("EISDIR", `${absolute} is a directory`);
        if (kind === undefined) throw error("ENOENT", `brain workspace file not found: ${absolute}`);
      }
      const object = await bucket.get(key(absolute));
      if (!object) {
        if (await stat(absolute) === "directory") throw error("EISDIR", `${absolute} is a directory`);
        throw error("ENOENT", `brain workspace file not found: ${absolute}`);
      }
      return new Uint8Array(await object.arrayBuffer());
    },
    async writeFile(path, contents) {
      const absolute = resolve(path);
      if (absolute === ROOT || await stat(absolute) === "directory") {
        throw error("EISDIR", `${absolute} is a directory`);
      }
      const bytes = typeof contents === "string" ? new TextEncoder().encode(contents)
        : contents instanceof ArrayBuffer ? new Uint8Array(contents)
        : new Uint8Array(contents.buffer, contents.byteOffset, contents.byteLength);
      await ancestors(absolute);
      await mkdir(absolute.slice(0, absolute.lastIndexOf("/")));
      await bucket.put(key(absolute), bytes);
      snapshot?.set(absolute, { path: absolute, kind: "file", size: bytes.byteLength, modifiedAt: Date.now() });
    },
    async remove(path, options = {}) {
      const absolute = resolve(path);
      if (absolute === ROOT) throw error("EPERM", "cannot remove the workspace root");
      await ancestors(absolute);
      const kind = await stat(absolute);
      if (kind === undefined) throw error("ENOENT", `${absolute} does not exist`);
      if (kind === "file") {
        await bucket.delete(key(absolute));
        snapshot?.delete(absolute);
        return;
      }
      const entries = await list(absolute, { recursive: true });
      if (!options.recursive && entries.length) throw error("ENOTEMPTY", `${absolute} is not empty`);
      const keys = entries.map((entry) => `${key(entry.path)}${entry.kind === "directory" ? "/" : ""}`);
      keys.push(`${key(absolute)}/`);
      for (let offset = 0; offset < keys.length; offset += 1_000) await bucket.delete(keys.slice(offset, offset + 1_000));
      for (const entry of [...entries, { path: absolute }]) {
        snapshot?.delete(entry.path);
        markers.delete(entry.path);
      }
    },
  } satisfies Workspace);
}

function error(code: string, message: string): Error {
  return Object.assign(new Error(message), { code });
}
