import { env } from "cloudflare:test";
import { afterAll } from "vitest";

// The Workers pool delivers every Durable Object event (fetch, RPC, alarm,
// webSocketClose) through a wrapper that first re-imports the Worker via the
// runner object. If such an event is still importing when the file's tests
// end, the vitest worker never reports the file finished: the pool slot is
// held forever and the run stalls until the CI job timeout. Background work a
// test leaves behind (a closed socket, waitUntil warmups, registry publishing)
// therefore has to settle while the file is still running.
const QUIET_MS = 50;
const DEADLINE_MS = 10_000;

type RunnerNamespace = { get(id: unknown): Record<PropertyKey, unknown> };
const runner = (env as unknown as { __VITEST_POOL_WORKERS_RUNNER_OBJECT?: RunnerNamespace }).__VITEST_POOL_WORKERS_RUNNER_OBJECT;
let inFlight = 0;
let lastActivity = Date.now();

if (runner) {
  const get = runner.get.bind(runner);
  runner.get = (id: unknown) => new Proxy(get(id), {
    get(target, key) {
      const value = Reflect.get(target, key);
      if (typeof value !== "function") return value;
      if (key !== "fetch") return value.bind(target);
      return (...args: unknown[]) => {
        inFlight++;
        lastActivity = Date.now();
        const settle = () => { inFlight--; lastActivity = Date.now(); };
        const pending = Promise.resolve(value.apply(target, args));
        pending.then(settle, settle);
        return pending;
      };
    },
  });
}

afterAll(async () => {
  const deadline = Date.now() + DEADLINE_MS;
  while (inFlight > 0 || Date.now() - lastActivity < QUIET_MS) {
    if (Date.now() > deadline) {
      throw new Error(`${inFlight} Durable Object event(s) were still running ${DEADLINE_MS}ms after this file's tests finished; await the work the test started`);
    }
    await scheduler.wait(10);
  }
});
