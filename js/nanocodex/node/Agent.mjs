import { createRequire } from "node:module";
import initWeb, { Nanocodex as WebNanocodex } from "../pkg-web/nanocodex.js";

import { agentActions } from "../actions/index.mjs";
import {
  activateHost,
  bindHostSession,
  createAgentClient,
  createEventChannel,
  createSessionId,
  defineRuntime,
  loadDurabilityRuntime,
  loadSubscriptionRuntime,
  reportError,
  registerDefinitionHost,
  releaseDefinitionHost,
  releaseHostSession,
  toWasmConfig,
} from "../internal.mjs";
import { createNodeHost } from "./host.mjs";
import { resolveResponsesTransport } from "../runtime/responses-transport.mjs";
import {
  createManagedAgent,
  managedTransportOptions,
} from "../runtime/managed-transport.mjs";
import { resolveTools } from "../runtime/tool-configuration.mjs";

let initializedWeb;
let NodeNanocodex;

export function create(options = {}) {
  if (managedTransportOptions(options?.transport)) return createManagedAgent(options);
  const {
    model,
    thinking,
    reasoningMode,
    fastMode,
    instructions,
    additionalInstructions,
    sessionId,
    workspace,
    resume,
    durability,
    durabilityId,
    transport,
    module,
    filesystem,
    tools,
    toolMode,
    mcp,
    codeEvaluator,
  } = options;
  const stableSessionId = sessionId ?? createSessionId();
  const {
    apiKey,
    subscription,
    mpp,
    websocketUrl,
    apiBaseUrl,
    websocketWarmup,
  } = resolveResponsesTransport(transport);
  const { tools: hostTools, subagents: subagentConfig } = resolveTools(tools);
  const events = createEventChannel();
  if (filesystem && workspace !== undefined && workspace !== filesystem.root) {
    throw new TypeError("workspace must match filesystem.root when both are provided");
  }
  const tempoMcp = mpp?.[Symbol.for("nanocodex.tempo.mcp")];
  let hostDefinitionId;
  const host = createNodeHost({
    mpp,
    mcpServers: mcp === false
      ? undefined
      : tempoMcp ? { ...tempoMcp, ...mcp } : mcp,
    onEvent: events.emit,
    filesystem,
    tools: hostTools,
    toolMode,
    workspace: workspace ?? filesystem?.root ?? resume?.workspace,
    codeEvaluator,
    onDispose: () => releaseDefinitionHost(hostDefinitionId),
  });
  let durabilityOwner;
  let creationStarted = false;
  hostDefinitionId = registerDefinitionHost(host);
  activateHost(host);
  const runtime = defineRuntime({
    key: "node-wasm",
    name: "Nanocodex Node WASM",
    type: "node",
    async create(config) {
      creationStarted = true;
      try {
        if (durability !== undefined || durabilityId !== undefined) {
          durabilityOwner = (await loadDurabilityRuntime()).own(
            host,
            durability,
            durabilityId,
          );
        }
        activateHost(host);
        await host.ready();
        const Nanocodex = module === undefined
          ? loadNodeNanocodex()
          : await loadWebNanocodex(module);
        activateHost(host);
        const configJson = JSON.stringify(toWasmConfig({
          apiKey: apiKey ?? (subscription === undefined
            ? mpp === undefined ? undefined : "mpp-managed"
            : "subscription-managed"),
          websocketUrl: websocketUrl ?? (mpp === undefined
            ? undefined
            : "wss://openai.mpp.tempo.xyz/v1/responses"),
          apiBaseUrl,
          websocketWarmup,
          subagents: subagentConfig,
          hostDefinitionId,
          ...config,
          durabilityHostId: durabilityOwner?.id,
        }));
        return subscription === undefined
          ? Nanocodex.create(configJson)
          : Nanocodex.createWithChatGpt(
              configJson,
              (await loadSubscriptionRuntime()).rawSubscription(subscription),
            );
      } catch (error) {
        durabilityOwner?.abandon();
        await host.dispose();
        throw error;
      }
    },
    subscribe: events.subscribe,
    adopt(raw) {
      host.retain();
      try {
        durabilityOwner?.retain();
        bindHostSession(host, raw.sessionId);
        events.addSource(raw);
      } catch (error) {
        events.removeSource(raw);
        durabilityOwner?.release();
        releaseHost(host);
        throw error;
      }
    },
    release(raw) {
      events.removeSource(raw);
      host.releaseSession(raw.sessionId);
      releaseHostSession(host, raw.sessionId);
      durabilityOwner?.release();
      releaseHost(host);
    },
    decorate: (agent) => agent.extend(agentActions()),
  });
  return createAgentClient(runtime, {
    model,
    thinking,
    reasoningMode,
    fastMode,
    instructions,
    additionalInstructions,
    sessionId: stableSessionId,
    workspace: workspace ?? filesystem?.root,
    resume,
    durabilityId,
  }).catch(async (error) => {
    if (!creationStarted) await host.dispose();
    throw error;
  });
}

function releaseHost(host) {
  void host.release().catch(reportError);
}

function loadNodeNanocodex() {
  const require = createRequire(import.meta.url);
  NodeNanocodex ||= require("../pkg-node/nanocodex.js").Nanocodex;
  return NodeNanocodex;
}

async function loadWebNanocodex(module) {
  initializedWeb ||= initWeb({ module_or_path: module });
  await initializedWeb;
  return WebNanocodex;
}
