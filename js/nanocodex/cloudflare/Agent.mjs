import * as HostAgent from "../host/Agent.mjs";
import {
  CLOUDFLARE_SESSION_RESERVATION,
  commitCloudflareAgentSession,
  installHostBridge,
  loadDurabilityRuntime,
  mayBindCloudflareSubagentSession,
  mayReleaseCloudflareSubagentSession,
  observeAgentRelease,
  prepareCloudflareAgentSession,
  releaseAgentSession,
  routePrompt,
} from "../internal.mjs";
import { pruneDurableReceipts as pruneWasmDurableReceipts } from "../pkg-web/nanocodex.js";
import * as Transport from "../browser/Transport.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import { createCloudflareDurabilityStore } from "../runtime/cloudflare-durability-store.mjs";
import {
  createMemoryDurabilityStore,
  durabilityRevision,
  exportDurabilityState as exportPortableState,
  exportDurabilityStatePage as exportPortableStatePage,
  importDurabilityState as importPortableState,
} from "../runtime/durability-store.mjs";
import { cloudflareEgress } from "./egress.mjs";
import { scopeCloudflareEgress } from "./egress-subject.mjs";
import {
  clearCloudflareEventSocket,
  createCloudflareEventSocket,
} from "./event-socket.mjs";

const STARTUP_TIMEOUT_MS = 10_000;
const INTERNAL_RUNTIME = Symbol.for("nanocodex.cloudflare.internalRuntime");
const INTERNAL_CONFIGURATION = Symbol.for("nanocodex.cloudflare.internalConfiguration");
const EPHEMERAL_APPLICATION_OPTIONS = new Set([
  "additionalInstructions",
  "fastMode",
  "instructions",
  "model",
  "reasoningMode",
  "resume",
  "sessionId",
  "thinking",
  "tools",
  "workspace",
]);
const APPLICATION_OPTIONS = new Set([
  "additionalInstructions",
  "durabilityId",
  "eventPersistence",
  "instructions",
  "terminalReceiptRetention",
  "tools",
]);
const lifecycles = new WeakMap();

/** @internal Binds the package-owned module to the public Cloudflare namespace. */
export function bindAgent(module, hostAgent = HostAgent) {
  return Object.freeze({
    pruneDurableReceipts: (owner, options) => pruneDurableReceipts(module, owner, options),
    create: (owner, options) => create(module, owner, options, hostAgent),
    createEphemeral: (owner, options) => createEphemeral(module, owner, options),
    destroy,
    exportDurabilityState,
    importDurabilityState: (owner, archive) => importDurabilityState(owner, archive, module),
    route,
  });
}

/** Atomically steers an active Cloudflare Agent turn or starts a new turn. */
export function route(agent, options) {
  return routePrompt(agent, options);
}

/** Removes the package-owned durable history for one Cloudflare Agent. */
export function destroy(owner) {
  const context = resolveContext(owner);
  const lifecycle = lifecycles.get(context);
  if (lifecycle?.creating) {
    throw new Error("Cloudflare Agent creation must settle before destroy");
  }
  if (lifecycle?.active !== undefined) {
    throw new Error("Cloudflare Agent shutdown must complete before destroy");
  }
  const storage = context.storage;
  createCloudflareDurabilityStore(storage);
  initializeAgentStorage(storage);
  const stateId = storedStateId(storage) ?? legacyStateId(storage);
  storage.transactionSync(() => {
    if (stateId !== undefined) {
      const retained = storage.sql.exec(
        "SELECT fence FROM nanocodex_durable_owners WHERE state_id = ?",
        stateId,
      ).toArray();
      const fence = durabilityRevision(
        BigInt(durabilityRevision(retained[0]?.fence ?? "0")) + 1n,
      );
      storage.sql.exec(
        `INSERT INTO nanocodex_durable_owners (state_id, owner_id, fence) VALUES (?, ?, ?)
         ON CONFLICT (state_id) DO UPDATE SET owner_id = excluded.owner_id, fence = excluded.fence`,
        stateId,
        `destroy:${globalThis.crypto.randomUUID()}`,
        fence,
      );
      storage.sql.exec(
        "DELETE FROM nanocodex_durable_chunk_heads WHERE state_id = ?",
        stateId,
      );
      storage.sql.exec(
        "DELETE FROM nanocodex_durable_state_chunks WHERE state_id = ?",
        stateId,
      );
      storage.sql.exec(
        "DELETE FROM nanocodex_durable_states WHERE state_id = ?",
        stateId,
      );
    }
    storage.sql.exec("DELETE FROM nanocodex_cloudflare_subagents");
    clearCloudflareEventSocket(context);
  });
}

