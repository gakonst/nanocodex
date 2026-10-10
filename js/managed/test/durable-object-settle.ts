// The pool reads the runner binding from this same workerd built-in. Importing
// "cloudflare:test" here would load the Worker's module graph before a test
// file's vi.mock() factories are registered, defeating them.
import { env } from "cloudflare:workers";
import { afterAll } from "vitest";
// @ts-expect-error workerd:unsafe is a workerd built-in without published types.
import workerdUnsafe from "workerd:unsafe";

// Workaround for a teardown deadlock in @cloudflare/vitest-pool-workers 0.19.1
// (same code in 0.23.0). Its Durable Object wrapper (createDurableObjectWrapper
// / kEnsureInstance in dist/worker/lib/cloudflare/test-internal.mjs) re-imports
// the Worker for every Durable Object event (fetch, RPC, alarm,
// webSocketClose) through importModule -> runInRunnerObject, a fetch to the
// __VITEST_POOL_WORKERS_RUNNER_OBJECT binding. If such an event is still in
// flight when a file's last test returns, the vitest worker never posts
// "testfileFinished"; vitest frees pool slots only on that message, so the run
// stalls until the CI job timeout. Background work a test leaves behind (a
// closed socket, waitUntil warmups, registry publishing) therefore has to
// settle while the file is still running. Tracking: https://github.com/gakonst/nanocodex/issues/951
const QUIET_MS = 50;
const DEADLINE_MS = 10_000;
const RUNNER_BINDING = "__VITEST_POOL_WORKERS_RUNNER_OBJECT";

type RunnerNamespace = { get(id: unknown): Record<PropertyKey, unknown> };
const runner = (env as unknown as Record<string, RunnerNamespace | undefined>)[RUNNER_BINDING];
if (typeof runner?.get !== "function") {
  // Fail every file loudly rather than silently losing the settle barrier.
  throw new Error(`${RUNNER_BINDING} is missing; @cloudflare/vitest-pool-workers internals changed, so revisit test/durable-object-settle.ts`);
}
// Tests stub Date.now (vi.spyOn(Date, "now").mockReturnValue(...)); keep the
// real clock so a pinned or future-dated mock cannot freeze this barrier.
const now = Date.now.bind(Date);
let observed = 0;
let lastActivity = now();
let nextCall = 0;
// In-flight calls with the frames that issued them, for the deadline error.
const inFlight = new Map<number, string>();
const get = runner.get.bind(runner);
runner.get = (id: unknown) => new Proxy(get(id), {
  get(target, key) {
    const value = Reflect.get(target, key);
    if (typeof value !== "function") return value;
    if (key !== "fetch") return value.bind(target);
    return (...args: unknown[]) => {
      const call = ++nextCall;
      observed++;
      lastActivity = now();
      inFlight.set(call, callSite());
      const settle = () => { inFlight.delete(call); lastActivity = now(); };
      const pending = Promise.resolve(value.apply(target, args));
      pending.then(settle, settle);
      return pending;
    };
  },
});

// abortAllDurableObjects()/evict*() tear objects down mid-call: their pending
// I/O never settles and nothing awaits it any more, so forget those calls.
const unsafe = workerdUnsafe as unknown as Record<string, unknown>;
for (const name of ["abortAllDurableObjects", "evictAllDurableObjects", "deleteAllDurableObjects", "evict"]) {
  const original = unsafe[name];
  if (typeof original !== "function") continue;
  unsafe[name] = (...args: unknown[]) => {
    inFlight.clear();
    lastActivity = now();
    return original.apply(workerdUnsafe, args);
  };
}

function callSite(): string {
  const frames = (new Error().stack ?? "").split("\n").slice(1).map(frame => frame.trim())
    .filter(frame => !frame.includes("durable-object-settle"));
  return frames.slice(0, 6).map(frame => frame.replace(/\(.*node_modules\/(?:\.pnpm\/[^/]+\/node_modules\/)?/, "(")).join(" <- ");
}

afterAll(async () => {
  // Importing the test file itself goes through runInRunnerObject; seeing no
  // call means the pool stopped using the wrapped binding and this barrier
  // would be a silent no-op.
  if (observed === 0) {
    throw new Error(`no ${RUNNER_BINDING} calls were observed; @cloudflare/vitest-pool-workers internals changed, so revisit test/durable-object-settle.ts`);
  }
  const deadline = now() + DEADLINE_MS;
  const callsBefore = observed;
  while (inFlight.size > 0 || now() - lastActivity < QUIET_MS) {
    if (now() > deadline) {
      const pending = [...inFlight.values()].slice(0, 3).map(site => "  - " + site).join("\n");
      throw new Error(`Durable Object work did not settle within ${DEADLINE_MS}ms after this file's tests finished `
        + `(${inFlight.size} in flight, ${observed - callsBefore} new runner-object calls while waiting); `
        + `await the work the test started.\n${pending}`);
    }
    await scheduler.wait(10);
  }
  // Stay below the hook timeout so the diagnostic above is what gets reported.
}, DEADLINE_MS + 5_000);
