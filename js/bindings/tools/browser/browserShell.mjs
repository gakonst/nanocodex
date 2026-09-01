import "./browserBuffer.mjs";
import git from "isomorphic-git";
import http from "isomorphic-git/http/web";
import { artifact } from "../artifact.mjs";
import { createJustBashRuntime } from "../bash.mjs";
import { createBrowserEgressFetch } from "./browserEgress.mjs";
import { browserThread, notifyThreadGitChanged, prepareThreadGit, THREAD_GIT_AUTHOR, THREAD_GIT_DIRECTORY, withThreadGitLock, } from "./threadGit.mjs";
import { openThreadWorkspace } from "./workspace.mjs";
const utf8 = new TextEncoder();
const utf8Decoder = new TextDecoder();
const diffDecoder = new TextDecoder("utf-8", { fatal: true });
const MAX_OUTPUT_BYTES = 4 * 1024 * 1024;
const MAX_EXECUTION_MS = 30_000;
const MAX_GIT_LOG_DEPTH = 200;
const MAX_DIFF_FILE_BYTES = 1024 * 1024;
const MAX_INDEXED_PATHS = 100_000;
const MAX_PROJECT_INSTRUCTIONS_BYTES = 32 * 1024;
const PROJECT_INSTRUCTION_FILES = ["AGENTS.override.md", "AGENTS.md"];
const DIFF_TRUNCATION_NOTICE = "\n[diff truncated by browser git]\n";
export function validateBrowserArtifactSource(source) {
    try {
        // Compile with the exact bindings and strict wrapper used by the
        // isolated artifact frame without running application code.
        new Function("React", "html", "sendPrompt", `"use strict";\n${source}\n;return typeof App === "function" ? App : undefined;`);
    }
    catch (error) {
        throw new Error(`artifact source is not executable JavaScript: ${errorMessage(error)}`);
    }
}
export async function prepareBrowserShell(threadId, origin, fetch, headers) {
    const thread = browserThread(threadId, origin);
    const [{ rawFs, workspaceRoot }, workspace] = await Promise.all([
        prepareThreadGit(thread),
        openThreadWorkspace(threadId),
    ]);
    const projectInstructions = await loadBrowserProjectInstructions(rawFs);
    let shellFs;
    const recordShellMutation = (operation, path) => {
        shellFs[operation](path);
    };
    const notifyingWorkspace = Object.freeze({
        root: workspace.root,
        list: workspace.list,
        readFile: workspace.readFile,
        async writeFile(path, contents) {
            await workspace.writeFile(path, contents);
            try {
                recordShellMutation("recordExternalWrite", path);
            }
            finally {
                notifyThreadGitChanged(thread);
            }
        },
        async remove(path, options) {
            await workspace.remove(path, options);
            try {
                recordShellMutation("recordExternalRemove", path);
            }
            finally {
                notifyThreadGitChanged(thread);
            }
        },
        async mkdir(path) {
            await workspace.mkdir(path);
            try {
                recordShellMutation("recordExternalWrite", path);
            }
            finally {
                notifyThreadGitChanged(thread);
            }
        },
    });
    const shell = await createBrowserBash(rawFs, thread, {
        workspaceRoot,
        origin,
        fetch,
        headers,
    });
    shellFs = shell.filesystem;
    return {
        descriptor: shell.descriptor,
        instructions: shell.instructions,
        projectInstructions,
        workspace: notifyingWorkspace,
        artifactTool: artifact({
            workspace: notifyingWorkspace,
            validateSource: validateBrowserArtifactSource,
        }),
        execTool: shell.tool,
    };
}
/** Captures the root project instructions using the native Nanocodex precedence and budget. */
export async function loadBrowserProjectInstructions(rawFs) {
    for (const filename of PROJECT_INSTRUCTION_FILES) {
        const path = `${THREAD_GIT_DIRECTORY}/${filename}`;
        let stat;
        try {
            stat = await rawFs.promises.stat(path);
        }
        catch (error) {
            if (error?.code === "ENOENT")
                continue;
            console.warn("failed to read project AGENTS.md instructions", { path, error });
            return undefined;
        }
        if (!stat.isFile())
            continue;
        if (stat.size > MAX_PROJECT_INSTRUCTIONS_BYTES) {
            console.warn("project doc exceeds remaining budget; truncating", {
                path,
                remainingBytes: MAX_PROJECT_INSTRUCTIONS_BYTES,
            });
        }
        try {
            const bytes = await rawFs.promises.readFile(path, {
                maxBytes: MAX_PROJECT_INSTRUCTIONS_BYTES,
            });
            const instructions = utf8Decoder.decode(bytes);
            return instructions.trim() ? instructions : undefined;
        }
        catch (error) {
            console.warn("failed to read project AGENTS.md instructions", { path, error });
            return undefined;
        }
    }
    return undefined;
}
/** Builds the browser shell over an already-open OPFS Git adapter. */
export async function createBrowserBash(rawFs, thread, options = {}) {
    const { createTwoFilesPatch } = await import("diff");
    const filesystem = new OpfsShellFileSystem(rawFs);
    await filesystem.refreshPaths();
    const executionTimeoutMs = options.executionTimeoutMs ?? MAX_EXECUTION_MS;
    const origin = options.origin ?? globalThis.location?.origin;
    const workerEgress = {
        origin,
        threadId: thread.id,
        ...(options.headers === undefined
            ? {}
            : { headers: Object.fromEntries(new Headers(options.headers).entries()) }),
    };
    let pythonRuntime = options.pythonRuntime;
    const shellFetch = options.fetch ?? (origin
        ? createBrowserEgressFetch({
            fetch: options.connectorFetch ?? globalThis.fetch,
            origin,
            threadId: thread.id,
        })
        : unavailableBrowserEgress);
    const loadPython = async (name) => {
        const module = await import("./browserPython.mjs");
        pythonRuntime ??= options.workspaceRoot
            ? new module.BrowserPythonRuntime(options.workspaceRoot, workerEgress)
            : undefined;
        return module.createPythonCommand(name, pythonRuntime, filesystem);
    };
    const runtime = await createJustBashRuntime({
        filesystem,
        cwd: THREAD_GIT_DIRECTORY,
        env: {
            HOME: THREAD_GIT_DIRECTORY,
            PWD: THREAD_GIT_DIRECTORY,
            GIT_AUTHOR_NAME: THREAD_GIT_AUTHOR.name,
            GIT_AUTHOR_EMAIL: THREAD_GIT_AUTHOR.email,
            GIT_COMMITTER_NAME: THREAD_GIT_AUTHOR.name,
            GIT_COMMITTER_EMAIL: THREAD_GIT_AUTHOR.email,
            PATH: THREAD_GIT_DIRECTORY,
        },
        fetch: shellFetch,
        networkMode: "connector-http-gateway",
        customCommands: ({ defineCommand }) => [
          gitCommand(rawFs, thread, filesystem, defineCommand, createTwoFilesPatch),
          createGhCompatibilityCommand(rawFs, thread, defineCommand, {
              fetch: shellFetch,
          }),
          unameCommand(defineCommand),
          ...["python3", "python"].map((name) => ({
              name,
              load: () => loadPython(name),
          })),
          {
              name: "ssh",
              load: async () => (await import("./browserSsh.mjs")).createSshCommand(filesystem),
          },
          ...["clang", "clang++", "gcc", "g++", "cc", "c++"].map((name) => ({
              name,
              load: async () => (await import("./browserCompiler.mjs")).createCompilerCommand(
                  name,
                  filesystem,
                  workerEgress,
              ),
          })),
        ],
        executionTimeoutMs,
        defaultMaxOutputTokens: 10_000,
        maxOutputTokens: 100_000,
        executionLimits: {
            maxCommandCount: 10_000,
            maxExecutionTimeMs: executionTimeoutMs,
            maxFileSystemBytes: 256 * 1024 * 1024,
            maxInputBytes: 16 * 1024 * 1024,
            maxLiveBytes: 64 * 1024 * 1024,
            maxOutputSize: MAX_OUTPUT_BYTES,
            maxSourceBytes: 1024 * 1024,
            maxStringLength: 16 * 1024 * 1024,
            maxTraversalEntries: 100_000,
        },
        supportsParallelToolCalls: true,
        instructions: browserInstructions,
        outputTruncationNotice: "\n[output truncated by browser exec_command]",
        retainNoticeWithinLimit: false,
        aroundExecute: ({ execute, signal }) => withThreadGitLock(thread, async () => {
            const mutationVersion = filesystem.mutationVersion;
            try {
                return await execute();
            }
            finally {
                if (filesystem.mutationVersion !== mutationVersion)
                    (options.onChanged ?? (() => notifyThreadGitChanged(thread)))();
            }
        }, signal),
    });
    return Object.freeze({ ...runtime, filesystem });
}
const unavailableBrowserEgress = async () => {
    throw new Error("browser egress is unavailable outside the Nanocodex browser host");
};
function unameCommand(defineCommand) {
    return defineCommand("uname", async (args) => {
        const fields = {
            s: "Nanocodex",
            n: "browser",
            r: "1.0.0",
            v: "browser-wasm",
            m: "wasm32",
            p: "wasm32",
            i: "wasm32",
            o: "Browser",
        };
        if (args.includes("--help")) {
            return ok("usage: uname [-asnrvmpio]\n");
        }
        const requested = [];
        for (const arg of args.length ? args : ["-s"]) {
            if (arg === "--all")
                requested.push("s", "n", "r", "v", "m", "p", "i", "o");
            else if (/^-[asnrvmpio]+$/.test(arg)) {
                for (const flag of arg.slice(1)) {
                    if (flag === "a")
                        requested.push("s", "n", "r", "v", "m", "p", "i", "o");
                    else
                        requested.push(flag);
                }
            }
            else {
                return fail(`uname: unrecognized option '${arg}'\n`, 1);
            }
        }
        return ok(`${[...new Set(requested)].map((key) => fields[key]).join(" ")}\n`);
    });
}
function browserInstructions(descriptor) {
    return `You are working in a persistent browser filesystem rooted at ${descriptor.cwd}.
Use exec_command for shell work. Available commands: ${descriptor.commands.join(", ")}. The shell,
Python runtime, and compilers execute entirely in browser sandboxes, with no host process, PTY,
session, or sandbox escalation. HTTP commands use one thread-scoped, same-origin egress gateway.
Destination policy and connected-account credentials stay outside the browser runtime. The C/C++
commands compile sources to WASI WebAssembly in a lazy worker. Browser SSH is noninteractive and
requires a wss:// endpoint that carries raw SSH because browsers cannot open TCP sockets. The
accountInfo tool lists the grant-approved Gmail and Google Drive accounts. When more than one is
available, choose the appropriate account by label and send its id in the
X-Nanocodex-Connector-Connection header on that provider request. Never invent an account id.
The
repository's only publish branch is nanocodex; publish with git add, git commit -m "...", and git
push origin nanocodex. Use the standard Rust apply_patch tool for focused edits. Create or update
custom React interfaces with the render_artifact tool. Its source defines function App({ sendPrompt });
React and the html tagged template helper are already in scope.`;
}
function gitCommand(fs, thread, shellFs, defineCommand, createTwoFilesPatch) {
    return defineCommand("git", async (args, context) => {
        try {
            const command = args[0];
            switch (command) {
                case undefined:
                case "help":
                case "--help":
                    return ok(gitHelp());
                case "status":
                    return ok(await gitStatus(fs, thread, args.slice(1)));
                case "add": {
                    const output = await gitAdd(fs, args.slice(1), context.cwd, () => shellFs.recordRepositoryMutation());
                    return ok(output);
                }
                case "commit": {
                    const output = await gitCommit(fs, args.slice(1));
                    shellFs.recordRepositoryMutation();
                    return ok(output);
                }
                case "push":
                    return ok(await gitPush(fs, thread, args.slice(1)));
                case "pull": {
                    const output = await gitPull(fs, thread, args.slice(1));
                    await shellFs.refreshPaths();
                    shellFs.recordRepositoryMutation();
                    return ok(output);
                }
                case "log":
                    return ok(await gitLog(fs, args.slice(1)));
                case "diff":
                    return ok(await gitDiff(fs, args.slice(1), createTwoFilesPatch));
                case "branch":
                    return ok(gitBranch(thread, args.slice(1)));
                case "rev-parse":
                    return ok(await gitRevParse(fs, thread, args.slice(1)));
                case "remote":
                    return ok(gitRemote(thread, args.slice(1)));
                case "ls-files":
                    return ok(`${(await git.listFiles({ fs, dir: THREAD_GIT_DIRECTORY })).join("\n")}\n`);
                default:
                    return fail(`git: '${command}' is not implemented by browser git\n${gitHelp()}`, 1);
            }
        }
        catch (error) {
            return fail(`git: ${errorMessage(error)}\n`, 1);
        }
    });
}
export function createGhCompatibilityCommand(fs, thread, defineCommand, options = {}) {
    return defineCommand("gh", async (args) => {
        try {
            if (!args.length || args[0] === "help" || args[0] === "--help") {
                return ok(ghHelp());
            }
            if (args[0] === "repo" && args[1] === "view") {
                const repository = repositoryArgument(args.slice(2));
                if (repository) {
                    const value = await githubConnectorJson(options, `/repos/${repository}`);
                    return ok(formatRepository(value));
                }
                const head = await resolveHead(fs);
                return ok([
                    `name:\t${thread.repositoryName}`,
                    `branch:\t${thread.branch}`,
                    `head:\t${head ?? "unborn"}`,
                    `remote:\t${thread.remoteUrl}`,
                    `share:\t${thread.shareUrl}`,
                    "",
                ].join("\n"));
            }
            if (args[0] === "auth" && args[1] === "status") {
                const user = await githubConnectorJson(options, "/user");
                return ok(`Logged in to github.com as ${textField(user, "login")} through the connected account.\n`);
            }
            if (args[0] === "api") {
                return ok(`${JSON.stringify(await githubApi(options, args.slice(1)), null, 2)}\n`);
            }
            if (args[0] === "pr" && args[1] === "list") {
                const repository = optionValue(args.slice(2), "--repo", "-R");
                if (!repository || !validRepository(repository)) {
                    return fail("gh: pr list requires --repo OWNER/REPO in the browser runtime\n", 1);
                }
                const limit = boundedLimit(optionValue(args.slice(2), "--limit", "-L"));
                const pulls = await githubConnectorJson(
                    options,
                    `/repos/${repository}/pulls?${new URLSearchParams({ state: "open", per_page: String(limit) })}`,
                );
                if (!Array.isArray(pulls))
                    throw new Error("GitHub returned an invalid pull request list");
                return ok(pulls.map((pull) => [
                    numberField(pull, "number"),
                    textField(pull, "title"),
                    textField(pull, "head", "ref"),
                ].join("\t")).join("\n") + (pulls.length ? "\n" : ""));
            }
            return fail(`gh: unsupported browser operation '${args.join(" ")}'\n${ghHelp()}`, 1);
        }
        catch (error) {
            return fail(`gh: ${errorMessage(error)}\n`, 1);
        }
    });
}
function ghHelp() {
    return [
        "gh (Nanocodex browser compatibility command)",
        "",
        "Supported commands:",
        "  gh auth status",
        "  gh api [--method METHOD] [-f key=value] ENDPOINT",
        "  gh repo view [OWNER/REPO]",
        "  gh pr list --repo OWNER/REPO [--limit N]",
        "",
        "Connected GitHub calls use the permissions granted in Profile.",
        "",
    ].join("\n");
}
async function githubApi(options, args) {
    const endpoint = args.find((arg, index) => !arg.startsWith("-")
        && args[index - 1] !== "--method" && args[index - 1] !== "-X"
        && !["-f", "-F", "--field", "--raw-field"].includes(args[index - 1]));
    if (!endpoint)
        throw new Error("gh api requires an endpoint");
    let url;
    try {
        url = new URL(endpoint.startsWith("/") ? endpoint : `/${endpoint}`, "https://api.github.com");
    }
    catch {
        throw new Error("gh api endpoint is invalid");
    }
    if (url.origin !== "https://api.github.com")
        throw new Error("browser gh api permits only api.github.com endpoints");
    const fields = githubApiFields(args);
    const hasFields = Object.keys(fields).length > 0;
    const method = (optionValue(args, "--method", "-X") ?? (hasFields ? "POST" : "GET")).toUpperCase();
    if (method === "GET" || method === "HEAD") {
        for (const [key, value] of Object.entries(fields))
            url.searchParams.set(key, value);
    }
    return githubConnectorJson(options, `${url.pathname}${url.search}`, {
        method,
        ...(hasFields && method !== "GET" && method !== "HEAD"
            ? { body: JSON.stringify(fields) }
            : {}),
    });
}
async function githubConnectorJson(options, path, request = {}) {
    if (typeof options.fetch !== "function")
        throw new Error("the managed GitHub connector is unavailable outside the Nanocodex browser host");
    const providerUrl = new URL(path, "https://api.github.com");
    if (providerUrl.origin !== "https://api.github.com")
        throw new Error("GitHub endpoint is outside api.github.com");
    const response = await options.fetch(providerUrl.href, {
        method: request.method ?? "GET",
        headers: {
            accept: "application/json",
            ...(request.body === undefined ? {} : { "content-type": "application/json" }),
        },
        ...(request.body === undefined ? {} : { body: request.body }),
    });
    const text = utf8Decoder.decode(response.body);
    let value;
    try {
        value = text ? JSON.parse(text) : null;
    }
    catch {
        throw new Error(`GitHub connector returned invalid JSON (HTTP ${response.status})`);
    }
    if (response.status < 200 || response.status >= 300) {
        const detail = value && typeof value === "object"
            ? value.message ?? value.error
            : undefined;
        throw new Error(`GitHub connector request failed (HTTP ${response.status}${detail ? `: ${detail}` : ""})`);
    }
    return value;
}
function githubApiFields(args) {
    const fields = {};
    for (let index = 0; index < args.length; index += 1) {
        if (!["-f", "-F", "--field", "--raw-field"].includes(args[index]))
            continue;
        const field = args[index + 1];
        const separator = field?.indexOf("=") ?? -1;
        if (!field || separator <= 0)
            throw new Error(`${args[index]} requires key=value`);
        fields[field.slice(0, separator)] = field.slice(separator + 1);
        index += 1;
    }
    return fields;
}
function repositoryArgument(args) {
    const flagged = optionValue(args, "--repo", "-R");
    const positional = args.find((arg, index) => !arg.startsWith("-")
        && args[index - 1] !== "--repo" && args[index - 1] !== "-R");
    const repository = flagged ?? positional;
    if (repository && !validRepository(repository))
        throw new Error("repository must be OWNER/REPO");
    return repository;
}
function validRepository(value) {
    return /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(value);
}
function optionValue(args, long, short) {
    for (let index = 0; index < args.length; index += 1) {
        const arg = args[index];
        if (arg === long || arg === short)
            return args[index + 1];
        if (arg.startsWith(`${long}=`))
            return arg.slice(long.length + 1);
    }
    return undefined;
}
function boundedLimit(value) {
    if (value === undefined)
        return 30;
    const parsed = Number(value);
    if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 100)
        throw new Error("--limit must be an integer from 1 to 100");
    return parsed;
}
function formatRepository(value) {
    return [
        `name:\t${textField(value, "full_name")}`,
        `description:\t${optionalTextField(value, "description") ?? ""}`,
        `visibility:\t${value?.private === true ? "private" : "public"}`,
        `default branch:\t${textField(value, "default_branch")}`,
        `url:\t${textField(value, "html_url")}`,
        "",
    ].join("\n");
}
function textField(value, ...path) {
    let current = value;
    for (const part of path)
        current = current?.[part];
    if (typeof current !== "string")
        throw new Error(`GitHub response is missing ${path.join(".")}`);
    return current;
}
function optionalTextField(value, ...path) {
    let current = value;
    for (const part of path)
        current = current?.[part];
    return typeof current === "string" ? current : undefined;
}
function numberField(value, field) {
    const current = value?.[field];
    if (typeof current !== "number")
        throw new Error(`GitHub response is missing ${field}`);
    return current;
}
async function gitStatus(fs, thread, args) {
    const matrix = await git.statusMatrix({ fs, dir: THREAD_GIT_DIRECTORY });
    const changed = matrix.filter(([, head, workdir, stage]) => head !== workdir || head !== stage);
    if (args.includes("--short") || args.includes("-s") || args.includes("--porcelain")) {
        return changed.map(([path, head, workdir, stage]) => {
            const code = head === 0 && stage === 0 && workdir !== 0
                ? "??"
                : `${indexCode(head, stage)}${worktreeCode(stage, workdir)}`;
            return `${code} ${path}`;
        }).join("\n") + (changed.length ? "\n" : "");
    }
    const head = await resolveHead(fs);
    if (!changed.length)
        return `On branch ${thread.branch}\nnothing to commit, working tree clean\n`;
    return [
        `On branch ${thread.branch}`,
        head ? "Changes not staged or staged for commit:" : "No commits yet",
        ...changed.map(([path, headStatus, workdirStatus, stageStatus]) => `  ${describeStatus(headStatus, workdirStatus, stageStatus)}: ${path}`),
        "",
    ].join("\n");
}
async function gitAdd(fs, args, cwd, onStaged) {
    const requested = args.filter((arg) => !arg.startsWith("-"));
    if (!requested.length && !args.includes("-A") && !args.includes("--all")) {
        throw new Error("nothing specified, nothing added");
    }
    const matrix = await git.statusMatrix({ fs, dir: THREAD_GIT_DIRECTORY });
    const all = args.includes("-A") || args.includes("--all") || requested.includes(".");
    const prefixes = requested.filter((path) => path !== ".").map((path) => repositoryPath(path, cwd));
    const selected = matrix.filter(([path]) => all || prefixes.some((prefix) => path === prefix || path.startsWith(`${prefix}/`)));
    if (!all && selected.length === 0)
        throw new Error(`pathspec '${requested.join(" ")}' did not match any files`);
    let staged = false;
    try {
        for (const [filepath, , workdirStatus] of selected) {
            if (workdirStatus === 0)
                await git.remove({ fs, dir: THREAD_GIT_DIRECTORY, filepath });
            else
                await git.add({ fs, dir: THREAD_GIT_DIRECTORY, filepath });
            staged = true;
        }
    }
    finally {
        if (staged)
            onStaged();
    }
    return "";
}
async function gitCommit(fs, args) {
    const messageIndex = args.findIndex((arg) => arg === "-m" || arg === "--message");
    const message = messageIndex >= 0 ? args[messageIndex + 1] : undefined;
    if (!message?.trim())
        throw new Error("a commit message is required (use -m)");
    const matrix = await git.statusMatrix({ fs, dir: THREAD_GIT_DIRECTORY });
    if (!matrix.some(([, head, , stage]) => head !== stage))
        throw new Error("nothing to commit");
    const oid = await git.commit({
        fs,
        dir: THREAD_GIT_DIRECTORY,
        message,
        author: THREAD_GIT_AUTHOR,
    });
    return `[nanocodex ${oid.slice(0, 7)}] ${message}\n`;
}
async function gitPush(fs, thread, args) {
    assertRemoteAndBranch(args, thread);
    const head = await resolveHead(fs);
    if (!head)
        throw new Error("the current branch has no commits");
    await git.push({
        fs,
        http,
        dir: THREAD_GIT_DIRECTORY,
        remote: "origin",
        ref: thread.branch,
        remoteRef: thread.branch,
    });
    return `To ${thread.remoteUrl}\n   ${head.slice(0, 7)}  ${thread.branch} -> ${thread.branch}\n`;
}
async function gitPull(fs, thread, args) {
    assertRemoteAndBranch(args, thread);
    await git.pull({
        fs,
        http,
        dir: THREAD_GIT_DIRECTORY,
        remote: "origin",
        ref: thread.branch,
        author: THREAD_GIT_AUTHOR,
    });
    return `Pulled origin/${thread.branch}.\n`;
}
async function gitLog(fs, args) {
    const countArgument = args.find((arg) => /^-\d+$/.test(arg));
    const depth = countArgument ? Number(countArgument.slice(1)) : 20;
    if (!Number.isSafeInteger(depth) || depth > MAX_GIT_LOG_DEPTH) {
        throw new Error(`browser git log depth cannot exceed ${MAX_GIT_LOG_DEPTH}`);
    }
    const commits = await git.log({ fs, dir: THREAD_GIT_DIRECTORY, depth }).catch(() => []);
    if (args.includes("--oneline")) {
        return commits.map(({ oid, commit }) => `${oid.slice(0, 7)} ${firstLine(commit.message)}`).join("\n") + (commits.length ? "\n" : "");
    }
    return commits.map(({ oid, commit }) => [
        `commit ${oid}`,
        `Author: ${commit.author.name} <${commit.author.email}>`,
        `Date:   ${new Date(commit.author.timestamp * 1000).toISOString()}`,
        "",
        `    ${commit.message.trim().replace(/\n/g, "\n    ")}`,
        "",
    ].join("\n")).join("\n");
}
async function gitDiff(fs, args, createTwoFilesPatch) {
    if (args.includes("--cached") || args.includes("--staged")) {
        throw new Error("--cached is not implemented by browser git yet");
    }
    const head = await resolveHead(fs);
    const matrix = await git.statusMatrix({ fs, dir: THREAD_GIT_DIRECTORY });
    const requested = args.filter((arg) => !arg.startsWith("-") && arg !== "--");
    const files = matrix.filter(([path, headStatus, workdirStatus]) => headStatus !== workdirStatus && (!requested.length || requested.includes(path)));
    const patches = [];
    let outputLength = 0;
    for (const [filepath, headStatus, workdirStatus] of files) {
        const worktreePath = `${THREAD_GIT_DIRECTORY}/${filepath}`;
        const worktreeTooLarge = workdirStatus !== 0 &&
            (await fs.promises.stat(worktreePath)).size > MAX_DIFF_FILE_BYTES;
        const beforeBytes = head && headStatus !== 0 && !worktreeTooLarge
            ? (await git.readBlob({ fs, dir: THREAD_GIT_DIRECTORY, oid: head, filepath })).blob
            : undefined;
        const afterBytes = workdirStatus !== 0 && !worktreeTooLarge
            ? await fs.promises.readFile(worktreePath)
            : undefined;
        const patch = worktreeTooLarge ||
            (beforeBytes?.byteLength ?? 0) > MAX_DIFF_FILE_BYTES ||
            (afterBytes?.byteLength ?? 0) > MAX_DIFF_FILE_BYTES ||
            beforeBytes?.includes(0) ||
            afterBytes?.includes(0)
            ? binaryFilePatch(filepath, headStatus, workdirStatus)
            : textFilePatch(filepath, headStatus, workdirStatus, beforeBytes, afterBytes, createTwoFilesPatch);
        const separatorLength = patches.length ? 1 : 0;
        const remaining = MAX_OUTPUT_BYTES - outputLength - separatorLength;
        if (patch.length > remaining) {
            if (remaining > DIFF_TRUNCATION_NOTICE.length) {
                patches.push(`${patch.slice(0, remaining - DIFF_TRUNCATION_NOTICE.length)}${DIFF_TRUNCATION_NOTICE}`);
            }
            else if (outputLength + DIFF_TRUNCATION_NOTICE.length <= MAX_OUTPUT_BYTES) {
                patches.push(DIFF_TRUNCATION_NOTICE);
            }
            break;
        }
        patches.push(patch);
        outputLength += separatorLength + patch.length;
    }
    return patches.join("\n");
}
function textFilePatch(filepath, headStatus, workdirStatus, beforeBytes, afterBytes, createTwoFilesPatch) {
    let before;
    let after;
    try {
        before = beforeBytes ? diffDecoder.decode(beforeBytes) : "";
        after = afterBytes ? diffDecoder.decode(afterBytes) : "";
    }
    catch {
        return binaryFilePatch(filepath, headStatus, workdirStatus);
    }
    return createTwoFilesPatch(headStatus === 0 ? "/dev/null" : `a/${filepath}`, workdirStatus === 0 ? "/dev/null" : `b/${filepath}`, before, after, "HEAD", "worktree");
}
function binaryFilePatch(filepath, headStatus, workdirStatus) {
    const before = headStatus === 0 ? "/dev/null" : `a/${filepath}`;
    const after = workdirStatus === 0 ? "/dev/null" : `b/${filepath}`;
    return `diff --git a/${filepath} b/${filepath}\nBinary files ${before} and ${after} differ\n`;
}
function gitBranch(thread, args) {
    if (!args.length || args.includes("--list"))
        return `* ${thread.branch}\n`;
    if (args.includes("--show-current"))
        return `${thread.branch}\n`;
    throw new Error("browser git exposes only the nanocodex branch");
}
async function gitRevParse(fs, thread, args) {
    if (args.includes("--show-toplevel"))
        return `${THREAD_GIT_DIRECTORY}\n`;
    if (args.includes("--abbrev-ref") && args.includes("HEAD"))
        return `${thread.branch}\n`;
    if (args.length === 1 && args[0] === "HEAD") {
        const head = await resolveHead(fs);
        if (!head)
            throw new Error("ambiguous argument 'HEAD': unknown revision");
        return `${head}\n`;
    }
    throw new Error("unsupported rev-parse arguments");
}
function gitRemote(thread, args) {
    if (!args.length)
        return "origin\n";
    if (args.length === 1 && (args[0] === "-v" || args[0] === "--verbose")) {
        return `origin\t${thread.remoteUrl} (fetch)\norigin\t${thread.remoteUrl} (push)\n`;
    }
    if (args[0] === "get-url" && args[1] === "origin")
        return `${thread.remoteUrl}\n`;
    throw new Error("only the origin remote is available");
}
function assertRemoteAndBranch(args, thread) {
    const positional = args.filter((arg) => !arg.startsWith("-"));
    const remote = positional[0] ?? "origin";
    const branch = positional[1] ?? thread.branch;
    if (remote !== "origin")
        throw new Error("only the origin remote is available");
    if (branch !== thread.branch && branch !== `HEAD:${thread.branch}`) {
        throw new Error(`only branch ${thread.branch} is available`);
    }
}
async function resolveHead(fs) {
    return git.resolveRef({ fs, dir: THREAD_GIT_DIRECTORY, ref: "HEAD" }).catch(() => undefined);
}
function indexCode(head, stage) {
    if (head === stage)
        return " ";
    if (stage === 0)
        return "D";
    if (head === 0)
        return "A";
    return "M";
}
function worktreeCode(stage, workdir) {
    if (stage === workdir)
        return " ";
    if (workdir === 0)
        return "D";
    if (stage === 0)
        return "?";
    return "M";
}
function describeStatus(head, workdir, stage) {
    if (head !== stage)
        return stage === 0 ? "deleted" : head === 0 ? "new file" : "modified";
    return workdir === 0 ? "deleted" : head === 0 ? "untracked" : "modified";
}
function repositoryPath(path, cwd) {
    const absolute = resolveWorkspacePath(cwd, path);
    if (absolute === THREAD_GIT_DIRECTORY)
        return "";
    return absolute.slice(THREAD_GIT_DIRECTORY.length + 1);
}
function gitHelp() {
    return [
        "usage: git <command> [<args>]",
        "",
        "Browser Git commands: status, add, commit, diff, log, branch, rev-parse,",
        "remote, ls-files, pull, and push. The repository is /workspace and its",
        "publish branch is nanocodex (git push origin nanocodex).",
        "",
    ].join("\n");
}
function ok(stdout) {
    return { stdout, stderr: "", exitCode: 0 };
}
function fail(stderr, exitCode) {
    return { stdout: "", stderr, exitCode };
}
class OpfsShellFileSystem {
    #fs;
    #paths = new Set([THREAD_GIT_DIRECTORY]);
    #sortedPaths;
    #mutationVersion = 0;
    constructor(fs) {
        this.#fs = fs;
    }
    get mutationVersion() {
        return this.#mutationVersion;
    }
    async refreshPaths() {
        const paths = new Set([THREAD_GIT_DIRECTORY]);
        await this.#visit(THREAD_GIT_DIRECTORY, paths);
        this.#paths = paths;
        this.#sortedPaths = undefined;
    }
    recordExternalWrite(path) {
        this.#recordMutation();
        this.#addPath(resolveWorkspacePath(THREAD_GIT_DIRECTORY, path));
    }
    recordExternalRemove(path) {
        this.#recordMutation();
        this.#removePath(resolveWorkspacePath(THREAD_GIT_DIRECTORY, path));
    }
    recordRepositoryMutation() {
        this.#recordMutation();
    }
    async readFile(path, options) {
        const bytes = await this.readFileBuffer(path);
        const encoding = typeof options === "string" ? options : options?.encoding ?? "utf8";
        return decode(bytes, encoding);
    }
    async readFileBytes(path) {
        return bytesToLatin1(await this.readFileBuffer(path));
    }
    async readFileBuffer(path) {
        const absolute = resolveShellPath(THREAD_GIT_DIRECTORY, path);
        if (absolute === "/dev/null")
            return new Uint8Array();
        const value = await this.#fs.promises.readFile(absolute);
        return value instanceof Uint8Array ? value : utf8.encode(value);
    }
    async writeFile(path, content, options) {
        const absolute = resolveShellPath(THREAD_GIT_DIRECTORY, path);
        if (absolute === "/dev/null")
            return;
        await this.#fs.promises.writeFile(absolute, encode(content, options));
        this.#recordMutation();
        this.#addPath(absolute);
    }
    async appendFile(path, content, options) {
        const absolute = resolveShellPath(THREAD_GIT_DIRECTORY, path);
        if (absolute === "/dev/null")
            return;
        await this.#fs.promises.appendFile(absolute, encode(content, options));
        this.#recordMutation();
        this.#addPath(absolute);
    }
    async exists(path) {
        let absolute;
        try {
            absolute = resolveShellPath(THREAD_GIT_DIRECTORY, path);
        }
        catch (error) {
            if (isCode(error, "EPERM"))
                return false;
            throw error;
        }
        if (isShellDevice(absolute))
            return true;
        return this.#fs.promises.stat(absolute).then(() => true, () => false);
    }
    async stat(path) {
        const absolute = resolveShellPath(THREAD_GIT_DIRECTORY, path);
        if (isShellDevice(absolute)) {
            return {
                isFile: true,
                isDirectory: false,
                isSymbolicLink: false,
                mode: 0o666,
                size: 0,
                mtime: new Date(0),
            };
        }
        const result = await this.#fs.promises.stat(absolute);
        return {
            isFile: result.isFile(),
            isDirectory: result.isDirectory(),
            isSymbolicLink: result.isSymbolicLink(),
            mode: result.mode,
            size: result.size,
            mtime: new Date(result.mtimeMs),
        };
    }
    async mkdir(path) {
        const absolute = resolveWorkspacePath(THREAD_GIT_DIRECTORY, path);
        await this.#fs.promises.mkdir(absolute);
        this.#recordMutation();
        this.#addPath(absolute);
    }
    async readdir(path) {
        return this.#fs.promises.readdir(resolveWorkspacePath(THREAD_GIT_DIRECTORY, path));
    }
    async readdirWithFileTypes(path) {
        const absolute = resolveWorkspacePath(THREAD_GIT_DIRECTORY, path);
        return this.#fs.promises.readdirWithFileTypes(absolute);
    }
    async rm(path, options) {
        const absolute = resolveWorkspacePath(THREAD_GIT_DIRECTORY, path);
        let removed = false;
        try {
            await this.#fs.promises.rm(absolute, { recursive: options?.recursive });
            removed = true;
        }
        catch (error) {
            if (!options?.force || !isCode(error, "ENOENT"))
                throw error;
        }
        if (removed) {
            this.#recordMutation();
            this.#removePath(absolute);
        }
    }
    async cp(src, dest, options) {
        const source = resolveWorkspacePath(THREAD_GIT_DIRECTORY, src);
        const target = resolveWorkspacePath(THREAD_GIT_DIRECTORY, dest);
        const sourceStat = await this.stat(source);
        if (sourceStat.isDirectory) {
            if (!options?.recursive)
                throw fsError("EISDIR", "copying a directory requires recursive mode");
            await this.mkdir(target);
            for (const name of await this.readdir(source)) {
                await this.cp(`${source}/${name}`, `${target}/${name}`, options);
            }
            return;
        }
        await this.writeFile(target, await this.readFileBuffer(source));
    }
    async mv(src, dest) {
        const source = resolveWorkspacePath(THREAD_GIT_DIRECTORY, src);
        await this.cp(source, dest, { recursive: true });
        await this.rm(source, { recursive: true });
    }
    resolvePath(base, path) {
        return resolveShellPath(base, path);
    }
    getAllPaths() {
        this.#sortedPaths ??= [...this.#paths].sort();
        return this.#sortedPaths.slice();
    }
    async chmod(path) {
        await this.stat(path);
    }
    async symlink() {
        throw fsError("ENOSYS", "OPFS does not support symbolic links");
    }
    async link() {
        throw fsError("ENOSYS", "OPFS does not support hard links");
    }
    async readlink() {
        throw fsError("ENOSYS", "OPFS does not support symbolic links");
    }
    lstat(path) {
        return this.stat(path);
    }
    async realpath(path) {
        const absolute = resolveWorkspacePath(THREAD_GIT_DIRECTORY, path);
        await this.stat(absolute);
        return absolute;
    }
    async utimes(path) {
        await this.stat(path);
    }
    async #visit(directory, paths) {
        for (const entry of await this.readdirWithFileTypes(directory)) {
            if (directory === THREAD_GIT_DIRECTORY && entry.name === ".git")
                continue;
            const path = `${directory}/${entry.name}`;
            this.#addIndexedPath(paths, path);
            if (entry.isDirectory)
                await this.#visit(path, paths);
        }
    }
    #addPath(path) {
        const gitDirectory = `${THREAD_GIT_DIRECTORY}/.git`;
        if (path === gitDirectory || path.startsWith(`${gitDirectory}/`))
            return;
        const segments = path.slice(THREAD_GIT_DIRECTORY.length + 1).split("/");
        let current = THREAD_GIT_DIRECTORY;
        let changed = false;
        for (const segment of segments) {
            if (!segment)
                continue;
            current += `/${segment}`;
            changed = this.#addIndexedPath(this.#paths, current) || changed;
        }
        if (changed)
            this.#sortedPaths = undefined;
    }
    #addIndexedPath(paths, path) {
        if (paths.has(path))
            return false;
        if (paths.size >= MAX_INDEXED_PATHS) {
            throw fsError("EFBIG", `browser shell path index exceeds ${MAX_INDEXED_PATHS} entries`);
        }
        paths.add(path);
        return true;
    }
    #removePath(path) {
        let changed = false;
        for (const candidate of this.#paths) {
            if (candidate === path || candidate.startsWith(`${path}/`)) {
                this.#paths.delete(candidate);
                changed = true;
            }
        }
        if (changed)
            this.#sortedPaths = undefined;
    }
    #recordMutation() {
        this.#mutationVersion += 1;
    }
}
const SHELL_DEVICES = new Set(["/dev/full", "/dev/null", "/dev/stderr", "/dev/stdout"]);
function isShellDevice(path) {
    return SHELL_DEVICES.has(path);
}
function resolveShellPath(base, path) {
    if (isShellDevice(path))
        return path;
    return resolveWorkspacePath(base, path);
}
function resolveWorkspacePath(base, path) {
    if (typeof path !== "string" || path.includes("\0"))
        throw fsError("EINVAL", "invalid path");
    const source = path.startsWith("/") ? path : `${base}/${path}`;
    const segments = [];
    for (const segment of source.replace(/\\/g, "/").split("/")) {
        if (!segment || segment === ".")
            continue;
        if (segment === "..")
            segments.pop();
        else
            segments.push(segment);
    }
    const absolute = `/${segments.join("/")}`;
    if (absolute !== THREAD_GIT_DIRECTORY && !absolute.startsWith(`${THREAD_GIT_DIRECTORY}/`)) {
        throw fsError("EPERM", `path escapes ${THREAD_GIT_DIRECTORY}`);
    }
    return absolute;
}
function encode(content, options) {
    if (content instanceof Uint8Array)
        return content;
    const encoding = typeof options === "string" ? options : options?.encoding ?? "utf8";
    if (encoding === "base64")
        return Uint8Array.from(atob(content), (character) => character.charCodeAt(0));
    if (encoding === "hex") {
        if (content.length % 2 !== 0 || !/^[a-f0-9]*$/i.test(content))
            throw fsError("EINVAL", "invalid hex input");
        return Uint8Array.from(content.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
    }
    if (encoding === "binary" || encoding === "latin1" || encoding === "ascii") {
        return Uint8Array.from(content, (character) => character.charCodeAt(0) & 0xff);
    }
    return utf8.encode(content);
}
function decode(bytes, encoding) {
    if (encoding === "base64")
        return btoa(bytesToLatin1(bytes));
    if (encoding === "hex")
        return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
    if (encoding === "binary" || encoding === "latin1")
        return bytesToLatin1(bytes);
    if (encoding === "ascii")
        return String.fromCharCode(...bytes.map((byte) => byte & 0x7f));
    return utf8Decoder.decode(bytes);
}
function bytesToLatin1(bytes) {
    let output = "";
    for (let offset = 0; offset < bytes.length; offset += 32_768) {
        output += String.fromCharCode(...bytes.subarray(offset, offset + 32_768));
    }
    return output;
}
function firstLine(value) {
    return value.trim().split("\n", 1)[0] ?? "";
}
function flagValue(args, flag) {
    const index = args.indexOf(flag);
    return index < 0 ? undefined : args[index + 1];
}
function errorMessage(error) {
    return error instanceof Error ? error.message : String(error);
}
function isCode(error, code) {
    return Boolean(error && typeof error === "object" && "code" in error && error.code === code);
}
function fsError(code, message) {
    return Object.assign(new Error(message), { code });
}