/** Fences and exports this inactive Cloudflare Agent's provider-neutral state. */
export async function exportDurabilityState(owner, request) {
  const context = reserveInactiveLifecycle(owner, "exporting durability state");
  try {
    const storage = context.storage;
    const durability = createCloudflareDurabilityStore(storage);
    initializeAgentStorage(storage);
    const stateId = storedStateId(storage) ?? legacyStateId(storage);
    if (stateId === undefined) {
      throw new Error("Cloudflare Agent has no durability state to export");
    }
    return request === undefined
      ? await exportPortableState(durability, stateId)
      : await exportPortableStatePage(durability, stateId, request);
  } finally {
    lifecycleFor(context).creating = false;
  }
}

/** Imports provider-neutral state into a pristine Cloudflare Agent owner. */
export async function importDurabilityState(owner, archive, module) {
  const context = reserveInactiveLifecycle(owner, "importing durability state");
  try {
    const storage = context.storage;
    const durability = createCloudflareDurabilityStore(storage);
    initializeAgentStorage(storage);
    const validationStateId = typeof archive?.stateId === "string" && archive.stateId
      ? archive.stateId
      : "nanocodex-invalid-import";
    const validationStore = createMemoryDurabilityStore(validationStateId);
    const validated = await importPortableState(validationStore, archive);
    if (module !== undefined) {
      const routeHost = {};
      const route = (await loadDurabilityRuntime()).own(
        routeHost,
        validationStore,
        validationStateId,
      );
      try {
        installHostBridge();
        await initializeBrowserEngine({ module });
        // Opening the Rust durability session validates the complete canonical
        // state. Pruning happens only in this throwaway memory copy.
        await pruneWasmDurableReceipts(route.id, validationStateId, 4_096);
      } finally {
        route.abandon();
      }
    }
    const retainedSessionId = storedSessionId(storage);
    const retainedStateId = storedStateId(storage);
    if (retainedSessionId !== undefined || retainedStateId !== undefined) {
      if (retainedSessionId !== undefined
        && retainedStateId === archive?.stateId
        && archive?.format === "nanocodex-durability-state-v1") {
        const retained = await durability.load(retainedStateId);
        if (retained.revision === validated.revision
          && retained.payload === validated.payload) {
          return retained;
        }
      }
      throw new Error("Cloudflare Agent durability import requires a pristine Durable Object");
    }
    const imported = await importPortableState(durability, archive);
    const sessionId = uuidV7();
    try {
      storage.transactionSync(() => {
        storage.sql.exec(
          "INSERT INTO nanocodex_cloudflare_agent (singleton, session_id) VALUES (1, ?)",
          sessionId,
        );
        storage.sql.exec(
          "INSERT INTO nanocodex_cloudflare_durability (singleton, state_id) VALUES (1, ?)",
          archive.stateId,
        );
      });
    } catch (error) {
      try {
        storage.transactionSync(() => {
          storage.sql.exec(
            "DELETE FROM nanocodex_durable_chunk_heads WHERE state_id = ?",
            archive.stateId,
          );
          storage.sql.exec(
            "DELETE FROM nanocodex_durable_state_chunks WHERE state_id = ?",
            archive.stateId,
          );
          storage.sql.exec(
            "DELETE FROM nanocodex_durable_states WHERE state_id = ?",
            archive.stateId,
          );
          storage.sql.exec(
            "DELETE FROM nanocodex_durable_owners WHERE state_id = ?",
            archive.stateId,
          );
        });
      } catch (rollbackError) {
        throw new AggregateError(
          [error, rollbackError],
          "Cloudflare Agent durability import metadata and rollback both failed",
        );
      }
      throw error;
    }
    return imported;
  } finally {
    lifecycleFor(context).creating = false;
  }
}

