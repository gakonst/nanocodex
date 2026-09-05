import { spawn } from "node:child_process";
import { realpath, stat } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import {
  EXEC_COMMAND_PARAMETERS,
  WRITE_STDIN_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
} from "./execution-contract.mjs";
import { namedTool } from "./namedTool.mjs";

/** Native, explicitly authorized host execution. The workspace is the initial cwd,
 * not an OS sandbox. Credentials from the embedding process are never inherited. */
export async function createNodeProcessTools({
  workspace,
  onActivity = () => {},
}) {
  const root = await realpath(workspace);
  const sessions = new Map();
  const processes = new Set();
  let sequence = 0;
  let disposed = false;
  const environment = Object.fromEntries(
    ["PATH", "HOME", "USER", "TMPDIR", "LANG", "LC_ALL", "SYSTEMROOT"]
      .filter((key) => process.env[key] !== undefined)
      .map((key) => [key, process.env[key]]),
  );
  if (process.platform === "darwin") {
    // Finder launches apps with a minimal PATH. Discover common installations
    // without sourcing shell profiles or importing their environment secrets.
    const candidates = [
      "/opt/homebrew/bin",
      "/usr/local/bin",
      join(homedir(), ".cargo", "bin"),
      "/opt/homebrew/sbin",
    ];
    const available = await Promise.all(candidates.map(async directory => {
      try { return (await stat(directory)).isDirectory() ? directory : undefined; }
      catch { return undefined; }
    }));
    const inherited = (environment.PATH || "/usr/bin:/bin:/usr/sbin:/sbin").split(":");
    environment.PATH = [...new Set([...inherited, ...available.filter(Boolean)])].join(":");
  }
  environment.TERM = "dumb";
  const kill = (record, signal = "SIGTERM") => {
    if (!record.child.pid) return;
    try {
      process.kill(
        process.platform === "win32" ? record.child.pid : -record.child.pid,
        signal,
      );
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  };
  const stopGroup = async (record, signal) => {
    for (let attempt = 0; ; attempt++) {
      try {
        kill(record, signal);
        return;
      } catch (error) {
        // Darwin can briefly return EPERM for a group whose last member is
        // being reaped. Retry that transition; still report a real refusal.
        if (error.code !== "EPERM" || !record.done || attempt === 4)
          throw error;
        await delay(25);
      }
    }
  };
  const stop = (record) =>
    (record.stopping ??= (async () => {
      await stopGroup(record, "SIGTERM");
      await Promise.race([
        record.closed,
        delay(1_500, undefined, { ref: false }),
      ]);
      // Kill descendants too, even if the shell itself exited first.
      await stopGroup(record, "SIGKILL");
      await record.closed;
      sessions.delete(record.id);
      processes.delete(record);
    })());
  const collect = () => {
    for (const record of processes)
      if (record.done) {
        try {
          if (record.child.pid)
            process.kill(
              process.platform === "win32"
                ? record.child.pid
                : -record.child.pid,
              0,
            );
          else processes.delete(record);
        } catch (error) {
          if (error.code === "ESRCH") processes.delete(record);
        }
      }
  };
  const activity = (event) => {
    try {
      onActivity(event);
    } catch {
      /* Observers cannot interrupt process cleanup. */
    }
  };
  const read = async (record, input, signal) => {
    const started = performance.now();
    const wait = integer(input.yield_time_ms, 1_000, 0, 30_000);
    const maxBytes = integer(input.max_output_tokens, 10_000, 1, 32_000) * 4;
    if (record.reading)
      throw new Error("A read is already pending for this process.");
    record.reading = true;
    try {
      await Promise.race([
        record.closed,
        delay(wait, undefined, { signal, ref: false }),
      ]);
      if (signal?.aborted) throw signal.reason;
      const output = record.buffer.slice(0, maxBytes);
      record.buffer = record.buffer.slice(maxBytes);
      const result = {
        output,
        wall_time_seconds: (performance.now() - started) / 1_000,
      };
      if (record.done && !record.buffer) {
        result.exit_code = record.exitCode;
        sessions.delete(record.id);
        collect();
      } else result.session_id = record.id;
      return result;
    } finally {
      record.reading = false;
    }
  };
  const exec = namedTool("exec_command", {
    description:
      "Run a native command in the authorized host workspace. Supports pipe sessions and write_stdin; PTYs are not available on this hand.",
    parameters: EXEC_COMMAND_PARAMETERS,
    outputSchema: EXECUTION_OUTPUT_SCHEMA,
    supportsParallelToolCalls: true,
    async handler(input, context) {
      if (disposed) throw new Error("This compute hand is stopped.");
      if (!input || typeof input.cmd !== "string" || !input.cmd.trim())
        throw new TypeError("cmd is required");
      if (input.tty)
        throw new Error(
          "This native Hand supports pipes. Use a VM Hand for PTY commands.",
        );
      if (input.sandbox_permissions === "require_escalated")
        throw new Error("This Hand does not provide privilege escalation.");
      integer(input.yield_time_ms, 1_000, 0, 30_000);
      integer(input.max_output_tokens, 10_000, 1, 32_000);
      if (input.workdir !== undefined && typeof input.workdir !== "string")
        throw new TypeError("workdir must be text");
      collect();
      const cwd = await realpath(resolve(root, input.workdir ?? root));
      const path = relative(root, cwd);
      if (path === ".." || path.startsWith(`..${sep}`) || isAbsolute(path))
        throw new Error("workdir is outside this Hand's workspace.");
      const shell =
        input.shell ?? (process.platform === "win32" ? "cmd.exe" : "/bin/sh");
      if (typeof shell !== "string" || !isAbsolute(shell))
        throw new Error("shell must be an absolute executable path.");
      if (disposed) throw new Error("This compute hand is stopped.");
      context.signal?.throwIfAborted();
      const child = spawn(
        shell,
        [input.login === true ? "-lc" : "-c", input.cmd],
        {
          cwd,
          env: environment,
          detached: process.platform !== "win32",
          stdio: ["pipe", "pipe", "pipe"],
        },
      );
      let finish;
      const record = {
        id: ++sequence,
        owner: context.sessionId,
        child,
        buffer: "",
        done: false,
        exitCode: 0,
        reading: false,
        closed: new Promise((resolve) => {
          finish = resolve;
        }),
      };
      sessions.set(record.id, record);
      processes.add(record);
      activity({
        type: "started",
        sessionId: context.sessionId,
        processId: record.id,
      });
      const append = (data) => {
        record.buffer += data.toString();
        if (record.buffer.length > 1_048_576)
          record.buffer =
            record.buffer.slice(0, 1_048_500) + "\n[output truncated]\n";
      };
      child.stdout.on("data", append);
      child.stderr.on("data", append);
      child.stdin.on("error", () => {});
      child.on("error", (error) => {
        append(error.message);
        record.exitCode = 1;
      });
      child.on("close", (code) => {
        record.done = true;
        record.exitCode =
          code !== null && code >= 0 ? code : record.exitCode || 130;
        finish();
        collect();
        activity({
          type: "completed",
          sessionId: context.sessionId,
          processId: record.id,
          exitCode: record.exitCode,
        });
      });
      const abort = () => {
        void stop(record).catch(() => {});
      };
      context.signal?.addEventListener("abort", abort, { once: true });
      if (context.signal?.aborted) abort();
      try {
        return await read(record, input, context.signal);
      } finally {
        context.signal?.removeEventListener("abort", abort);
      }
    },
    releaseSession(owner) {
      for (const record of processes)
        if (record.owner === owner) void stop(record).catch(() => {});
    },
    async dispose() {
      disposed = true;
      await Promise.all([...processes.values()].map(stop));
      sessions.clear();
    },
  });
  const stdin = namedTool("write_stdin", {
    description:
      "Write to or poll a pipe session belonging to this agent on this Hand.",
    parameters: WRITE_STDIN_PARAMETERS,
    outputSchema: EXECUTION_OUTPUT_SCHEMA,
    supportsParallelToolCalls: true,
    async handler(input, context) {
      const record = sessions.get(input?.session_id);
      if (disposed || !record || record.owner !== context.sessionId)
        throw new Error("Process session is unavailable for this agent.");
      if (input.chars !== undefined && typeof input.chars !== "string")
        throw new TypeError("chars must be text");
      integer(input.yield_time_ms, 1_000, 0, 30_000);
      integer(input.max_output_tokens, 10_000, 1, 32_000);
      if (record.reading)
        throw new Error("A read is already pending for this process.");
      context.signal?.throwIfAborted();
      if (input.chars && !record.done) {
        if (input.chars === "\u0003") kill(record, "SIGINT");
        else record.child.stdin.write(input.chars);
      }
      const abort = () => {
        void stop(record).catch(() => {});
      };
      context.signal?.addEventListener("abort", abort, { once: true });
      try {
        return await read(record, input, context.signal);
      } finally {
        context.signal?.removeEventListener("abort", abort);
      }
    },
  });
  return Object.freeze({ tools: [exec, stdin], close: exec.dispose });
}

function integer(value, fallback, minimum, maximum) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum)
    throw new RangeError(
      `Expected an integer between ${minimum} and ${maximum}.`,
    );
  return value;
}
