import { defineCommand } from "nanocodex-tools/just-bash/browser";
export class BrowserPythonRuntime {
    #workspaceRoot;
    #egress;
    #worker;
    #nextId = 1;
    #queue = Promise.resolve();
    constructor(workspaceRoot, egress) {
        this.#workspaceRoot = workspaceRoot;
        this.#egress = egress;
    }
    execute(input, signal) {
        const run = this.#queue.then(() => this.#run(input, signal));
        this.#queue = run.then(() => undefined, () => undefined);
        return run;
    }
    async #run(input, signal) {
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
                // NativeFS can push changes back to OPFS but cannot refresh an existing
                // mount after bash edits. A process-like fresh worker per invocation
                // guarantees each Python command begins from the latest workspace.
                this.#discardWorker(worker);
                if (event.data.result)
                    resolve(event.data.result);
                else
                    reject(new Error(event.data.error ?? "Python worker failed"));
            };
            const onError = (event) => {
                cleanup();
                this.#discardWorker(worker);
                reject(new Error(event.message || "Python worker crashed"));
            };
            const onAbort = () => {
                cleanup();
                this.#discardWorker(worker);
                reject(signal?.reason instanceof Error ? signal.reason : new Error("Python execution aborted"));
            };
            worker.addEventListener("message", onMessage);
            worker.addEventListener("error", onError);
            signal?.addEventListener("abort", onAbort, { once: true });
            if (signal?.aborted)
                return onAbort();
            worker.postMessage({ type: "execute", id, input });
        });
    }
    #createWorker() {
        const worker = new Worker(new URL("./python.worker.mjs", import.meta.url), { type: "module" });
        worker.postMessage({
            type: "initialize",
            workspaceRoot: this.#workspaceRoot,
            egress: this.#egress,
        });
        this.#worker = worker;
        return worker;
    }
    #discardWorker(worker) {
        worker.terminate();
        if (this.#worker === worker)
            this.#worker = undefined;
    }
}
export function createPythonCommand(name, runtime, filesystem) {
    const execute = async (args, context) => {
        const parsed = parsePythonArguments(name, args, String(context.stdin));
        if ("result" in parsed)
            return parsed.result;
        if (!runtime) {
            return {
                stdout: "",
                stderr: `${name}: browser Python is unavailable without an OPFS workspace\n`,
                exitCode: 1,
            };
        }
        try {
            const result = await runtime.execute({
                ...parsed,
                cwd: context.cwd,
                stdin: String(context.stdin),
            }, context.signal);
            await filesystem.refreshPaths?.();
            filesystem.recordRepositoryMutation?.();
            return result;
        }
        catch (error) {
            return {
                stdout: "",
                stderr: `${name}: ${error instanceof Error ? error.message : String(error)}\n`,
                exitCode: context.signal?.aborted ? 124 : 1,
            };
        }
    };
    return defineCommand(name, execute);
}
export function parsePythonArguments(name, args, stdin) {
    if (args[0] === "--help" || args[0] === "-h") {
        return { result: { stdout: `usage: ${name} [-c code | -m module | script | -] [args...]\n`, stderr: "", exitCode: 0 } };
    }
    if (args[0] === "--version" || args[0] === "-V") {
        return { result: { stdout: "Python 3 (Pyodide)\n", stderr: "", exitCode: 0 } };
    }
    if (args[0] === "-c") {
        if (args[1] === undefined)
            return pythonArgumentError(name, "argument expected for -c");
        return { source: args[1], moduleName: null, filename: "<string>", argv: ["-c", ...args.slice(2)] };
    }
    if (args[0] === "-m") {
        if (args[1] === undefined)
            return pythonArgumentError(name, "argument expected for -m");
        return { source: null, moduleName: args[1], filename: args[1], argv: [args[1], ...args.slice(2)] };
    }
    if (!args.length || args[0] === "-") {
        if (!stdin)
            return pythonArgumentError(name, "no input provided (use -c, -m, a script, or stdin)");
        return { source: stdin, moduleName: null, filename: "<stdin>", argv: ["-", ...args.slice(1)] };
    }
    if (args[0].startsWith("-"))
        return pythonArgumentError(name, `unrecognized option '${args[0]}'`);
    return { source: null, moduleName: null, filename: args[0], argv: [args[0], ...args.slice(1)] };
}
function pythonArgumentError(name, message) {
    return { result: { stdout: "", stderr: `${name}: ${message}\n`, exitCode: 2 } };
}