function reserveInactiveLifecycle(owner, operation) {
  const context = resolveContext(owner);
  const lifecycle = lifecycleFor(context);
  if (lifecycle.creating) {
    throw new Error("Cloudflare Agent lifecycle operation is already in progress");
  }
  if (lifecycle.active !== undefined) {
    throw new Error(`Cloudflare Agent shutdown must complete before ${operation}`);
  }
  lifecycle.creating = true;
  return context;
}

/** Prunes old terminal receipts before constructing the full Agent runtime. */
export async function pruneDurableReceipts(module, owner, options = {}) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Cloudflare durability receipt-pruning options must be an object");
  }
  const terminalReceiptRetention = options.terminalReceiptRetention ?? 512;
  if (!Number.isSafeInteger(terminalReceiptRetention)
    || terminalReceiptRetention < 0
    || terminalReceiptRetention > 4_096) {
    throw new TypeError("terminalReceiptRetention must be an integer from 0 through 4096");
  }
  const context = resolveContext(owner);
  const lifecycle = lifecycleFor(context);
  if (lifecycle.creating) {
    throw new Error("Cloudflare Agent lifecycle operation is already in progress");
  }
  if (lifecycle.active !== undefined) {
    throw new Error("Cloudflare Agent shutdown must complete before pruning durability receipts");
  }
  lifecycle.creating = true;
  try {
    const storage = context.storage;
    const durability = createCloudflareDurabilityStore(storage);
    initializeAgentStorage(storage);
    const stateId = storedStateId(storage) ?? legacyStateId(storage);
    if (stateId === undefined) return;
    const routeHost = {};
    const route = (await loadDurabilityRuntime()).own(
      routeHost,
      durability,
      stateId,
    );
    try {
      installHostBridge();
      await initializeBrowserEngine({ module });
      await pruneWasmDurableReceipts(route.id, stateId, terminalReceiptRetention);
    } finally {
      route.abandon();
    }
  } finally {
    lifecycle.creating = false;
  }
}

/** @internal Creates one Agent with an explicitly supplied package module. */
export async function create(module, owner, options = {}, hostAgent = HostAgent) {
  const resolved = resolveOwner(owner);
  const lifecycle = lifecycleFor(resolved.context);
  if (lifecycle.creating) {
    throw new Error("Cloudflare Agent creation is already in progress for this Durable Object");
  }
  if (lifecycle.active !== undefined) {
    throw new Error("Cloudflare Agent shutdown must complete before create");
  }
  lifecycle.creating = true;
  try {
    return await createOwned(module, resolved, options, hostAgent, lifecycle);
  } finally {
    lifecycle.creating = false;
  }
}

