import git, { type GitHttpRequest, type HttpClient } from "isomorphic-git";
import type { Workspace, WorkspaceEntry } from "../tools/types.mjs";

export type ShellFetchOptions = Readonly<{
  method?: string | undefined;
  headers?: Headers | Record<string, string> | undefined;
  body?: string | Uint8Array | undefined;
  signal?: AbortSignal | undefined;
}>;

export type ShellFetchResult = Readonly<{
  status: number;
  statusText: string;
  headers: Record<string, string>;
  body: Uint8Array;
  url: string;
}>;

export type ShellFetch = (
  url: string,
  options?: ShellFetchOptions,
) => Promise<ShellFetchResult>;

type CommandContext = Readonly<{ cwd?: unknown; signal?: AbortSignal }>;
type CommandResult = Readonly<{
  stdout: string;
  stderr: string;
  exitCode: number;
}>;
type GitClone = (
  args: string[],
  context: CommandContext,
) => Promise<CommandResult>;

const REPOSITORY = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const GITHUB_REPOSITORY = /^https:\/\/github\.com\/([A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+?)(?:\.git)?\/?$/;
const MAX_GIT_HTTP_BODY_BYTES = 16 * 1024 * 1024;
const MAX_GIT_ENTRIES = 20_000;

/** gh compatibility command backed by the connected GitHub account. */
export function createGhCommand(
  fetch: ShellFetch,
  clone?: GitClone,
) {
  return {
    name: "gh",
    trusted: true,
    async execute(args: string[], context: CommandContext = {}) {
      const request: ShellFetch = (url, options) => {
        context.signal?.throwIfAborted();
        return fetch(url, { ...options, signal: context.signal });
      };
      try {
        context.signal?.throwIfAborted();
        if (args[0] === "auth" && args[1] === "status") {
          const user = await github(request, "/user");
          return ok(`Logged in to github.com as ${text(user, "login")} through the connected account.\n`);
        }
        if (args[0] === "api") {
          const fields = apiFields(args);
          const explicitMethod = option(args, "--method", "-X");
          const method = (explicitMethod ?? (Object.keys(fields).length ? "POST" : "GET")).toUpperCase();
          const endpoint = positional(args.slice(1), [
            "--method", "-X", "-f", "-F", "--field", "--raw-field",
          ]);
          if (!endpoint) throw new Error("gh api requires an endpoint");
          let path = endpoint.startsWith("/") ? endpoint : `/${endpoint}`;
          const hasFields = Object.keys(fields).length > 0;
          if (hasFields && (method === "GET" || method === "HEAD")) {
            const target = new URL(path, "https://api.github.com");
            for (const [name, value] of Object.entries(fields)) target.searchParams.set(name, value);
            path = `${target.pathname}${target.search}`;
          }
          return ok(`${JSON.stringify(await github(request, path, {
            method,
            ...(hasFields && method !== "GET" && method !== "HEAD"
              ? { body: JSON.stringify(fields) }
              : {}),
          }), null, 2)}\n`);
        }
        if (args[0] === "repo" && args[1] === "view") {
          const repository = option(args.slice(2), "--repo", "-R")
            ?? args.slice(2).find((value) => !value.startsWith("-"));
          requireRepository(repository, "gh repo view requires OWNER/REPO");
          const repo = requireRecord(
            await github(request, `/repos/${repository}`),
            "repository",
          );
          return ok([
            `name:\t${optionalText(repo, "full_name") ?? repository}`,
            `description:\t${optionalText(repo, "description") ?? ""}`,
            `url:\t${optionalText(repo, "html_url") ?? `https://github.com/${repository}`}`,
            "",
          ].join("\n"));
        }
        if (args[0] === "repo" && args[1] === "clone") {
          if (!clone) throw new Error("repository cloning is unavailable in this runtime");
          return clone(ghRepoCloneArguments(args.slice(2)), context);
        }
        if (args[0] === "repo" && args[1] === "list") {
          const owner = positional(args.slice(2), ["--limit", "-L"]);
          const perPage = limit(option(args.slice(2), "--limit", "-L"));
          const repositories = await github(request, `/user/repos?${new URLSearchParams({
            affiliation: "owner,collaborator,organization_member",
            per_page: "100",
            sort: "updated",
          })}`);
          if (!Array.isArray(repositories)) throw new Error("GitHub returned an invalid repository list");
          const selected = repositories.filter((repository) => {
            if (!owner) return true;
            const record = requireRecord(repository, "repository");
            const repositoryOwner = requireRecord(record.owner, "repository owner");
            return optionalText(repositoryOwner, "login")?.toLowerCase() === owner.toLowerCase();
          }).slice(0, perPage);
          return ok(selected.map((repository) => {
            const record = requireRecord(repository, "repository");
            return [
              optionalText(record, "full_name") ?? "unknown",
              optionalText(record, "description") ?? "",
              record.private === true ? "private" : "public",
            ].join("\t");
          }).join("\n") + (selected.length ? "\n" : ""));
        }
        if (args[0] === "pr" && args[1] === "list") {
          const repository = option(args.slice(2), "--repo", "-R");
          requireRepository(repository, "gh pr list requires --repo OWNER/REPO");
          const pulls = await github(request, `/repos/${repository}/pulls?${new URLSearchParams({
            state: "open",
            per_page: String(limit(option(args.slice(2), "--limit", "-L"))),
          })}`);
          if (!Array.isArray(pulls)) throw new Error("GitHub returned an invalid pull request list");
          return ok(pulls.map((pull) => {
            const row = requireRecord(pull, "pull request");
            const head = requireRecord(row.head, "pull request head");
            return [row.number, text(row, "title"), text(head, "ref")].join("\t");
          }).join("\n") + (pulls.length ? "\n" : ""));
        }
        return fail([
          "gh (Nanocodex Just Bash compatibility command)",
          "",
          "Supported commands:",
          "  gh auth status",
          "  gh api [--method METHOD] [-f key=value] ENDPOINT",
          "  gh repo list [OWNER] [--limit N]",
          "  gh repo clone OWNER/REPO [DIRECTORY] [-- GITFLAGS...]",
          "  gh repo view OWNER/REPO",
          "  gh pr list --repo OWNER/REPO [--limit N]",
          "",
          "Connected GitHub calls use the permissions granted in Profile.",
          "",
        ].join("\n"));
      } catch (error) {
        return fail(`gh: ${error instanceof Error ? error.message : String(error)}\n`);
      }
    },
  };
}

function ghRepoCloneArguments(args: string[]): string[] {
  const separator = args.indexOf("--");
  const commandArgs = separator === -1 ? args : args.slice(0, separator);
  const gitArgs = separator === -1 ? [] : args.slice(separator + 1);
  if (commandArgs.some((value) => value.startsWith("-"))) {
    throw new Error("gh repo clone supports OWNER/REPO [DIRECTORY] [-- GITFLAGS...]");
  }
  if (commandArgs.length < 1 || commandArgs.length > 2) {
    throw new Error("gh repo clone requires OWNER/REPO and an optional destination");
  }
  const repository = commandArgs[0];
  requireRepository(repository, "gh repo clone requires OWNER/REPO");
  return [
    "clone",
    ...gitArgs,
    `https://github.com/${repository}.git`,
    ...(commandArgs[1] === undefined ? [] : [commandArgs[1]]),
  ];
}

/** Git compatibility command backed by durable workspace storage and public Git smart HTTP. */
export function createGitCommand(
  fetch: ShellFetch,
  workspace: () => Workspace,
) {
  return {
    name: "git",
    trusted: true,
    async execute(args: string[], context: CommandContext = {}) {
      try {
        const mounted = commandWorkspace(workspace(), context.signal);
        const command = args[0];
        if (command === "clone") {
          return ok(await cloneRepository(fetch, mounted, args.slice(1), context));
        }
        const dir = await gitDirectory(mounted, context.cwd);
        const fs = workspaceFs(mounted);
        if (command === "status") {
          const matrix = await git.statusMatrix({ fs, dir });
          const changed = matrix.filter(([, head, workdir, stage]) => head !== workdir || head !== stage);
          if (args.includes("--short") || args.includes("-s") || args.includes("--porcelain")) {
            const lines = changed.flatMap(([path, head, workdir, stage]) =>
              shortStatusCodes(head, workdir, stage).map((code) => `${code} ${path}`));
            return ok(lines.join("\n") + (lines.length ? "\n" : ""));
          }
          const branch = await git.currentBranch({ fs, dir, fullname: false });
          return ok(`On branch ${branch ?? "HEAD"}\n${changed.length ? `${changed.length} changed path(s)\n` : "nothing to commit, working tree clean\n"}`);
        }
        if (command === "log") {
          const depth = logDepth(args.slice(1));
          const commits = await git.log({ fs, dir, depth });
          if (args.includes("--oneline")) {
            return ok(commits.map(({ oid, commit }) => `${oid.slice(0, 7)} ${commit.message.split("\n")[0]}`).join("\n") + (commits.length ? "\n" : ""));
          }
          return ok(commits.map(({ oid, commit }) => [
            `commit ${oid}`,
            `Author: ${commit.author.name} <${commit.author.email}>`,
            `Date:   ${new Date(commit.author.timestamp * 1_000).toISOString()}`,
            "",
            `    ${commit.message.trim().replaceAll("\n", "\n    ")}`,
            "",
          ].join("\n")).join("\n"));
        }
        if (command === "rev-parse" && args[1] === "HEAD") {
          return ok(`${await git.resolveRef({ fs, dir, ref: "HEAD" })}\n`);
        }
        if (command === "branch" && (args.length === 1 || args.includes("--show-current"))) {
          const branch = await git.currentBranch({ fs, dir, fullname: false });
          return ok(args.includes("--show-current") ? `${branch ?? ""}\n` : `${branch ? `* ${branch}` : "* (detached HEAD)"}\n`);
        }
        if (command === "remote" && (args.length === 1 || args.includes("-v"))) {
          const remotes = await git.listRemotes({ fs, dir });
          return ok(remotes.flatMap(({ remote, url }) => args.includes("-v")
            ? [`${remote}\t${url} (fetch)`]
            : [remote]).join("\n") + (remotes.length ? "\n" : ""));
        }
        if (command === "ls-files") {
          const files = await git.listFiles({ fs, dir });
          return ok(`${files.join("\n")}${files.length ? "\n" : ""}`);
        }
        return fail([
          `git: '${command ?? ""}' is not implemented by managed git`,
          "Nanocodex git supports clone, status, log, rev-parse HEAD, branch, remote, and ls-files.",
          "",
        ].join("\n"));
      } catch (error) {
        return fail(`git: ${error instanceof Error ? error.message : String(error)}\n`);
      }
    },
  };
}

function shortStatusCodes(head: number, workdir: number, stage: number): string[] {
  if (head === 0 && stage === 0) return workdir === 0 ? [] : ["??"];
  if (head === 1 && stage === 0) return workdir === 0
    ? ["D "]
    : ["D ", "??"];

  const index = head === 0 ? "A" : stage === head ? " " : "M";
  const workingTree = workdir === stage ? " " : workdir === 0 ? "D" : "M";
  return index === " " && workingTree === " " ? [] : [`${index}${workingTree}`];
}

async function gitDirectory(workspace: Workspace, cwd: unknown): Promise<string> {
  let directory = typeof cwd === "string" ? gitWorkspacePath(workspace, cwd) : workspace.root;
  for (;;) {
    if (await workspaceEntry(workspace, `${directory}/.git`)) return directory;
    if (directory === workspace.root) throw new Error("not a git repository");
    directory = directory.slice(0, directory.lastIndexOf("/")) || workspace.root;
  }
}

function logDepth(args: string[]): number {
  const compact = args.find((value) => /^-\d+$/.test(value));
  const explicit = option(args, "--max-count", "-n") ?? joinedOption(args, "--max-count");
  const value = explicit ?? compact?.slice(1);
  if (value === undefined) return 20;
  const depth = positiveInteger(value, "log depth");
  if (depth > 200) throw new Error("log depth cannot exceed 200");
  return depth;
}

async function cloneRepository(
  fetch: ShellFetch,
  workspace: Workspace,
  args: string[],
  context: CommandContext,
): Promise<string> {
  const { cwd, signal } = context;
  const depthValue = option(args, "--depth", "-") ?? joinedOption(args, "--depth");
  const depth = depthValue === undefined ? undefined : positiveInteger(depthValue, "--depth");
  const branch = option(args, "--branch", "-b") ?? joinedOption(args, "--branch");
  const positionals = gitPositionals(args);
  const remote = positionals[0];
  const match = remote?.match(GITHUB_REPOSITORY);
  if (!match) throw new Error("clone requires an https://github.com/OWNER/REPO URL");
  if (branch !== undefined && (!branch || branch.startsWith("-") || branch.includes(".."))) {
    throw new Error("--branch must name one branch or tag");
  }
  const repository = match[1]!;
  const requestedDestination = positionals[1] ?? repository.slice(repository.indexOf("/") + 1);
  const absoluteWorkspaceDestination = requestedDestination.startsWith(`${workspace.root}/`);
  const destination = absoluteWorkspaceDestination
    ? requestedDestination.slice(workspace.root.length + 1)
    : requestedDestination;
  if (!/^[A-Za-z0-9_.-]+$/.test(destination) || destination === "." || destination === "..") {
    throw new Error("clone destination must be one workspace directory name");
  }
  const root = !absoluteWorkspaceDestination
    && typeof cwd === "string" && cwd.startsWith(`${workspace.root}/`)
    ? cwd
    : workspace.root;
  const dir = `${root}/${destination}`;
  if (await workspaceEntry(workspace, dir)) throw new Error(`destination path '${destination}' already exists`);
  try {
    await git.clone({
      fs: workspaceFs(workspace),
      http: managedGitHttp(fetch, signal),
      dir,
      url: `https://github.com/${repository}.git`,
      ...(depth === undefined ? {} : { depth, singleBranch: true }),
      ...(branch === undefined ? {} : { ref: branch }),
    });
  } catch (error) {
    if (await workspaceEntry(workspace, dir)) {
      await workspace.remove(dir, { recursive: true });
    }
    throw error;
  }
  return `Cloning into '${destination}'...\n`;
}

function managedGitHttp(fetch: ShellFetch, signal?: AbortSignal): HttpClient {
  return {
    async request(request: GitHttpRequest) {
      signal?.throwIfAborted();
      const body = request.body === undefined
        ? undefined
        : await collectGitBody(request.body);
      const response = await fetch(request.url, {
        method: request.method,
        headers: request.headers,
        body,
        signal: signal ?? (request.signal instanceof AbortSignal ? request.signal : undefined),
      });
      return {
        url: response.url,
        statusCode: response.status,
        statusMessage: response.statusText,
        headers: response.headers,
        body: (async function* () { yield response.body; })(),
      };
    },
  };
}

async function collectGitBody(body: AsyncIterable<Uint8Array>): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  let size = 0;
  for await (const chunk of body) {
    size += chunk.byteLength;
    if (size > MAX_GIT_HTTP_BODY_BYTES) throw new Error("git HTTP request body is too large");
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

function workspaceFs(workspace: Workspace) {
  const resolve = (path: string) => gitWorkspacePath(workspace, path);
  const promises = {
    readFile: async (path: string, options?: string | { encoding?: string }) => {
      const contents = await workspace.readFile(resolve(path));
      const encoding = typeof options === "string" ? options : options?.encoding;
      return encoding ? new TextDecoder(encoding).decode(contents) : contents;
    },
    writeFile: async (path: string, contents: Uint8Array | string) => workspace.writeFile(resolve(path), contents),
    unlink: async (path: string) => workspace.remove(resolve(path)),
    readdir: async (path: string) => (await workspace.list(resolve(path), { maxEntries: MAX_GIT_ENTRIES }))
      .map(({ path: child }) => child.slice(child.lastIndexOf("/") + 1)),
    mkdir: async (path: string, options?: { recursive?: boolean }) => {
      path = resolve(path);
      if (options?.recursive) {
        const relative = path.startsWith(workspace.root) ? path.slice(workspace.root.length) : path;
        let current = workspace.root;
        for (const segment of relative.split("/").filter(Boolean)) {
          current += `/${segment}`;
          if (!(await workspaceEntry(workspace, current))) await workspace.mkdir(current);
        }
      } else {
        await workspace.mkdir(path);
      }
    },
    rmdir: async (path: string) => workspace.remove(resolve(path)),
    stat: async (path: string) => gitStat(await requiredWorkspaceEntry(workspace, resolve(path))),
    lstat: async (path: string) => gitStat(await requiredWorkspaceEntry(workspace, resolve(path))),
    readlink: async () => { throw Object.assign(new Error("symbolic links are unavailable"), { code: "EINVAL" }); },
    symlink: async () => { throw Object.assign(new Error("symbolic links are unavailable"), { code: "ENOSYS" }); },
    chmod: async (path: string) => { await requiredWorkspaceEntry(workspace, resolve(path)); },
  };
  return { promises };
}

function commandWorkspace(workspace: Workspace, signal?: AbortSignal): Workspace {
  return {
    root: workspace.root,
    list: (...args) => { signal?.throwIfAborted(); return workspace.list(...args); },
    readFile: (...args) => { signal?.throwIfAborted(); return workspace.readFile(...args); },
    writeFile: (...args) => { signal?.throwIfAborted(); return workspace.writeFile(...args); },
    mkdir: (...args) => { signal?.throwIfAborted(); return workspace.mkdir(...args); },
    remove: (...args) => { signal?.throwIfAborted(); return workspace.remove(...args); },
  };
}

function gitWorkspacePath(workspace: Workspace, path: string): string {
  const source = path.startsWith("/") ? path : `${workspace.root}/${path}`;
  const segments: string[] = [];
  for (const segment of source.split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") segments.pop();
    else segments.push(segment);
  }
  const resolved = `/${segments.join("/")}`;
  if (resolved !== workspace.root && !resolved.startsWith(`${workspace.root}/`)) {
    throw Object.assign(new Error(`path escapes ${workspace.root}`), { code: "EPERM" });
  }
  return resolved;
}

async function workspaceEntry(workspace: Workspace, path: string): Promise<WorkspaceEntry | undefined> {
  if (path === workspace.root) return { kind: "directory", path };
  const separator = path.lastIndexOf("/");
  const parent = path.slice(0, separator) || workspace.root;
  const target = path.slice(separator + 1);
  try {
    return (await workspace.list(parent, { maxEntries: MAX_GIT_ENTRIES }))
      .find((entry) => entry.path === target || entry.path.endsWith(`/${target}`));
  } catch (error) {
    if ((error as { code?: unknown })?.code === "ENOENT") return undefined;
    throw error;
  }
}

async function requiredWorkspaceEntry(workspace: Workspace, path: string): Promise<WorkspaceEntry> {
  const entry = await workspaceEntry(workspace, path);
  if (entry) return entry;
  throw Object.assign(new Error(`${path} does not exist`), { code: "ENOENT" });
}

function gitStat(entry: WorkspaceEntry) {
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

function gitPositionals(args: string[]): string[] {
  const positionals: string[] = [];
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index]!;
    if (value === "--depth" || value === "--branch" || value === "-b") {
      index += 1;
      continue;
    }
    if (value.startsWith("--depth=") || value.startsWith("--branch=")) continue;
    if (value.startsWith("-")) throw new Error(`unsupported clone option '${value}'`);
    positionals.push(value);
  }
  if (positionals.length < 1 || positionals.length > 2) throw new Error("clone requires a repository URL and optional destination");
  return positionals;
}

function joinedOption(args: string[], name: string): string | undefined {
  return args.find((value) => value.startsWith(`${name}=`))?.slice(name.length + 1);
}

function positiveInteger(value: string, name: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) throw new Error(`${name} must be a positive integer`);
  return parsed;
}

