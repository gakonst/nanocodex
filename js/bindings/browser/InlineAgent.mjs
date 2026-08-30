import { applyBrowserPatch, Nanocodex } from "../pkg-web/nanocodex.js";

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
import { createBrowserHost } from "./host.mjs";
import { initializeBrowserEngine } from "./engine.mjs";
import { resolveResponsesTransport } from "../runtime/responses-transport.mjs";
import {
  createManagedAgent,
  managedTransportOptions,
} from "../runtime/managed-transport.mjs";
import { resolveTools } from "../runtime/tool-configuration.mjs";
import {
  hostManaged as defaultHostManagedTransport,
} from "./Transport.mjs";
import { createProvider as createWebMcpProvider, isProvider as isWebMcpProvider } from "../webmcp/WebMcp.mjs";

/** Creates the Rust/WASM Agent in the current Web API host isolate. */
export async function create(options = {}) {
  if (managedTransportOptions(options?.transport)) return createManagedAgent(options);
  const internalRuntime = options[Symbol.for("nanocodex.browser.internalRuntime")];
  if (internalRuntime !== undefined
    && (!internalRuntime || typeof internalRuntime !== "object" || Array.isArray(internalRuntime))) {
    throw new TypeError("browser Agent internal runtime options must be an object");
  }
  const {
    transport,
    module,
    model,
    thinking,
    reasoningMode,
    fastMode,
    instructions,
    sessionId,
    workspace,
    resume,
    durability,
    durabilityId,
    terminalReceiptRetention,
    filesystem,
    filesystemTools,
    tools,
    toolMode,
    mcp,
    executionEnvironment,
    codeEvaluator,
    webMcp,
  } = options;
  const stableSessionId = sessionId ?? createSessionId();
  const {
    apiKey,
    hostAuth,
    hostManagedProtocol,
    subscription,
    mpp,
    websocketUrl,
    websocketPreconnect,
    apiBaseUrl,
    websocketWarmup,
    WebSocketImpl,
    createWebSocket,
  } = resolveResponsesTransport(transport ?? defaultHostManagedTransport());
  const { tools: hostTools, subagents: subagentConfig } = resolveTools(tools);
  if (filesystem && workspace !== undefined && workspace !== filesystem.root) {
    throw new TypeError("workspace must match filesystem.root when both are provided");
  }
  const events = createEventChannel();
  const tempoMcp = mpp?.[Symbol.for("nanocodex.tempo.mcp")];
  let hostDefinitionId;
  let webMcpProvider;
  if (webMcp !== undefined && webMcp !== false) {
    webMcpProvider = isWebMcpProvider(webMcp)
      ? webMcp
      : await createWebMcpProvider(webMcp === true ? {} : webMcp);
  }
  const toolProviders = [
    ...(internalRuntime?.toolProviders ?? []),
    ...(webMcpProvider ? [webMcpProvider] : []),
  ];
  let host;
  try {
    host = createBrowserHost({
      WebSocketImpl,
      createWebSocket,
      hostAuth: hostAuth === true
        || (apiKey === undefined && mpp === undefined && subscription === undefined),
      hostManagedProtocol,
      mpp,
      onEvent: events.emit,
      filesystem,
      filesystemTools,
      tools: hostTools,
      toolProviders,
      toolMode,
      mcp: mcp === false
        ? undefined
        : tempoMcp ? { ...tempoMcp, ...mcp } : mcp,
      codeEvaluator,
      applyPatch: applyBrowserPatch,
      websocketPreconnect,
      websocketUrl,
      onDispose: () => {
        releaseDefinitionHost(hostDefinitionId);
        webMcpProvider?.close();
      },
    });
  } catch (error) {
    webMcpProvider?.close();
    throw error;
  }
  let durabilityOwner;
  let creationStarted = false;
  hostDefinitionId = registerDefinitionHost(host);
  activateHost(host);
  const runtime = defineRuntime({
    key: "browser-wasm",
    name: "Nanocodex Browser WASM",
    type: "browser",
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
        await initializeBrowserEngine({ module });
        activateHost(host);
        const configJson = JSON.stringify(toWasmConfig({
          apiKey: apiKey ?? (mpp === undefined
            ? subscription === undefined ? "host-managed" : "subscription-managed"
            : "mpp-managed"),
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
  let agent;
  try {
    agent = await createAgentClient(runtime, {
      model,
      thinking,
      reasoningMode,
      fastMode,
      instructions,
      sessionId: stableSessionId,
      workspace: workspace ?? filesystem?.root,
      executionEnvironment,
      resume,
      durabilityId,
      terminalReceiptRetention,
    });
  } catch (error) {
    if (!creationStarted) await host.dispose();
    throw error;
  }
  if (websocketPreconnect && websocketUrl) {
    // Preconnect is speculative. A normal turn reconnects through the owned
    // transport path, while adapters that require startup validation (such as
    // Cloudflare) observe the same attempt at their createWebSocket boundary.
    void host.preconnect(websocketUrl, agent.sessionId).catch(() => {});
  }
  return agent;
}

function releaseHost(host) {
  void host.release().catch(reportError);
}