async function createOwned(module, resolved, options, hostAgent, lifecycle) {
  const { context, egress, subject } = resolved;
  const configured = applicationOptions(options);
  const {
    durabilityId,
    eventPersistence = "durable",
    [INTERNAL_RUNTIME]: internalRuntime,
    [INTERNAL_CONFIGURATION]: internalConfiguration,
    ...agentOptions
  } = configured;
  if (internalRuntime !== undefined
    && (!internalRuntime || typeof internalRuntime !== "object" || Array.isArray(internalRuntime))) {
    throw new TypeError("Cloudflare Agent internal runtime options must be an object");
  }
  if (internalRuntime?.subagentLifecycle !== undefined
    && typeof internalRuntime.subagentLifecycle !== "function") {
    throw new TypeError("Cloudflare Agent subagent lifecycle hook must be a function");
  }
  validateInternalConfiguration(internalConfiguration);
  const eventSocket = eventPersistence === "durable"
    ? createCloudflareEventSocket(context)
    : undefined;
  if (eventPersistence === "caller") clearCloudflareEventSocket(context);
  const durability = createCloudflareDurabilityStore(context.storage);
  const { sessionId, stateId } = durableIdentity(context.storage, durabilityId);
  const endpoint = cloudflareEgress({
    binding: scopeCloudflareEgress(egress, subject),
  });
  const startup = deferred();
  const transport = Transport.hostManaged({
    ...endpoint,
    websocketPreconnect: true,
    async createWebSocket(url, id, request) {
      try {
        const opened = await endpoint.createWebSocket(url, id, request);
        if (request.authorization === "preconnect") startup.resolve();
        return opened;
      } catch (error) {
        if (request.authorization === "preconnect") startup.reject(error);
        throw error;
      }
    },
  });

  const sessionReservation = prepareCloudflareAgentSession(sessionId, subject);
  const subagentSessions = cloudflareSubagentSessions(
    context.storage,
    sessionReservation,
    internalRuntime?.subagentLifecycle,
  );
  let agent;
  let watcher;
  let unwatch;
  try {
    agent = await hostAgent.create({
      ...agentOptions,
      ...(internalConfiguration === undefined ? {} : {
        model: internalConfiguration.model,
        thinking: internalConfiguration.thinking,
        reasoningMode: internalConfiguration.reasoning_mode,
        fastMode: internalConfiguration.fast_mode,
      }),
      module,
      toolMode: internalRuntime?.toolMode ?? "direct",
      codeEvaluator: internalRuntime?.codeEvaluator,
      [Symbol.for("nanocodex.browser.internalRuntime")]: {
        toolProviders: internalRuntime?.toolProviders,
        subagentMaxConcurrency: internalRuntime?.subagentMaxConcurrency,
        subagentSessions,
        [CLOUDFLARE_SESSION_RESERVATION]: sessionReservation,
      },
      transport,
      sessionId,
      durability,
      durabilityId: stateId,
    });
    await withTimeout(
      startup.promise,
      STARTUP_TIMEOUT_MS,
      "Cloudflare Agent EGRESS startup validation timed out",
    );

    if (eventSocket !== undefined) {
      watcher = agent.events.watch();
      unwatch = watcher.onEvent((event) => {
        try {
          eventSocket.publish(event);
        } catch (error) {
          unwatch?.();
          eventSocket.fail(error);
          console.error("Nanocodex Cloudflare event projection failed", error);
        }
      });
    }
    const exposed = agent.extend((owned) => ({
      events: {
        connect: (request) => eventSocket?.connect(request) ?? Response.json(
          { error: "event_persistence_caller_owned" },
          { status: 409 },
        ),
      },
      turn: {
        ...owned.turn,
        route: (options) => routePrompt(owned, options),
      },
    }));
    const active = {};
    lifecycle.active = active;
    observeAgentRelease(exposed, () => {
      if (lifecycle.active === active) lifecycle.active = undefined;
    });
    commitCloudflareAgentSession(sessionReservation);
    return exposed;
  } catch (error) {
    const cleanupErrors = [];
    try { unwatch?.(); } catch (cleanupError) { cleanupErrors.push(cleanupError); }
    try { watcher?.off(); } catch (cleanupError) { cleanupErrors.push(cleanupError); }
    if (agent) {
      try { await agent.session.shutdown(); } catch (cleanupError) { cleanupErrors.push(cleanupError); }
    }
    releaseAgentSession(sessionReservation);
    if (cleanupErrors.length > 0) {
      const cause = new AggregateError(
        [error, ...cleanupErrors],
        "Cloudflare Agent creation and resource rollback both failed",
      );
      throw Object.assign(
        new Error(
          `Cloudflare Agent creation failed and rollback requires reopen: ${errorMessage(error)}`,
          { cause },
        ),
        { code: "reopen_required" },
      );
    }
    throw error;
  }
}

