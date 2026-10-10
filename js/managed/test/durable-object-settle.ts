import { env } from "cloudflare:test";
import { afterAll } from "vitest";

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
// settle while the file is still running.
const QUIET_MS = 50;
const DEADLINE_MS = 10_000;
const RUNNER_BINDING = "__VITEST_POOL_WORKERS_RUNNER_OBJECT";

type RunnerNamespace = { get(id: unknown): Record<PropertyKey, unknown> };
const runner = (env as unknown as Record<string, RunnerNamespace | undefined>)[RUNNER_BINDING];
if (typeof runner?.get !== "function") {
  // Fail every file loudly rather than silently losing the settle barrier.
  throw new Error(`${RUNNER_BINDING} is missing; @cloudflare/vitest-pool-workers internals changed, so revisit test/durable-object-settle.ts`);
}
let inFlight = 0;
let observed = 0;
let lastActivity = Date.now();
const get = runner.get.bind(runner);
runner.get = (id: unknown) => new Proxy(get(id), {
  get(target, key) {
    const value = Reflect.get(target, key);
    if (typeof value !== "function") return value;
    if (key !== "fetch") return value.bind(target);
    return (...args: unknown[]) => {
      inFlight++;
      observed++;
      lastActivity = Date.now();
      const settle = () => { inFlight--; lastActivity = Date.now(); };
      const pending = Promise.resolve(value.apply(target, args));
      pending.then(settle, settle);
      return pending;
    };
  },
});

afterAll(async () => {
  // Importing the test file itself goes through runInRunnerObject; seeing no
  // call means the pool stopped using the wrapped binding and this barrier
  // would be a silent no-op.
  if (observed === 0) {
    throw new Error(`no ${RUNNER_BINDING} calls were observed; @cloudflare/vitest-pool-workers internals changed, so revisit test/durable-object-settle.ts`);
  }
  const deadline = Date.now() + DEADLINE_MS;
  while (inFlight > 0 || Date.now() - lastActivity < QUIET_MS) {
    if (Date.now() > deadline) {
      throw new Error(`${inFlight} Durable Object event(s) were still running ${DEADLINE_MS}ms after this file's tests finished; await the work the test started`);
    }
    await scheduler.wait(10);
  }
});
