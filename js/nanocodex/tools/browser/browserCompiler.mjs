import { defineCommand } from "nanocodex-tools/just-bash/browser";
const runtimes = new WeakMap();
export function createCompilerCommand(name, filesystem, egress) {
    const runtime = runtimes.get(filesystem) ?? new BrowserCompilerRuntime(egress);
    runtimes.set(filesystem, runtime);
    return defineCommand(name, async (args, context) => {
        const parsed = parseArguments(name, args, context);
        if ("result" in parsed)
            return parsed.result;
        try {
            const files = await collectCompilerFiles(filesystem, context.cwd, parsed.sources);
            const result = await runtime.execute({ ...parsed, files }, context.signal);
            if (result.exitCode === 0 && result.output) {
                await filesystem.writeFile(parsed.output, result.output);
            }
            return { stdout: result.stdout, stderr: result.stderr, exitCode: result.exitCode };
        }
        catch (error) {
            return fail(`${name}: ${error instanceof Error ? error.message : String(error)}\n`, context.signal?.aborted ? 124 : 1);
        }
    });
}
class BrowserCompilerRuntime {
    #egress;
    #worker;
    #nextId = 1;
    #queue = Promise.resolve();
    constructor(egress) {
        this.#egress = egress;
    }
    execute(input, signal) {
        const run = this.#queue.then(() => this.#run(input, signal));
        this.#queue = run.then(() => undefined, () => undefined);
        return run;
    }
    #run(input, signal) {
        const worker = this.#worker ?? this.#createWorker();
        const id = this.#nextId++;
        return new Promise((resolve, reject) => {
            const cleanup = () => {
                worker.removeEventListener("message", onMessage);
                worker.removeEventListener("error", onError);
                signal?.removeEventListener("abort", onAbort);
            };
            const onMessage = (event) => {
                if (event.data.id !== id)
                    return;
                cleanup();
                if (event.data.result)
                    resolve(event.data.result);
                else
                    reject(new Error(event.data.error ?? "compiler worker failed"));
            };
            const onError = (event) => {
                cleanup();
                this.#discard(worker);
                reject(new Error(event.message || "compiler worker crashed"));
            };
            const onAbort = () => {
                cleanup();
                this.#discard(worker);
                reject(signal?.reason instanceof Error ? signal.reason : new Error("compilation aborted"));
            };
            worker.addEventListener("message", onMessage);
            worker.addEventListener("error", onError);
            signal?.addEventListener("abort", onAbort, { once: true });
            if (signal?.aborted)
                return onAbort();
            worker.postMessage({ type: "compile", id, input });
        });
    }
    #createWorker() {
        const worker = new Worker(new URL("./compiler.worker.mjs", import.meta.url), { type: "module" });
        worker.postMessage({ type: "initialize", egress: this.#egress });
        this.#worker = worker;
        return worker;
    }
    #discard(worker) {
        worker.terminate();
        if (this.#worker === worker)
            this.#worker = undefined;
    }
}
function parseArguments(name, args, context) {
    if (args.includes("--help")) {
        return { result: ok(`usage: ${name} [-c] [-O0|-O1|-O2|-O3] [-o OUTPUT] SOURCE...\n`) };
    }
    let output = "a.wasm";
    let outputWasSet = false;
    let optimize = "2";
    let compileOnly = false;
    const sources = [];
    for (let index = 0; index < args.length; index += 1) {
        const arg = args[index];
        if (arg === "-c")
            compileOnly = true;
        else if (/^-O[0-3]$/.test(arg))
            optimize = arg.slice(2);
        else if (arg === "-o") {
            const value = args[++index];
            if (!value)
                return { result: fail(`${name}: -o requires a filename\n`, 1) };
            output = context.fs.resolvePath(context.cwd, value);
            outputWasSet = true;
        }
        else if (arg.startsWith("-")) {
            return { result: fail(`${name}: unsupported browser compiler option '${arg}'\n`, 1) };
        }
        else {
            sources.push(context.fs.resolvePath(context.cwd, arg));
        }
    }
    if (!sources.length)
        return { result: fail(`${name}: no input files\n`, 1) };
    if (compileOnly && sources.length > 1) {
        return { result: fail(`${name}: browser compilation does not support -c with multiple source files; compile one source at a time\n`, 1) };
    }
    if (!output.startsWith("/"))
        output = context.fs.resolvePath(context.cwd, output);
    if (compileOnly && !outputWasSet)
        output = sources[0].replace(/\.[^.]+$/, ".o");
    return { sources, output, optimize, compileOnly };
}
async function collectCompilerFiles(filesystem, cwd, sources) {
    const indexed = filesystem;
    const paths = indexed.getAllPaths?.() ?? sources;
    const sourceSet = new Set(sources);
    const relevant = paths.filter((path) => sourceSet.has(path) || /\.(?:h|hh|hpp|hxx|inc)$/.test(path));
    const files = [];
    for (const path of relevant) {
        if (!await filesystem.exists(path))
            continue;
        const stat = await filesystem.stat(path);
        if (!stat.isFile)
            continue;
        const relative = relativeWorkspacePath(path, cwd);
        files.push({ path: relative, contents: await filesystem.readFileBuffer(path) });
    }
    for (const source of sources) {
        if (!files.some((file) => file.path === relativeWorkspacePath(source, cwd))) {
            throw new Error(`input file not found: ${source}`);
        }
    }
    return files;
}
function relativeWorkspacePath(path, cwd) {
    const root = cwd.split("/").slice(0, 2).join("/") || "/workspace";
    return path.startsWith(`${root}/`) ? path.slice(root.length + 1) : path.replace(/^\/+/, "");
}
function ok(stdout) {
    return { stdout, stderr: "", exitCode: 0 };
}
function fail(stderr, exitCode) {
    return { stdout: "", stderr, exitCode };
}
