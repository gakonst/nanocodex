import git from "isomorphic-git";

const DEFAULT_DIRECTORY = "repository";
const MARKER_NAME = "nanocodex-repository.json";
const MARKER_VERSION = 1;
const MAX_HTTP_REQUEST_BYTES = 16 * 1024 * 1024;
const MAX_MARKER_BYTES = 16 * 1024;
const SHA1 = /^[a-f0-9]{40}$/;

/**
 * Materializes one immutable repository generation into a workspace child.
 * A completed generation is never refreshed: matching reopens only validate
 * the host marker and local HEAD, leaving retained worktree changes untouched.
 */
export async function materializeRepositoryWorkspace(options) {
  const input = repositoryOptions(options);
  const fs = workspaceFs(input.workspace);
  const directory = `${input.workspace.root}/${input.directory}`;
  const markerPath = `${directory}/.git/${MARKER_NAME}`;
  const expectedMarker = marker(input);
  const retained = await readMarker(input.workspace, markerPath);

  if (retained !== undefined) {
    if (!validMarker(retained)) {
      throw new Error("repository workspace has an invalid host marker");
    }
    if (!sameMarker(retained, expectedMarker)) {
      throw new Error("repository workspace is already materialized for a different generation");
    }
    const localHead = await resolveHead(fs, directory);
    if (localHead !== input.head) {
      throw new Error("repository workspace HEAD no longer matches its immutable generation");
    }
    return descriptor(input, directory, markerPath);
  }

  // This named child is package-owned. Only an unmarked partial checkout at
  // this exact, validated child path is eligible for recursive cleanup.
  if (await workspaceEntry(input.workspace, directory)) {
    await input.workspace.remove(directory, { recursive: true });
  }

  await git.clone({
    fs,
    http: gitHttp(input.fetch),
    dir: directory,
    url: input.seedUrl,
    ref: input.branch,
    remote: "seed",
    singleBranch: true,
    noTags: true,
  });

  const localHead = await resolveHead(fs, directory);
  if (localHead !== input.head) {
    throw new Error(`repository seed resolved ${localHead}, expected ${input.head}`);
  }
  if (input.writableRemote !== undefined) {
    const remote = input.writableRemote;
    await git.addRemote({ fs, dir: directory, remote: remote.name, url: remote.url, force: true });
    await git.setConfig({
      fs,
      dir: directory,
      path: `branch.${input.branch}.remote`,
      value: remote.name,
    });
    await git.setConfig({
      fs,
      dir: directory,
      path: `branch.${input.branch}.merge`,
      value: `refs/heads/${remote.branch}`,
    });
  }
  await input.workspace.writeFile(markerPath, `${JSON.stringify(expectedMarker)}\n`);
  return descriptor(input, directory, markerPath);
}

function repositoryOptions(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("repository workspace options are required");
  }
  const workspace = value.workspace;
  validateWorkspace(workspace);
  if (typeof value.fetch !== "function") {
    throw new TypeError("repository workspace fetch must be a function");
  }
  const head = sha1(value.head, "repository head");
  const seedUrl = repositoryUrl(value.seedUrl, "repository seed URL");
  if (!new URL(seedUrl).pathname.endsWith(`/${head}`)) {
    throw new TypeError("repository seed URL must end in its immutable head");
  }
  const branch = gitRef(value.branch ?? "master", "repository branch");
  const directory = childName(value.directory ?? DEFAULT_DIRECTORY, "repository directory");
  const writableRemote = value.writableRemote === undefined
    ? undefined
    : writableRemoteOptions(value.writableRemote, branch);
  return { workspace, fetch: value.fetch, head, seedUrl, branch, directory, writableRemote };
}

function writableRemoteOptions(value, defaultBranch) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("writableRemote must be an object");
  }
  const name = childName(value.name ?? "origin", "writable remote name");
  if (name === "seed") throw new TypeError("writable remote name is reserved");
  return Object.freeze({
    name,
    url: repositoryUrl(value.url, "writable remote URL"),
    branch: gitRef(value.branch ?? defaultBranch, "writable remote branch"),
  });
}

function descriptor(input, directory, markerPath) {
  return Object.freeze({
    branch: input.branch,
    directory,
    gitDirectory: `${directory}/.git`,
    head: input.head,
    markerPath,
    seedUrl: input.seedUrl,
    writableRemote: input.writableRemote,
  });
}

function marker(input) {
  return {
    version: MARKER_VERSION,
    branch: input.branch,
    directory: input.directory,
    head: input.head,
    seedUrl: input.seedUrl,
    ...(input.writableRemote === undefined ? {} : { writableRemote: input.writableRemote }),
  };
}

