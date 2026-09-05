import assert from "node:assert/strict";
import { test } from "node:test";

import * as BrowserTransport from "../browser/Transport.mjs";
import * as NodeTransport from "../node/Transport.mjs";
import * as Subagents from "../runtime/subagents.mjs";
import { resolveTools } from "../runtime/tool-configuration.mjs";

test("Responses transports own authentication and connection setup", () => {
  const openAi = NodeTransport.openAi({
    apiKey: "sk-test",
    websocketUrl: "wss://responses.test",
    websocketWarmup: true,
  });
  assert.equal(Object.isFrozen(openAi), true);
  assert.deepEqual(Reflect.ownKeys(openAi), []);
  assert.equal("resolve" in NodeTransport, false);

  const createWebSocket = () => ({ socket: {} });
  const hostManaged = BrowserTransport.hostManaged({ createWebSocket });
  assert.equal(Object.isFrozen(hostManaged), true);
  assert.deepEqual(Reflect.ownKeys(hostManaged), []);
  assert.equal("resolve" in BrowserTransport, false);
  assert.throws(() => NodeTransport.openAi({ apiKey: " " }), /non-empty/);
});

test("subagents are installed by default and expose branded lifecycle helpers", () => {
  assert.deepEqual(Object.keys(Subagents), [
    "close", "create", "interrupt", "list", "send", "spawn", "spawnMany", "wait",
  ]);
  assert.deepEqual(resolveTools(undefined), {
    tools: {},
    subagents: { max_concurrency: 32 },
  });
  const ping = {
    name: "ping",
    description: "Return pong.",
    handler: () => "pong",
  };
  assert.deepEqual(resolveTools([ping]), {
    tools: {
      ping: { description: "Return pong.", handler: ping.handler },
    },
    subagents: { max_concurrency: 32 },
  });
  const subagents = Subagents.create({ maxConcurrency: 7 });
  const handler = () => "pong";
  assert.deepEqual(resolveTools([{
    name: "ping",
    description: "Return pong.",
    handler,
  }, ...subagents]), {
    tools: {
      ping: { description: "Return pong.", handler },
    },
    subagents: { max_concurrency: 7 },
  });
  assert.equal(Object.isFrozen(subagents), true);
  assert.equal(Object.isFrozen(subagents[0]), true);
  assert.throws(() => resolveTools([{ maxConcurrency: 7 }]), /named tools/);
  assert.throws(
    () => resolveTools([...subagents, ...subagents]),
    /only be included once/,
  );
  assert.deepEqual(resolveTools([...subagents]), {
    tools: {},
    subagents: { max_concurrency: 7 },
  });
  assert.throws(() => Subagents.create({ maxConcurrency: 0 }), /positive safe integer/);
});
