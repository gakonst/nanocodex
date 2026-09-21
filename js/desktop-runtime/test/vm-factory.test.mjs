import assert from "node:assert/strict";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";
import { desktopFactoryRecipe, superviseVmFactory } from "../src/vm-factory.mjs";

test("desktop factories require a separate desktop image and keep stable, account-scoped recipes", () => {
  const defaults = { binary: "/app/helper", rootfs: "/app/shell.ext4", guestRuntime: "/app/guest" };
  const host = { id: "local-mac", name: "My Mac" };
  assert.equal(desktopFactoryRecipe(defaults, host, "/account-a"), undefined);
  const recipe = desktopFactoryRecipe({ ...defaults, desktopRootfs: "/app/desktop.ext4" }, host, "/account-a");
  assert.equal(recipe.factoryName, desktopFactoryRecipe({ ...defaults, desktopRootfs: "/app/desktop.ext4" }, host, "/account-b").factoryName);
  assert(recipe.args.includes("/app/desktop.ext4"));
  assert(!recipe.args.includes(defaults.rootfs));
  assert(recipe.args.includes(`/account-a/vm-factories/${recipe.factoryName}`));
  for (const override of [{ binary: "relative" }, { factoryName: "cf_sandbox" }, { factoryName: "bad name" }]) {
    assert.throws(() => desktopFactoryRecipe({ ...defaults, desktopRootfs: "/app/desktop.ext4", ...override }, host, "/account-a"));
  }
});

function supervisor(t, source, options = {}) {
  const states = [], children = [];
  const controller = new AbortController();
  const factory = superviseVmFactory({
    binary: process.execPath, args: ["-e", source], env: { PATH: process.env.PATH },
    signal: controller.signal, onState: state => states.push(state),
    readyTimeoutMs: 3_000, stopTimeoutMs: 100, killTimeoutMs: 100,
    retryDelays: [], sanitize: () => "Sanitized failure",
    spawnProcess: (...args) => { const child = spawn(...args); children.push(child); return child; },
    ...options,
  });
  t.after(() => factory.close());
  return { factory, states, children, controller };
}

test("factory readiness follows the registration stage and shutdown reaps its child", { timeout: 5_000 }, async t => {
  const { factory, states, children } = supervisor(t, `
    console.log(JSON.stringify({fields:{message:'VM host is ready for managed allocations'}}));
    setTimeout(() => console.log(JSON.stringify({fields:{stage:'vm.host.ready'}})), 100);
    setInterval(() => {}, 1000);
  `);
  let ready = false;
  void factory.ready.then(() => { ready = true; });
  await delay(30);
  assert.equal(ready, false, "local preflight does not mean the remote registration is ready");
  await factory.ready;
  assert.equal(states.at(-1).status, "connected");
  await factory.close();
  assert(children.every(child => child.exitCode !== null || child.signalCode !== null));
});

test("reconnection outlives the initial readiness deadline without killing the factory", { timeout: 5_000 }, async t => {
  const { factory, states, children } = supervisor(t, `
    console.log(JSON.stringify({fields:{stage:'vm.host.ready'}}));
    setTimeout(() => console.log(JSON.stringify({fields:{stage:'vm.host.reconnecting'}})), 30);
    setTimeout(() => console.log(JSON.stringify({fields:{stage:'vm.host.ready'}})), 1500);
    setInterval(() => {}, 1000);
  `, { readyTimeoutMs: 1_000 });
  await factory.ready;
  await delay(1200);
  assert.equal(states.at(-1).status, "reconnecting");
  assert.equal(children.length, 1);
  assert.equal(children[0].exitCode, null);
  assert.equal(children[0].signalCode, null);
  await delay(400);
  assert.equal(states.at(-1).status, "connected");
  await factory.close();
  assert(children[0].signalCode);
});

test("initial registration still stops a factory that never becomes ready", { timeout: 5_000 }, async t => {
  const { factory, states, children } = supervisor(t, `
    console.log(JSON.stringify({fields:{stage:'vm.host.reconnecting'}}));
    setInterval(() => {}, 1000);
  `, { readyTimeoutMs: 100 });
  await factory.done;
  assert.equal(states.at(-1).status, "error");
  assert.match(states.at(-1).error, /readiness deadline/);
  assert(children[0].signalCode);
});

test("failed children retry within a bounded budget and errors are sanitized", { timeout: 5_000 }, async t => {
  const { factory, states, children } = supervisor(t, `
    console.error(JSON.stringify({level:'ERROR',fields:{error:'secret-value'}}));
    process.exit(2);
  `, { retryDelays: [5, 5] });
  await factory.done;
  assert.equal(children.length, 3);
  assert.equal(states.at(-1).error, "Sanitized failure");
  assert(!JSON.stringify(states).includes("secret-value"));
});

test("cancellation during startup reaps the process without emitting late state", { timeout: 5_000 }, async t => {
  const { factory, states, children, controller } = supervisor(t, "setInterval(() => {},1000)");
  await once(children[0], "spawn");
  controller.abort();
  const count = states.length;
  await factory.done;
  assert.equal(states.length, count);
  assert(children[0].signalCode);
});

test("spawn failure settles readiness and reports a sanitized error", { timeout: 5_000 }, async t => {
  const { factory, states } = supervisor(t, "", { binary: "/missing/nanocodex-factory" });
  await factory.ready;
  await factory.done;
  assert.deepEqual(states.at(-1), { status: "error", error: "Sanitized failure" });
});
