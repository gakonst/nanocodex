import { applyBrowserPatch, Nanocodex } from "../pkg-web/nanocodex.js";

import { agentActions } from "../actions/index.mjs";
import {
  activateHost,
  activateCloudflareAgentSession,
  bindHostSession,
  CLOUDFLARE_SESSION_RESERVATION,
  createAgentClient,
  createEventChannel,
  createSessionId,
  defineRuntime,
  loadDurabilityRuntime,
  loadSubscriptionRuntime,
  mayReleaseCloudflareSubagentSession,
  reportError,
  registerDefinitionHost,
  releaseDefinitionHost,
  releaseHostSession,
  releaseHostSessions,
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
    additionalInstructions,
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
  } = options;
  const toolProviders = internalRuntime?.toolProviders;
  const subagentSessions = internalRuntime?.subagentSessions;
  const cloudflareReservation = internalRuntime?.[CLOUDFLARE_SESSION_RESERVATION];
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
    stateless,
    WebSocketImpl,
    createWebSocket,
    createResponse,
  } = resolveResponsesTransport(transport ?? defaultHostManagedTransport());
  const subagentsEnabled = internalRuntime?.subagentsEnabled;
  if (subagentsEnabled !== undefined && typeof subagentsEnabled !== "boolean") {
    throw new TypeError("host subagentsEnabled must be a boolean");
  }
  const { tools: hostTools, subagents: resolvedSubagents } = resolveTools(tools, {
    defaultSubagents: subagentsEnabled !== false,
  });
  // A host prohibition also overrides an explicit Subagents.create() tool entry.
  const configuredSubagents = subagentsEnabled === false ? undefined : resolvedSubagents;
  const subagentMaxConcurrency = internalRuntime?.subagentMaxConcurrency;
  if (subagentMaxConcurrency !== undefined
    && (!Number.isSafeInteger(subagentMaxConcurrency) || subagentMaxConcurrency < 1)) {
    throw new TypeError("host subagentMaxConcurrency must be a positive safe integer");
  }
  // Hosted runtimes own the resource ceiling, including when their tools are
  // a prepared router rather than a named-tool array. A caller's lower cap wins.
  const subagentConfig = configuredSubagents === undefined || subagentMaxConcurrency === undefined
    ? configuredSubagents
    : {
      ...configuredSubagents,
      max_concurrency: Math.min(configuredSubagents.max_concurrency ?? subagentMaxConcurrency, subagentMaxConcurrency),
    };
  if (filesystem && workspace !== undefined && workspace !== filesystem.root) {
    throw new TypeError("workspace must match filesystem.root when both are provided");
  }
  const events = createEventChannel();
  const tempoMcp = mpp?.[Symbol.for("nanocodex.tempo.mcp")];
  let hostDefinitionId;
  const host = createBrowserHost({
    WebSocketImpl,
    createWebSocket,
    createResponse,
    hostAuth: hostAuth === true
      || (apiKey === undefined && mpp === undefined && subscription === undefined),
    hostManagedProtocol,
    mpp,
    onEvent: events.emit,
    filesystem,
    filesystemTools,
    tools: hostTools,
    toolProviders,
    subagentSessions,
    subagentRouting: internalRuntime?.subagentRouting,
    toolMode,
    mcp: mcp === false
      ? undefined
      : tempoMcp ? { ...tempoMcp, ...mcp } : mcp,
    codeEvaluator,
    applyPatch: applyBrowserPatch,
    websocketPreconnect,
    websocketUrl,
    onDispose: () => releaseDefinitionHost(hostDefinitionId),
  });
  let durabilityOwner;
  let creationStarted = false;
  hostDefinitionId = registerDefinitionHost(host, cloudflareReservation);
  activateHost(host);
  const runtime = defineRuntime({
    key: "browser-wasm",
    name: "Nanocodex Browser WASM",
    type: "browser",
    async create(config) {
      creationStarted = true;
      let raw;
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
          stateless,
          subagents: subagentConfig,
          subagentRouting: internalRuntime?.subagentRouting !== undefined,
          hostDefinitionId,
          ...config,
          durabilityHostId: durabilityOwner?.id,
        }));
        raw = subscription === undefined
          ? await Nanocodex.create(configJson)
          : await Nanocodex.createWithChatGpt(
              configJson,
              (await loadSubscriptionRuntime()).rawSubscription(subscription),
            );
        if (cloudflareReservation !== undefined) {
          activateCloudflareAgentSession(cloudflareReservation);
          const restoredSubagents = subagentSessions?.restore?.() ?? [];
          const checkpoint = subagentSessions?.restoreCheckpoint?.();
          let reusableCheckpoint = checkpoint;
          if (checkpoint !== undefined) {
            const complete = validateRestoredChildBindings(checkpoint, raw.sessionId, restoredSubagents, subagentSessions);
            raw.validateSubagentCheckpoint(checkpoint);
            if (!complete) {
              // Older owners retained snapshots while admitting new children.
              // Keep their durable identities, but never replay an obsolete tree.
              reusableCheckpoint = undefined;
            }
          }
          if (restoredSubagents.length > 0 || checkpoint !== undefined) {
            const restoredHostContextRefs = Object.fromEntries(
              restoredSubagents.map((descriptor) => {
                const hostContextRef = subagentSessions?.hostContextRef?.(descriptor.sessionId);
                return [descriptor.sessionId, hostContextRef ?? null];
              }),
            );
            await raw.restoreSubagents(
              reusableCheckpoint ?? JSON.stringify(restoredSubagents),
              JSON.stringify(restoredHostContextRefs),
            );
          }
        }
        return raw;
      } catch (error) {
        const cleanupErrors = [];
        if (raw !== undefined) {
          try {
            if (cloudflareReservation !== undefined) await raw.shutdownDurable();
            else await raw.shutdown();
          }
          catch (cleanupError) { cleanupErrors.push(cleanupError); }
          try { raw.free(); }
          catch (cleanupError) { cleanupErrors.push(cleanupError); }
        }
        releaseHostSessions(host);
        try { durabilityOwner?.abandon(); }
        catch (cleanupError) { cleanupErrors.push(cleanupError); }
        try { await host.dispose(); }
        catch (cleanupError) { cleanupErrors.push(cleanupError); }
        if (cleanupErrors.length > 0) {
          throw new AggregateError(
            [error, ...cleanupErrors],
            "browser Agent creation and cleanup both failed",
          );
        }
        throw error;
      }
    },
    async shutdown(raw) {
      if (cloudflareReservation === undefined || raw.sessionId !== cloudflareReservation.sessionId
        || subagentSessions?.checkpoint === undefined) {
        await raw.shutdown();
        return;
      }
      const errors = [];
      try {
        if (mayReleaseCloudflareSubagentSession(cloudflareReservation)) {
          const checkpoint = await raw.checkpointSubagents();
          if (mayReleaseCloudflareSubagentSession(cloudflareReservation)) {
            subagentSessions.checkpoint(checkpoint);
          }
        }
      } catch (error) {
        errors.push(error);
      }
      try { await raw.shutdownDurable(); }
      catch (error) { errors.push(error); }
      // Durable unload keeps descriptors, but relinquishes this host's in-memory
      // registrations. The released Cloudflare generation has no host authority.
      releaseHostSessions(host);
      if (errors.length === 1) throw errors[0];
      if (errors.length > 1) throw new AggregateError(errors, "Subagent checkpoint and shutdown failed");
    },
    subscribe: events.subscribe,
    adopt(raw) {
      host.retain();
      try {
        durabilityOwner?.retain();
        bindHostSession(host, raw.sessionId, cloudflareReservation);
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
      additionalInstructions,
      sessionId: stableSessionId,
      workspace: workspace ?? filesystem?.root,
      executionEnvironment,
      resume,
      durabilityId,
      terminalReceiptRetention,
    }, cloudflareReservation);
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

function validateRestoredChildBindings(encoded, rootSessionId, bindings, sessions) {
  const checkpoint = JSON.parse(encoded);
  if (checkpoint.version !== 1 || checkpoint.root_session_id !== rootSessionId
    || !Array.isArray(checkpoint.children) || checkpoint.children.length > bindings.length) {
    throw new Error("Durable child checkpoint does not match its session bindings");
  }
  const retained = new Map(bindings.map((binding) => [binding.sessionId, binding]));
  for (const child of checkpoint.children) {
    const descriptor = child?.descriptor;
    const binding = retained.get(descriptor?.session_id);
    if (binding === undefined || String(descriptor.id) !== binding.agentId || descriptor.role !== binding.role
      || (descriptor.parent == null ? null : String(descriptor.parent)) !== (binding.parentAgentId ?? null)) {
      throw new Error("Durable child checkpoint identity differs from its session binding");
    }
    if ((child.host_context ?? null) !== (sessions.hostContextRef?.(binding.sessionId) ?? null)) {
      throw new Error("child checkpoint host context differs from its retained binding");
    }
    retained.delete(descriptor.session_id);
  }
  return retained.size === 0;
}