function lifecycleFor(context) {
  let lifecycle = lifecycles.get(context);
  if (lifecycle === undefined) {
    lifecycle = { active: undefined, creating: false };
    lifecycles.set(context, lifecycle);
  }
  return lifecycle;
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

/** @internal Creates one non-durable Agent in the current Cloudflare isolate. */
export async function createEphemeral(module, owner, options = {}) {
  const { egress, subject } = resolveOwner(owner);
  const agentOptions = ephemeralApplicationOptions(options);
  const endpoint = cloudflareEgress({
    binding: scopeCloudflareEgress(egress, subject),
  });
  const startup = deferred();
  const transport = Transport.hostManaged({
    ...endpoint,
    websocketPreconnect: true,
    async createWebSocket(url, id, request) {
      try {
        const opened = await endpoint.createWebSocket(url, id, request);
        if (request.authorization === "preconnect") startup.resolve();
        return opened;
      } catch (error) {
        if (request.authorization === "preconnect") startup.reject(error);
        throw error;
      }
    },
  });

  let agent;
  try {
    agent = await HostAgent.create({
      ...agentOptions,
      module,
      toolMode: "direct",
      transport,
    });
    await withTimeout(
      startup.promise,
      STARTUP_TIMEOUT_MS,
      "Cloudflare ephemeral Agent EGRESS startup validation timed out",
    );
    return agent;
  } catch (error) {
    if (agent) await agent.session.shutdown().catch(() => {});
    throw error;
  }
}

function resolveOwner(owner) {
  const context = resolveContext(owner);
  const egress = owner.env?.NANOCODEX;
  if (!egress || typeof egress.fetch !== "function") {
    throw new TypeError(
      "Cloudflare Agent.create requires the private owner.env.NANOCODEX Service Binding",
    );
  }
  const subject = context.id?.toString?.();
  if (typeof subject !== "string" || !subject) {
    throw new TypeError("Cloudflare Agent.create requires owner.ctx.id");
  }
  return { context, egress, subject };
}

function resolveContext(owner) {
  if (!owner || (typeof owner !== "object" && typeof owner !== "function")) {
    throw new TypeError("Cloudflare Agent.create requires a Durable Object instance");
  }
  const context = owner.ctx;
  if (!context || typeof context !== "object") {
    throw new TypeError("Cloudflare Agent.create requires owner.ctx");
  }
  return context;
}

function applicationOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Cloudflare Agent.create options must be an object");
  }
  for (const name of Object.keys(options)) {
    if (!APPLICATION_OPTIONS.has(name)) {
      throw new TypeError(
        `Cloudflare Agent.create does not accept ${name}; only durabilityId, eventPersistence, instructions, additionalInstructions, terminalReceiptRetention, and tools are configurable`,
      );
    }
  }
  if (options.eventPersistence !== undefined
    && options.eventPersistence !== "durable"
    && options.eventPersistence !== "caller") {
    throw new TypeError(
      "Cloudflare Agent.create eventPersistence must be durable or caller",
    );
  }
  if (options.terminalReceiptRetention !== undefined
    && (!Number.isSafeInteger(options.terminalReceiptRetention)
      || options.terminalReceiptRetention < 0
      || options.terminalReceiptRetention > 4_096)) {
    throw new TypeError(
      "Cloudflare Agent.create terminalReceiptRetention must be an integer from 0 through 4096",
    );
  }
  return options;
}

