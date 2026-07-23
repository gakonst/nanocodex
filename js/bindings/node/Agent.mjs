import { createRequire } from "node:module";

import { agentActions } from "../actions/index.mjs";
import {
  activateHost,
  bindHostSession,
  createAgentClient,
  createEventChannel,
  defineRuntime,
  releaseHostSession,
  toWasmConfig,
} from "../internal.mjs";
import { createNodeHost } from "./host.mjs";

const require = createRequire(import.meta.url);
const { Nanocodex } = require("../pkg-node/nanocodex.js");

export function create(options = {}) {
  const {
    thinking,
    reasoningMode,
    fastMode,
    instructions,
    sessionId,
    resume,
    apiKey,
    websocketUrl,
    apiBaseUrl,
    ...hostOptions
  } = options;
  const events = createEventChannel();
  const host = createNodeHost({ ...hostOptions, onEvent: events.emit });
  activateHost(host);
  const runtime = defineRuntime({
    key: "node-wasm",
    name: "Nanocodex Node WASM",
    type: "node",
    create(config) {
      activateHost(host);
      return new Nanocodex(JSON.stringify(toWasmConfig({
        apiKey,
        websocketUrl,
        apiBaseUrl,
        ...config,
      })));
    },
    subscribe: events.subscribe,
    adopt: (raw) => bindHostSession(host, raw.sessionId),
    release: (raw) => releaseHostSession(host, raw.sessionId),
    decorate: (agent) => agent.extend(agentActions()),
  });
  return createAgentClient(runtime, {
    thinking,
    reasoningMode,
    fastMode,
    instructions,
    sessionId,
    resume,
  });
}