async function readMarker(workspace, path) {
  const entry = await workspaceEntry(workspace, path);
  if (entry === undefined) return undefined;
  if (entry.kind !== "file" || (entry.size !== undefined
    && (!Number.isSafeInteger(entry.size) || entry.size > MAX_MARKER_BYTES))) {
    throw new Error("repository workspace has an invalid host marker");
  }
  try {
    const contents = await workspace.readFile(path);
    if (contents.byteLength > MAX_MARKER_BYTES) throw new Error("marker is too large");
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(contents));
  } catch {
    throw new Error("repository workspace has an invalid host marker");
  }
}

function validMarker(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const keys = Object.keys(value).sort();
  const expectedKeys = ["branch", "directory", "head", "seedUrl", "version"];
  if (value.writableRemote !== undefined) expectedKeys.push("writableRemote");
  if (keys.join("\0") !== expectedKeys.sort().join("\0")) return false;
  try {
    sha1(value.head, "marker head");
    childName(value.directory, "marker directory");
    gitRef(value.branch, "marker branch");
    const url = repositoryUrl(value.seedUrl, "marker seed URL");
    if (!new URL(url).pathname.endsWith(`/${value.head}`)) return false;
    if (value.writableRemote !== undefined) writableRemoteOptions(value.writableRemote, value.branch);
    return value.version === MARKER_VERSION;
  } catch {
    return false;
  }
}

