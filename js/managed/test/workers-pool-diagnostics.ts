// Names the test file whose workers-pool runner never reports completion.
//
// Vitest frees a pool slot, and prints the run summary, only when a runner
// posts "testfileFinished". @cloudflare/vitest-pool-workers 0.19.1 can lose that
// message when Durable Object work is still in flight as a file ends
// (https://github.com/gakonst/nanocodex/issues/951; see durable-object-settle.ts).
// The run then hangs until the CI job timeout, after every file printed a
// result, so the log cannot say which runner is stuck. This plugin only
// observes: once a running file's runner has been silent for QUIET_MS it writes
// one report per file, naming the pending files and that runner's last
// messages, to stderr and to a bounded log under output/. It never ends,
// retries or fails a run, so a stalled run still stalls visibly.
import { appendFileSync, mkdirSync } from "node:fs";
import { relative } from "node:path";
import { fileURLToPath } from "node:url";

const QUIET_MS = positive(process.env.NANOCODEX_WORKERS_POOL_STALL_MS) ?? 120_000;
const LOG_DIR = fileURLToPath(new URL("../../../output/workers-pool-diagnostics/", import.meta.url).href);
const RECENT = 8;
const MAX_REPORTS = 20;

type WorkerMessage = Record<string, unknown> | undefined | null;
type Listener = (...args: unknown[]) => unknown;
type PoolWorker = {
  send(message: unknown): unknown;
  on(event: string, callback: Listener): unknown;
  off(event: string, callback: Listener): unknown;
  stop(): Promise<unknown>;
};
type PoolRunner = { name?: string; createPoolWorker(options: unknown): PoolWorker };
type Running = { id: number; files: string[]; runAt: number; lastAt: number; recent: string[]; reported: boolean };

export function workersPoolDiagnostics() {
  return {
    name: "nanocodex:workers-pool-diagnostics",
    configureVitest(context: { project: { config: { root: string; poolRunner?: PoolRunner } }; vitest: { onClose(fn: () => unknown): void } }) {
      const runner = context.project.config.poolRunner;
      if (runner?.name !== "cloudflare-pool" || typeof runner.createPoolWorker !== "function") {
        throw new Error("workers-pool diagnostics expected the @cloudflare/vitest-pool-workers poolRunner; register it after cloudflareTest() and revisit test/workers-pool-diagnostics.ts after pool upgrades");
      }
      const root = context.project.config.root;
      const running = new Map<number, Running>();
      let nextId = 0;
      let reports = 0;
      let timer: ReturnType<typeof setInterval> | undefined;
      const stopTimer = () => { if (timer) clearInterval(timer); timer = undefined; };
      context.vitest.onClose(stopTimer);

      const check = () => {
        const now = Date.now();
        for (const entry of running.values()) {
          if (entry.reported || now - entry.lastAt < QUIET_MS || reports >= MAX_REPORTS) continue;
          entry.reported = true;
          reports++;
          report(entry, [...running.values()], now);
        }
      };
      const track = (entry: Running) => {
        running.set(entry.id, entry);
        timer ??= setInterval(check, Math.max(250, Math.min(QUIET_MS / 4, 15_000)));
        timer.unref?.();
      };
      const untrack = (id: number) => {
        running.delete(id);
        if (running.size === 0) stopTimer();
      };

      const create = runner.createPoolWorker.bind(runner);
      runner.createPoolWorker = (options: unknown) => {
        const worker = create(options);
        for (const method of ["send", "on", "off", "stop"] as const) {
          if (typeof worker?.[method] !== "function") throw new Error("workers-pool diagnostics: pool worker has no " + method + "(); revisit test/workers-pool-diagnostics.ts");
        }
        const id = ++nextId;
        let entry: Running | undefined;
        const listeners = new Map<Listener, Listener>();
        const send = worker.send.bind(worker), on = worker.on.bind(worker), off = worker.off.bind(worker), stop = worker.stop.bind(worker);
        worker.send = (message: unknown) => {
          const m = message as { type?: string; context?: { files?: Array<{ filepath?: unknown }> } };
          if (m?.type === "run" || m?.type === "collect") {
            const files = m.context?.files;
            if (!Array.isArray(files) || files.some(file => typeof file?.filepath !== "string")) {
              throw new Error("workers-pool diagnostics: unexpected " + m.type + " message shape; revisit test/workers-pool-diagnostics.ts");
            }
            const now = Date.now();
            entry = { id, files: files.map(file => relative(root, file.filepath as string)), runAt: now, lastAt: now, recent: [], reported: false };
            track(entry);
          }
          return send(message);
        };
        worker.on = (event: string, callback: Listener) => {
          if (event !== "message") return on(event, callback);
          const wrapped: Listener = function (this: unknown, ...args: unknown[]) {
            const current = entry;
            if (current) observe(current, args[0] as WorkerMessage, () => { entry = undefined; untrack(id); });
            return callback.apply(this, args);
          };
          listeners.set(callback, wrapped);
          return on(event, wrapped);
        };
        worker.off = (event: string, callback: Listener) => {
          if (event !== "message") return off(event, callback);
          const wrapped = listeners.get(callback);
          if (wrapped) listeners.delete(callback);
          return off(event, wrapped ?? callback);
        };
        worker.stop = (...args: unknown[]) => { entry = undefined; untrack(id); return (stop as (...a: unknown[]) => Promise<unknown>)(...args); };
        return worker;
      };
    },
  };
}

function observe(entry: Running, message: WorkerMessage, finished: () => void) {
  entry.lastAt = Date.now();
  const kind = typeof message !== "object" || message === null ? typeof message
    : message.__vitest_worker_response__ ? String(message.type)
    : typeof message.m === "string" ? "rpc " + message.m
    : "rpc reply";
  if (entry.recent[entry.recent.length - 1] !== kind) {
    entry.recent.push(kind);
    if (entry.recent.length > RECENT) entry.recent.shift();
  }
  if (message?.__vitest_worker_response__ && message.type === "testfileFinished") finished();
}

function report(entry: Running, all: Running[], now: number) {
  const age = (ms: number) => Math.round(ms / 1000) + "s";
  const pending = all.map(other => other.files.join(", ") + " (running " + age(now - other.runAt) + ", silent " + age(now - other.lastAt) + ")");
  const logPath = LOG_DIR + "pid-" + process.pid + ".log";
  const text = "[workers-pool-diagnostics] " + entry.files.join(", ") + " has not reported testfileFinished: run sent "
    + age(now - entry.runAt) + " ago, runner silent for " + age(now - entry.lastAt)
    + "; its last messages: " + (entry.recent.join(" -> ") || "none")
    + ". Files still running: " + pending.join("; ")
    + ". A long-running file can also be silent; if it printed its result, see #951 (Durable Object work left in flight as a file ends). Log: " + logPath + "\n";
  process.stderr.write(text);
  try {
    mkdirSync(LOG_DIR, { recursive: true });
    appendFileSync(logPath, new Date(now).toISOString() + " " + text);
  } catch (error) {
    process.stderr.write("[workers-pool-diagnostics] could not write " + logPath + ": " + String(error) + "\n");
  }
}

function positive(value: string | undefined): number | undefined {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
}