async function github(
  fetch: ShellFetch,
  path: string,
  options: Readonly<{ method?: string; body?: string }> = {},
): Promise<unknown> {
  const url = new URL(path, "https://api.github.com");
  if (url.origin !== "https://api.github.com") throw new Error("endpoint is outside api.github.com");
  const response = await fetch(url.href, {
    method: options.method,
    headers: {
      accept: "application/vnd.github+json",
      ...(options.body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: options.body,
  });
  const raw = new TextDecoder().decode(response.body);
  let value: unknown;
  try { value = raw ? JSON.parse(raw) : null; } catch {
    throw new Error(`GitHub returned invalid JSON (HTTP ${response.status})`);
  }
  if (response.status < 200 || response.status >= 300) {
    const detail = value && typeof value === "object"
      ? optionalText(value as Record<string, unknown>, "message")
        ?? optionalText(value as Record<string, unknown>, "error")
      : undefined;
    throw new Error(`GitHub request failed (HTTP ${response.status}${detail ? `: ${detail}` : ""})`);
  }
  return value;
}

function apiFields(args: string[]): Record<string, string> {
  const fields: Record<string, string> = Object.create(null) as Record<string, string>;
  for (let index = 0; index < args.length; index += 1) {
    if (!["-f", "-F", "--field", "--raw-field"].includes(args[index]!)) continue;
    const field = args[index + 1];
    const separator = field?.indexOf("=") ?? -1;
    if (!field || separator <= 0) throw new Error(`${args[index]} requires key=value`);
    fields[field.slice(0, separator)] = field.slice(separator + 1);
    index += 1;
  }
  return fields;
}

function option(args: string[], long: string, short: string): string | undefined {
  const index = args.findIndex((value) => value === long || value === short);
  return index === -1 ? undefined : args[index + 1];
}

function positional(args: string[], optionsWithValues: string[]): string | undefined {
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index]!;
    if (optionsWithValues.includes(value)) {
      index += 1;
      continue;
    }
    if (!value.startsWith("-")) return value;
  }
  return undefined;
}

function limit(value: string | undefined): number {
  if (value === undefined) return 30;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 100) {
    throw new Error("--limit must be an integer from 1 to 100");
  }
  return parsed;
}

function requireRepository(value: string | undefined, message: string): asserts value is string {
  if (!value || !REPOSITORY.test(value)) throw new Error(message);
}

function requireRecord(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`GitHub returned an invalid ${name}`);
  }
  return value as Record<string, unknown>;
}

function text(value: unknown, key: string): string {
  const field = requireRecord(value, "response")[key];
  if (typeof field !== "string") throw new Error(`GitHub response is missing ${key}`);
  return field;
}

function optionalText(value: Record<string, unknown>, key: string): string | undefined {
  return typeof value[key] === "string" ? value[key] : undefined;
}

function ok(stdout: string) { return { stdout, stderr: "", exitCode: 0 }; }
function fail(stderr: string) { return { stdout: "", stderr, exitCode: 1 }; }
