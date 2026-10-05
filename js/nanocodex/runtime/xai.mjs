import {
  CLOUDFLARE_SESSION_RESERVATION, activateCloudflareAgentSession, activateHost, bindHostSession, createAgentClient, createEventChannel, createSessionId,
  defineRuntime, loadDurabilityRuntime, registerDefinitionHost, releaseDefinitionHost,
  releaseHostSession, prompt, compact, shutdown, getTurnHostId,
} from '../internal.mjs';
import { watch } from '../actions/events.mjs';
import { prepareHarnesses } from './harnesses.mjs';
import { createXaiHost } from './xai-host.mjs';

const OPTION_KEYS = new Set([
  'auth', 'fetch', 'endpoint', 'model', 'instructions', 'sessionId', 'tools',
  'harness', 'harnesses', 'subagents', 'serverTools', 'durability', 'durabilityId', 'module', 'workspace',
  'thinking', 'contextWindowTokens', 'autoCompactThresholdPercent', 'maxSteps', 'maxRetries', 'repetitionLimit', 'compactionKeepTail', 'requestTimeoutMs', 'terminalReceiptRetention',
]);
export function toXaiConfig(options = {}) {
  if (!options || typeof options !== 'object' || Array.isArray(options)) throw new TypeError('Xai options must be an object');
  for (const key of Object.keys(options)) if (!OPTION_KEYS.has(key)) throw new TypeError('unsupported Xai option');
  if (options.harness !== undefined && options.harness !== 'xai') throw new TypeError('Xai requires harness xai');
  if (typeof options.model !== 'string' || !options.model.trim()) throw new TypeError('Xai model must be non-empty');
  if (options.fetch !== undefined && typeof options.fetch !== 'function') throw new TypeError('Xai fetch must be a function');
  if (options.endpoint !== undefined) {
    let endpoint;
    try { endpoint = new URL(options.endpoint); } catch { throw new TypeError('Xai endpoint must be an absolute HTTP URL'); }
    if (!['https:', 'http:'].includes(endpoint.protocol) || endpoint.username || endpoint.password || endpoint.hash) throw new TypeError('Xai endpoint must be an absolute HTTP URL without credentials or fragment');
  }
  if ((options.durability === undefined) !== (options.durabilityId === undefined)) throw new TypeError('durability and durabilityId must be supplied together');
  for (const key of ['sessionId', 'durabilityId']) if (options[key] !== undefined && (typeof options[key] !== 'string' || !options[key].trim())) throw new TypeError(`Xai ${key} must be non-empty`);
  for (const key of ['contextWindowTokens', 'maxSteps', 'requestTimeoutMs', 'autoCompactThresholdPercent']) if (options[key] !== undefined && (!Number.isSafeInteger(options[key]) || options[key] < 1)) throw new TypeError(`Xai ${key} must be a positive safe integer`);
  for (const key of ['maxRetries', 'repetitionLimit', 'compactionKeepTail']) {
    const minimum = key === 'repetitionLimit' ? 1 : 0;
    if (options[key] !== undefined && (!Number.isSafeInteger(options[key]) || options[key] < minimum || options[key] > 0xffff_ffff)) throw new TypeError(`Xai ${key} must be an integer from ${minimum} through 4294967295`);
  }
  if (options.autoCompactThresholdPercent > 100) throw new TypeError('autoCompactThresholdPercent must be 1..100');
  for (const key of ['instructions', 'workspace']) if (options[key] !== undefined && typeof options[key] !== 'string') throw new TypeError(`Xai ${key} must be a string`);
  if (options.thinking !== undefined && !['low', 'medium', 'high', 'xhigh'].includes(options.thinking)) throw new TypeError('unsupported Xai thinking');
  if (options.serverTools !== undefined && !Array.isArray(options.serverTools)) throw new TypeError('Xai serverTools must be an array');
  if (options.terminalReceiptRetention !== undefined && (options.durability === undefined || !Number.isSafeInteger(options.terminalReceiptRetention) || options.terminalReceiptRetention < 0 || options.terminalReceiptRetention > 4096)) throw new TypeError('terminalReceiptRetention requires durability and must be 0..4096');
  if (options.durabilityId !== undefined && options.sessionId !== undefined && options.durabilityId !== options.sessionId) throw new TypeError('durable Xai sessionId must equal durabilityId');
  if (options.subagents !== undefined && (!options.subagents || typeof options.subagents !== 'object' || Array.isArray(options.subagents) || Object.keys(options.subagents).some(key => key !== 'maxConcurrency') || (options.subagents.maxConcurrency !== undefined && (!Number.isSafeInteger(options.subagents.maxConcurrency) || options.subagents.maxConcurrency < 1)))) throw new TypeError('subagents maxConcurrency must be positive');
  const config = {};
  for (const key of OPTION_KEYS) if (!['auth', 'fetch', 'tools', 'module', 'durability', 'harness', 'harnesses', 'subagents'].includes(key) && options[key] !== undefined) config[key] = options[key];
  return JSON.parse(JSON.stringify(config));
}

