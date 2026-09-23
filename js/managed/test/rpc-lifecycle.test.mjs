import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

// A small real workerd graph, independent of production credentials or bindings.
// A custom disposer lets us observe finalization without relying on V8 GC timing.
const disposalTracker = `
const disposed = new Set();
function observeDisposal(ctx, id) {
  // workerd schedules a remote disposer as an ordinary task in the exporting
  // context. Keep that context alive until the callback runs, otherwise ending
  // the RPC can cancel the callback itself. The timer bounds a broken fixture.
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  const timeout = setTimeout(release, 5000);
  ctx.waitUntil(pending);
  return () => {
    disposed.add(id);
    clearTimeout(timeout);
    release();
  };
}
`;

const source = `
import { WorkerEntrypoint, RpcTarget } from "cloudflare:workers";
${disposalTracker}
class Capability extends RpcTarget {
  constructor(onDispose) { super(); this.onDispose = onDispose; }
  [Symbol.dispose]() { this.onDispose(); }
}
export default class Source extends WorkerEntrypoint {
  plain() { return { catalog: { connectors: {}, mcp_connections: [] } }; }
  read(id) { return { ...this.plain(), [Symbol.dispose]: observeDisposal(this.ctx, id) }; }
  capability(id) { return { ...this.plain(), target: new Capability(observeDisposal(this.ctx, id)) }; }
  disposed(id) { return disposed.has(id); }
}`;

const forwarder = `
import { WorkerEntrypoint } from "cloudflare:workers";
import { consumeRpcData } from "nanocodex/cloudflare/rpc";
${disposalTracker}
export default class Forwarder extends WorkerEntrypoint {
  forward(id) {
    return this.env.SOURCE.read(id);
  }
  async detach(id) {
    return consumeRpcData(await this.env.SOURCE.read(id));
  }
  async generatedDisposer(id) {
    const onDispose = observeDisposal(this.ctx, id);
    const result = await this.env.SOURCE.plain();
    const original = result[Symbol.dispose];
    // Reproduce Egress returning a generated first-hop disposer with its data.
    Object.defineProperty(result, Symbol.dispose, { value() { original.call(result); onDispose(); } });
    return result;
  }
  disposed(id) { return disposed.has(id); }
}`;

const client = `
import { consumeRpcData } from "nanocodex/cloudflare/rpc";
async function finalized(service, id) {
  for (let i = 0; i < 100; i++) {
    if (await service.disposed(id)) return true;
    await scheduler.wait(1);
  }
  return false;
}
export default {
  async fetch(request, env) {
    const generated = await env.SOURCE.plain();
    const hasGeneratedDisposer = typeof generated[Symbol.dispose] === "function";
    const cached = consumeRpcData(generated).catalog;

    const direct = consumeRpcData(await env.SOURCE.read("direct"));
    const directFinalized = await finalized(env.SOURCE, "direct");

    const forwarded = await env.FORWARDER.forward("forwarded");
    const forwardedRetainsOwner = !await env.SOURCE.disposed("forwarded");
    const forwardedData = consumeRpcData(forwarded);
    const forwardedFinalized = await finalized(env.SOURCE, "forwarded");

    const generatedForwarded = await env.FORWARDER.generatedDisposer("generated");
    const generatedRetainsOwner = !await env.FORWARDER.disposed("generated");
    consumeRpcData(generatedForwarded);
    const generatedFinalized = await finalized(env.FORWARDER, "generated");

    const detached = await env.FORWARDER.detach("detached");
    // The inner owner is finalized before this outer result is disposed.
    const detachedFinalizedBeforeOuterDisposal = await finalized(env.SOURCE, "detached");
    const detachedData = consumeRpcData(detached);

    const withCapability = await env.SOURCE.capability("capability");
    let capabilityRejected = false;
    try { consumeRpcData(withCapability); }
    catch (error) { capabilityRejected = error.name === "DataCloneError"; }
    const capabilityFinalized = await finalized(env.SOURCE, "capability");

    // Simulate retaining discovery after unrelated RPC I/O and all owner disposal.
    const cacheSurvives = JSON.stringify(cached) === JSON.stringify(detachedData.catalog)
      && JSON.stringify(cached) === JSON.stringify(forwardedData.catalog)
      && JSON.stringify(cached) === JSON.stringify(direct.catalog)
      && Object.getOwnPropertySymbols(cached).length === 0
      && Object.getOwnPropertySymbols(detachedData).length === 0;
    return Response.json({ hasGeneratedDisposer, directFinalized, forwardedRetainsOwner, forwardedFinalized,
      generatedRetainsOwner, generatedFinalized, detachedFinalizedBeforeOuterDisposal,
      capabilityRejected, capabilityFinalized, cacheSurvives });
  }
}`;

async function bundle(contents) {
  const result = await build({ stdin: { contents, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser", external: ["cloudflare:workers"] });
  return result.outputFiles[0].text;
}

test("real workerd finalizes every data owner, separates forwarding ownership and rejects cached stubs", { timeout: 20_000 }, async () => {
  const mf = new Miniflare({ workers: [
    { name: "client", modules: true, compatibilityDate: "2026-07-29", script: await bundle(client),
      serviceBindings: { SOURCE: "source", FORWARDER: "forwarder" } },
    { name: "forwarder", modules: true, compatibilityDate: "2026-07-29", script: await bundle(forwarder),
      serviceBindings: { SOURCE: "source" } },
    { name: "source", modules: true, compatibilityDate: "2026-07-29", script: source },
  ] });
  try {
    const response = await mf.dispatchFetch("https://rpc-lifecycle.test");
    assert.equal(response.status, 200, await response.clone().text());
    const evidence = await response.json();
    console.log("RPC_LIFECYCLE_EVIDENCE", JSON.stringify(evidence));
    assert.deepEqual(evidence, { hasGeneratedDisposer: true, directFinalized: true, forwardedRetainsOwner: true,
      forwardedFinalized: true, generatedRetainsOwner: true, generatedFinalized: true,
      detachedFinalizedBeforeOuterDisposal: true, capabilityRejected: true,
      capabilityFinalized: true, cacheSurvives: true });
  } finally { await mf.dispose(); }
});
