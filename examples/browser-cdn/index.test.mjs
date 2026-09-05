import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const html = await readFile(new URL("./index.html", import.meta.url), "utf8");
const moduleSource = html.match(
  /<script type="module">([\s\S]*?)<\/script>/,
)?.[1];
assert(moduleSource, "the browser example module is missing");

test("concurrent submits share retryable creation and pagehide owns cleanup", async () => {
  const firstCreation = deferred();
  const firstAgent = fakeAgent();
  let createCalls = 0;
  const concurrent = await loadExample({
    create() {
      createCalls += 1;
      return firstCreation.promise;
    },
  });

  const first = concurrent.submit("first");
  const second = concurrent.submit("second");
  await tick();
  assert.equal(createCalls, 1);
  firstCreation.resolve(firstAgent.agent);
  await Promise.all([first, second]);

  assert.deepEqual(firstAgent.prompts, ["first", "second"]);
  assert.deepEqual(firstAgent.resultDisposals, [1, 1]);
  assert.deepEqual(firstAgent.turnDisposals, [1, 1]);
  concurrent.pagehide();
  await tick();
  assert.equal(firstAgent.shutdowns, 1);
  assert.equal(firstAgent.disposals, 0);

  const retryAgent = fakeAgent();
  let attempts = 0;
  const retryable = await loadExample({
    create() {
      attempts += 1;
      return attempts === 1
        ? Promise.reject(new Error("startup failed"))
        : Promise.resolve(retryAgent.agent);
    },
  });
  await retryable.submit("fails");
  assert.equal(retryable.result.textContent, "startup failed");
  await retryable.submit("retries");
  assert.equal(attempts, 2);
  assert.deepEqual(retryAgent.prompts, ["retries"]);
  retryable.pagehide();
  await tick();
  assert.equal(retryAgent.shutdowns, 1);

  const inFlightCreation = deferred();
  const inFlightAgent = fakeAgent();
  const inFlight = await loadExample({
    create: () => inFlightCreation.promise,
  });
  const pendingSubmit = inFlight.submit("must not run");
  await tick();
  inFlight.pagehide();
  inFlightCreation.resolve(inFlightAgent.agent);
  await pendingSubmit;
  await tick();

  assert.deepEqual(inFlightAgent.prompts, []);
  assert.equal(inFlightAgent.shutdowns, 1);
  assert.equal(inFlightAgent.disposals, 0);
  assert.match(inFlight.result.textContent, /page closed/i);

  const shutdownError = new Error("shutdown failed");
  const shutdownFailureAgent = fakeAgent({ shutdownError });
  const reported = [];
  const reportErrorDescriptor = Object.getOwnPropertyDescriptor(
    globalThis,
    "reportError",
  );
  const originalConsoleError = console.error;
  try {
    Object.defineProperty(globalThis, "reportError", {
      configurable: true,
      value: undefined,
    });
    console.error = (...args) => reported.push(args);

    const shutdownFailure = await loadExample({
      create: () => Promise.resolve(shutdownFailureAgent.agent),
    });
    await shutdownFailure.submit("cleanup fallback");
    shutdownFailure.pagehide();
    await tick();

    assert.equal(shutdownFailureAgent.shutdowns, 1);
    assert.equal(shutdownFailureAgent.disposals, 1);
    assert.deepEqual(reported, [
      ["Nanocodex agent shutdown failed:", shutdownError],
    ]);
  } finally {
    console.error = originalConsoleError;
    if (reportErrorDescriptor) {
      Object.defineProperty(globalThis, "reportError", reportErrorDescriptor);
    } else {
      delete globalThis.reportError;
    }
  }
});

let moduleId = 0;

async function loadExample(Agent) {
  const formListeners = new Map();
  const pageListeners = new Map();
  const form = {
    addEventListener(type, listener) {
      formListeners.set(type, listener);
    },
  };
  const prompt = { value: "" };
  const result = { textContent: "" };

  globalThis.__nanocodexTestAgent = Agent;
  globalThis.document = {
    querySelector(selector) {
      if (selector === "#prompt-form") return form;
      if (selector === "#prompt") return prompt;
      if (selector === "#result") return result;
      throw new Error(`unexpected selector: ${selector}`);
    },
  };
  globalThis.location = {
    href: "https://example.test/",
    protocol: "https:",
    host: "example.test",
  };
  globalThis.addEventListener = (type, listener) => {
    pageListeners.set(type, listener);
  };

  const source = moduleSource
    .replace(
      /import \{ Agent, Transport \} from "[^"]+";/,
      "const Agent = globalThis.__nanocodexTestAgent; const Transport = { hostManaged: (options) => options };",
    )
    .concat(`\n// deterministic test module ${moduleId++}`);
  await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

  return {
    result,
    submit(value) {
      prompt.value = value;
      return formListeners.get("submit")({ preventDefault() {} });
    },
    pagehide() {
      pageListeners.get("pagehide")();
    },
  };
}

function fakeAgent({ shutdownError } = {}) {
  const prompts = [];
  const resultDisposals = [];
  const turnDisposals = [];
  let shutdowns = 0;
  let disposals = 0;
  const agent = {
    session: {
      async shutdown() {
        shutdowns += 1;
        if (shutdownError) throw shutdownError;
      },
    },
    turn: {
      prompt({ input }) {
        prompts.push(input);
        const index = turnDisposals.push(0) - 1;
        resultDisposals.push(0);
        return {
          async result() {
            return {
              finalMessage: input,
              dispose() {
                resultDisposals[index] += 1;
              },
            };
          },
          dispose() {
            turnDisposals[index] += 1;
          },
        };
      },
    },
    dispose() {
      disposals += 1;
    },
  };
  return {
    agent,
    prompts,
    resultDisposals,
    turnDisposals,
    get shutdowns() {
      return shutdowns;
    },
    get disposals() {
      return disposals;
    },
  };
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

function tick() {
  return new Promise((resolve) => setImmediate(resolve));
}