/** Shared host lifecycle; loader selects the actual Nanoxai WASM class. */
export async function createXai(options, load, type, harnessDefaults) {
  const reservation = options?.[CLOUDFLARE_SESSION_RESERVATION];
  const internalRuntime = options?.[Symbol.for("nanocodex.browser.internalRuntime")];
  const config = toXaiConfig(options);
  config.sessionId ??= options.durabilityId ?? createSessionId();
  const { durability, durabilityId, module } = options;
  const events = createEventChannel();
  const host = createXaiHost({ auth: options.auth, tools: options.tools, onEvent: events.emit, fetch: options.fetch, endpoint: options.endpoint ?? "https://api.x.ai/v1/responses",
    subagentSessions: internalRuntime?.subagentSessions, subagentRouting: internalRuntime?.subagentRouting });
  let harnesses;
  try { harnesses = await prepareHarnesses(options.harnesses, events.emit, { ...harnessDefaults,
    subagentSessions: internalRuntime?.subagentSessions, subagentRouting: internalRuntime?.subagentRouting,
    toolProviders: internalRuntime?.toolProviders,
  }); }
  catch (error) { host.dispose(); throw error; }
  config.codexHarness = harnesses.codex;
  config.claudeHarness = harnesses.claude;
  config.subagentRouting = internalRuntime?.subagentRouting !== undefined;
  if (options.subagents !== undefined) config.subagents = options.subagents.maxConcurrency === undefined ? {} : { max_concurrency: options.subagents.maxConcurrency };
  options = undefined; // Do not retain caller credentials in runtime lifecycle closures.
  const hostDefinitionId = registerDefinitionHost(host);
  config.hostDefinitionId = hostDefinitionId;
  config.authHostId = hostDefinitionId;
  config.tools = JSON.parse(host.toolDefinitions());
  let owner;
  let cleaned = false;
  let detached = false;
  let detachedRaw;
  const pending = new Set();
  const finishDetached = () => {
    if (!detached || pending.size) return;
    cleanup();
    if (detachedRaw) {
      const raw = detachedRaw;
      detachedRaw = undefined;
      raw.free();
    }
  };
  const track = (operation) => {
    const result = Promise.resolve(operation).finally(() => {
      pending.delete(result);
      finishDetached();
    });
    pending.add(result);
    // Lifecycle observation must not introduce an unhandled rejection when callers detach.
    void result.catch(() => {});
    return result;
  };
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    releaseHostSession(host, config.sessionId);
    owner?.release();
    owner?.abandon();
    releaseDefinitionHost(hostDefinitionId);
    host.dispose();
    void harnesses.close();
  };
  const runtime = defineRuntime({
    key: `xai-${type}-wasm`, name: 'Nanoxai WASM', type,
    async create() {
      try {
        if (durability !== undefined) {
          owner = (await loadDurabilityRuntime()).own(host, durability, durabilityId);
          config.durabilityHostId = owner.id;
        }
        activateHost(host);
        const Nanoxai = await load(module);
        activateHost(host);
        if (typeof Nanoxai?.create !== 'function') throw new Error('this WASM build does not expose Nanoxai');
        bindHostSession(host, config.sessionId, reservation);
        const raw = await Nanoxai.create(JSON.stringify(config));
        if (!raw || typeof raw.prompt !== 'function') {
          raw?.free?.();
          throw new TypeError('the runtime returned an invalid Nanoxai handle');
        }
        if (reservation) activateCloudflareAgentSession(reservation);
        return raw;
      } catch (error) { cleanup(); throw error; }
    },
    adopt(raw) {
      owner?.retain();
      try { bindHostSession(host, raw.sessionId, reservation); events.addSource(raw); }
      catch (error) { cleanup(); throw error; }
    },
    release(raw) {
      events.removeSource(raw);
      detached = true;
      finishDetached();
    },
    dispose(raw) {
      if (pending.size) detachedRaw = raw;
      else raw.free();
    },
    async shutdown(raw) { host.cancelCodeTurn(raw.sessionId); await raw.shutdown(); },
    subscribe: events.subscribe,
    decorate: (agent, raw) => agent.extend(() => ({
      events: { watch: (options) => watch(agent, options) },
      session: { context: async () => JSON.parse(await raw.context()), compact: () => track(compact(agent)), cancel: () => { host.cancelCodeTurn(raw.sessionId); return raw.cancel(); }, shutdown: () => shutdown(agent) },
      turn: { prompt: (options) => {
        if (typeof options?.input !== 'string' || !options.input.trim()) throw new TypeError('Xai prompt requires non-empty text');
        const turn = prompt(agent, options);
        const identity = getTurnHostId(turn);
        void identity.catch(() => {});
        // Observe every issued turn, even if the caller never requests its result.
        // Accepted work owns host/auth/durability routes until its terminal receipt settles.
        const result = track(turn.result().finally(async () => {
          const id = await identity.catch(() => undefined);
          if (id !== undefined) host.releaseTurn(raw.sessionId, id);
        }));
        let disposed = false;
        return Object.freeze({ ...turn,
          result: () => disposed ? Promise.reject(new Error('the Nanocodex turn has been disposed')) : result,
          dispose: () => { if (!disposed) { disposed = true; turn.dispose(); } },
          cancel: async () => {
            if (disposed) throw new Error('the Nanocodex turn has been disposed');
            // A durable request ID is optional; the Rust lifecycle ID is not.
            // Never guess using the current active turn: this may be a queued prompt.
            const id = await identity;
            if (id !== undefined) host.cancelCodeTurn(raw.sessionId, id);
            return turn.cancel();
          },
        });
      } },
    })),
  });
  try { return await createAgentClient(runtime, { sessionId: config.sessionId }, reservation); }
  catch (error) { cleanup(); throw error; }
}