function sameMarker(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

async function resolveHead(fs, directory) {
  try {
    return await git.resolveRef({ fs, dir: directory, ref: "HEAD" });
  } catch (error) {
    throw new Error("repository workspace has no valid local HEAD", { cause: error });
  }
}

function gitHttp(fetch) {
  return {
    async request(request) {
      const body = request.body === undefined ? undefined : await collectBody(request.body);
      const response = await fetch(request.url, {
        ...request.fetchOptions,
        method: request.method ?? "GET",
        headers: request.headers,
        ...(body === undefined ? {} : { body }),
        ...(typeof AbortSignal !== "undefined" && request.signal instanceof AbortSignal
          ? { signal: request.signal }
          : {}),
      });
      if (!response || typeof response.status !== "number") {
        throw new TypeError("repository fetch returned an invalid response");
      }
      return {
        url: response.url || request.url,
        statusCode: response.status,
        statusMessage: response.statusText ?? "",
        headers: responseHeaders(response.headers),
        body: responseBody(response),
      };
    },
  };
}

async function collectBody(body) {
  const chunks = [];
  let size = 0;
  for await (const chunk of body) {
    if (!(chunk instanceof Uint8Array)) throw new TypeError("git HTTP body must contain bytes");
    size += chunk.byteLength;
    if (size > MAX_HTTP_REQUEST_BYTES) throw new RangeError("git HTTP request exceeds its byte bound");
    chunks.push(chunk);
  }
  const joined = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    joined.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return joined;
}

function responseHeaders(headers) {
  const output = Object.create(null);
  if (headers && typeof headers.forEach === "function") {
    headers.forEach((value, name) => { output[name] = value; });
  } else if (headers && typeof headers === "object") {
    for (const [name, value] of Object.entries(headers)) output[name] = String(value);
  }
  return output;
}

async function* responseBody(response) {
  if (response.body instanceof Uint8Array) {
    yield response.body;
    return;
  }
  if (response.body?.[Symbol.asyncIterator]) {
    for await (const chunk of response.body) yield chunk;
    return;
  }
  if (typeof response.arrayBuffer !== "function") {
    throw new TypeError("repository fetch response has no byte body");
  }
  yield new Uint8Array(await response.arrayBuffer());
}

function workspaceFs(workspace) {
  const operation = (run) => async (...args) => {
    try {
      return await run(...args);
    } catch (error) {
      throw gitFsError(error);
    }
  };
  const promises = {
    readFile: operation(async (path, options) => {
      const contents = await workspace.readFile(workspacePath(workspace, path));
      const encoding = typeof options === "string" ? options : options?.encoding;
      return encoding ? new TextDecoder(encoding).decode(contents) : contents;
    }),
    writeFile: operation((path, contents) => workspace.writeFile(workspacePath(workspace, path), contents)),
    unlink: operation((path) => workspace.remove(workspacePath(workspace, path))),
    readdir: operation(async (path) => (await workspace.list(workspacePath(workspace, path)))
      .map((entry) => entry.path.slice(entry.path.lastIndexOf("/") + 1))),
    mkdir: operation((path) => workspace.mkdir(workspacePath(workspace, path))),
    rmdir: operation((path) => workspace.remove(workspacePath(workspace, path))),
    stat: operation(async (path) => gitStat(await requiredWorkspaceEntry(workspace, workspacePath(workspace, path)))),
    lstat: operation(async (path) => gitStat(await requiredWorkspaceEntry(workspace, workspacePath(workspace, path)))),
    readlink: async () => { throw fsError("EINVAL", "symbolic links are unavailable"); },
    symlink: async () => { throw fsError("ENOSYS", "symbolic links are unavailable"); },
    chmod: operation(async (path) => { await requiredWorkspaceEntry(workspace, workspacePath(workspace, path)); }),
  };
  return { promises };
}

function workspacePath(workspace, value) {
  if (typeof value !== "string" || value.includes("\0") || value.includes("\\")) {
    throw fsError("EINVAL", "invalid repository filesystem path");
  }
  const source = value.startsWith("/") ? value : `${workspace.root}/${value}`;
  const segments = [];
  for (const segment of source.split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") segments.pop();
    else segments.push(segment);
  }
  const resolved = `/${segments.join("/")}`;
  if (resolved !== workspace.root && !resolved.startsWith(`${workspace.root}/`)) {
    throw fsError("EPERM", `repository path escapes ${workspace.root}`);
  }
  return resolved;
}

async function workspaceEntry(workspace, path) {
  if (path === workspace.root) return { kind: "directory", path };
  const slash = path.lastIndexOf("/");
  const parent = path.slice(0, slash) || workspace.root;
  try {
    return (await workspace.list(parent)).find((entry) => entry.path === path);
  } catch (error) {
    if (missingError(error)) return undefined;
    throw error;
  }
}

async function requiredWorkspaceEntry(workspace, path) {
  const entry = await workspaceEntry(workspace, path);
  if (entry !== undefined) return entry;
  throw fsError("ENOENT", `${path} does not exist`);
}

function gitStat(entry) {
  const modified = new Date(entry.modifiedAt ?? 0);
  return {
    isFile: () => entry.kind === "file",
    isDirectory: () => entry.kind === "directory",
    isSymbolicLink: () => false,
    mode: entry.kind === "directory" ? 0o755 : 0o644,
    size: entry.size ?? 0,
    mtime: modified,
    ctime: modified,
    birthtime: modified,
    dev: 0,
    ino: 0,
    uid: 0,
    gid: 0,
  };
}

function validateWorkspace(workspace) {
  if (!workspace || typeof workspace !== "object" || typeof workspace.root !== "string") {
    throw new TypeError("repository materialization requires a workspace handle");
  }
  if (!/^\/[^/]+(?:\/[^/]+)*$/.test(workspace.root)
    || workspace.root.includes("\\") || workspace.root.includes("\0")
    || workspace.root.split("/").some((segment) => segment === "." || segment === "..")) {
    throw new TypeError("repository workspace root must be a bounded absolute path");
  }
  for (const method of ["list", "readFile", "writeFile", "remove", "mkdir"]) {
    if (typeof workspace[method] !== "function") {
      throw new TypeError(`repository workspace requires ${method}()`);
    }
  }
}

function repositoryUrl(value, name) {
  if (typeof value !== "string" || !value) throw new TypeError(`${name} must be a URL`);
  let url;
  try { url = new URL(value); } catch { throw new TypeError(`${name} must be a URL`); }
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new TypeError(`${name} must be a credential-free HTTP URL`);
  }
  return url.href.replace(/\/$/, "");
}

function sha1(value, name) {
  if (typeof value !== "string" || !SHA1.test(value)) {
    throw new TypeError(`${name} must be a lowercase SHA-1 object ID`);
  }
  return value;
}

function childName(value, name) {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(value)
    || value === "." || value === "..") {
    throw new TypeError(`${name} must be one safe child name`);
  }
  return value;
}

function gitRef(value, name) {
  if (typeof value !== "string" || !value || value.length > 255
    || value.startsWith(".") || value.endsWith(".") || value.endsWith("/")
    || value.includes("..") || value.includes("//") || value.includes("@{")
    || /[\x00-\x20~^:?*[\\]/.test(value)
    || value.split("/").some((part) => !part || part.endsWith(".lock"))) {
    throw new TypeError(`${name} is invalid`);
  }
  return value;
}

function missingError(error) {
  return error?.code === "ENOENT" || error?.code === "ENOTDIR" || error?.name === "NotFoundError";
}

function gitFsError(error) {
  if (missingError(error) && !error?.code) error.code = "ENOENT";
  return error;
}

function fsError(code, message) {
  return Object.assign(new Error(message), { code });
}