function validateInternalConfiguration(configuration) {
  if (configuration === undefined) return;
  if (!configuration || typeof configuration !== "object" || Array.isArray(configuration)
    || Reflect.ownKeys(configuration).some((key) => ![
      "model",
      "thinking",
      "reasoning_mode",
      "fast_mode",
    ].includes(key))
    || !["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-6-astra"]
      .includes(configuration.model)
    || !["none", "low", "medium", "high", "xhigh", "max"].includes(configuration.thinking)
    || !["standard", "pro"].includes(configuration.reasoning_mode)
    || typeof configuration.fast_mode !== "boolean"
    || (configuration.model === "gpt-6-astra" && configuration.thinking === "none")) {
    throw new TypeError("Cloudflare Agent internal configuration is invalid");
  }
}

function ephemeralApplicationOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Cloudflare Agent.createEphemeral options must be an object");
  }
  for (const name of Object.keys(options)) {
    if (!EPHEMERAL_APPLICATION_OPTIONS.has(name)) {
      throw new TypeError(
        `Cloudflare Agent.createEphemeral does not accept ${name}; transport and runtime policy are owned by the adapter`,
      );
    }
  }
  return options;
}

function durableIdentity(storage, configuredStateId) {
  initializeAgentStorage(storage);
  if (configuredStateId !== undefined
    && (typeof configuredStateId !== "string" || !configuredStateId.trim())) {
    throw new TypeError("Cloudflare Agent durabilityId must be a non-empty string");
  }
  const previousSessionId = storedSessionId(storage);
  const previousStateId = storedStateId(storage);
  if (previousStateId !== undefined
    && configuredStateId !== undefined
    && previousStateId !== configuredStateId) {
    throw new Error("Cloudflare Agent durabilityId does not match the retained state identity");
  }
  const generated = previousSessionId ?? uuidV7();
  const generatedStateId = previousStateId
    ?? configuredStateId
    ?? (previousSessionId === undefined ? generated : `cloudflare:${previousSessionId}`);
  storage.transactionSync(() => {
    storage.sql.exec(
      "INSERT OR IGNORE INTO nanocodex_cloudflare_agent (singleton, session_id) VALUES (1, ?)",
      generated,
    );
    storage.sql.exec(
      "INSERT OR IGNORE INTO nanocodex_cloudflare_durability (singleton, state_id) VALUES (1, ?)",
      generatedStateId,
    );
  });
  const sessionId = storedSessionId(storage);
  const stateId = storedStateId(storage);
  if (typeof sessionId !== "string" || !sessionId) {
    throw new Error("Cloudflare Agent failed to persist its runtime session ID");
  }
  if (typeof stateId !== "string" || !stateId) {
    throw new Error("Cloudflare Agent failed to persist its durability state ID");
  }
  return { sessionId, stateId };
}

function initializeAgentStorage(storage) {
  storage.sql.exec(`
    CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_agent (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
      session_id TEXT NOT NULL UNIQUE
    )
  `);
  storage.sql.exec(`
    CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_durability (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
      state_id TEXT NOT NULL UNIQUE
    )
  `);
  storage.sql.exec(`
    CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_subagents (
      session_id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL UNIQUE,
      descriptor_json TEXT NOT NULL,
      host_context_ref TEXT
    )
  `);
  const subagentColumns = storage.sql.exec(
    "PRAGMA table_info('nanocodex_cloudflare_subagents')",
  ).toArray();
  if (!subagentColumns.some(({ name }) => name === "host_context_ref")) {
    storage.sql.exec(
      "ALTER TABLE nanocodex_cloudflare_subagents ADD COLUMN host_context_ref TEXT",
    );
  }
}

function cloudflareSubagentSessions(storage, reservation, lifecycle) {
  const restoredHostContextRefs = new Map();
  return Object.freeze({
    restore() {
      const restored = storage.sql.exec(
        "SELECT descriptor_json, host_context_ref FROM nanocodex_cloudflare_subagents",
      ).toArray().map(({ descriptor_json, host_context_ref }) => {
        const descriptor = Object.freeze(JSON.parse(descriptor_json));
        restoredHostContextRefs.set(
          descriptor.sessionId,
          host_context_ref === null ? undefined : host_context_ref,
        );
        return descriptor;
      });
      return Object.freeze(restored);
    },
    hostContextRef(sessionId) {
      return restoredHostContextRefs.get(sessionId);
    },
    bind(sessionId, descriptor, hostContextRef) {
      if (!mayBindCloudflareSubagentSession(reservation)) return;
      if (hostContextRef !== undefined
        && (typeof hostContextRef !== "string" || hostContextRef.length === 0)) {
        throw new TypeError("subagent host context ref must be a non-empty string when supplied");
      }
      const type = restoredHostContextRefs.has(sessionId) ? "reconstruct" : "bind";
      storage.transactionSync(() => {
        storage.sql.exec(
          `INSERT INTO nanocodex_cloudflare_subagents
             (session_id, agent_id, descriptor_json, host_context_ref) VALUES (?, ?, ?, ?)
           ON CONFLICT (session_id) DO UPDATE SET
             agent_id = excluded.agent_id,
             descriptor_json = excluded.descriptor_json,
             host_context_ref = excluded.host_context_ref`,
          sessionId,
          descriptor.agentId,
          JSON.stringify(descriptor),
          hostContextRef ?? null,
        );
        notifySubagentLifecycle(lifecycle, {
          type,
          rootSessionId: reservation.sessionId,
          sessionId,
          descriptor,
          hostContextRef,
        });
      });
      restoredHostContextRefs.delete(sessionId);
    },
    release(sessionId, hostContextRef) {
      if (!mayReleaseCloudflareSubagentSession(reservation)) return;
      storage.transactionSync(() => {
        const retained = storage.sql.exec(
          `SELECT 1 AS retained FROM nanocodex_cloudflare_subagents
           WHERE session_id = ? AND host_context_ref IS ?`,
          sessionId,
          hostContextRef ?? null,
        ).toArray();
        if (retained.length === 0) return;
        notifySubagentLifecycle(lifecycle, {
          type: "release",
          rootSessionId: reservation.sessionId,
          sessionId,
          hostContextRef,
        });
        storage.sql.exec(
          `DELETE FROM nanocodex_cloudflare_subagents
           WHERE session_id = ? AND host_context_ref IS ?`,
          sessionId,
          hostContextRef ?? null,
        );
      });
    },
  });
}

function notifySubagentLifecycle(lifecycle, event) {
  if (lifecycle === undefined) return;
  lifecycle(Object.freeze(event));
}

function storedSessionId(storage) {
  return storage.sql.exec(
    "SELECT session_id FROM nanocodex_cloudflare_agent WHERE singleton = 1",
  ).toArray()[0]?.session_id;
}

function storedStateId(storage) {
  return storage.sql.exec(
    "SELECT state_id FROM nanocodex_cloudflare_durability WHERE singleton = 1",
  ).toArray()[0]?.state_id;
}

function legacyStateId(storage) {
  const sessionId = storedSessionId(storage);
  return sessionId === undefined ? undefined : `cloudflare:${sessionId}`;
}

function uuidV7() {
  if (typeof globalThis.crypto?.getRandomValues !== "function") {
    throw new Error("Cloudflare Agent requires crypto.getRandomValues()");
  }
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16));
  let timestamp = Date.now();
  for (let index = 5; index >= 0; index -= 1) {
    bytes[index] = timestamp % 256;
    timestamp = Math.floor(timestamp / 256);
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const encoded = [...bytes].map((byte) => byte.toString(16).padStart(2, "0"));
  return `${encoded.slice(0, 4).join("")}-${encoded.slice(4, 6).join("")}-${encoded.slice(6, 8).join("")}-${encoded.slice(8, 10).join("")}-${encoded.slice(10).join("")}`;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  // Preconnect may fail before HostAgent.create returns and installs the
  // startup waiter. Mark the original promise handled without changing what
  // the later await observes.
  void promise.catch(() => {});
  return { promise, resolve, reject };
}

async function withTimeout(promise, timeoutMs, message) {
  let timer;
  try {
    await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
