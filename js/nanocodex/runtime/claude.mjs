import {
  CLOUDFLARE_SESSION_RESERVATION, activateCloudflareAgentSession, activateHost, bindHostSession, createAgentClient, createEventChannel, createSessionId,
  defineRuntime, loadDurabilityRuntime, registerDefinitionHost, releaseDefinitionHost,
  releaseHostSession, prompt, routePrompt, compact, shutdown, getTurnHostId,
} from '../internal.mjs';
import { watch } from '../actions/events.mjs';
import { prepareHarnesses } from './harnesses.mjs';
import { createClaudeHost } from './claude-host.mjs';

const OPTION_KEYS = new Set([
  'toolMode', 'codeEvaluator', 'auth', 'fetch', 'endpoint', 'compatibilityProfile', 'subscriptionIdentity', 'model', 'instructions', 'sessionId', 'tools',
  'harness', 'harnesses', 'subagents', 'serverTools', 'durability', 'durabilityId', 'module', 'maxTokens', 'workspace',
  'cache', 'adaptiveThinking', 'keepThinking', 'thinking', 'parallelTools', 'clientToolSearch',
  'contextWindowTokens', 'autoCompactWindowTokens', 'autoCompact', 'systemBlocks', 'terminalReceiptRetention',
]);

export function toClaudeConfig(options = {}) {
  if (!options || typeof options !== 'object' || Array.isArray(options)) throw new TypeError('Claude options must be an object');
  for (const key of Object.keys(options)) if (!OPTION_KEYS.has(key)) throw new TypeError('unsupported Claude option');
  if (options.toolMode !== undefined && !['direct', 'code-only'].includes(options.toolMode)) throw new TypeError('unsupported Claude toolMode');
  if (options.codeEvaluator !== undefined && typeof options.codeEvaluator !== 'function') throw new TypeError('Claude codeEvaluator must be a function');
  if (options.toolMode === 'code-only' && typeof options.codeEvaluator !== 'function') throw new TypeError('Claude Code Mode requires an explicit codeEvaluator');
  if (options.toolMode === 'code-only' && (options.serverTools?.length || options.clientToolSearch)) throw new TypeError('Claude Code Mode does not expose provider or client search tools directly');
  if (options.harness !== undefined && options.harness !== 'claude') throw new TypeError('Claude requires harness claude');
  if (options.subagents !== undefined && (!options.subagents || typeof options.subagents !== 'object' || Array.isArray(options.subagents) || Object.keys(options.subagents).some(key => key !== 'maxConcurrency') || (options.subagents.maxConcurrency !== undefined && (!Number.isSafeInteger(options.subagents.maxConcurrency) || options.subagents.maxConcurrency < 1)))) throw new TypeError('subagents maxConcurrency must be positive');
  if (typeof options.model !== 'string' || !options.model.trim()) throw new TypeError('Claude model must be non-empty');
  if (options.fetch !== undefined && typeof options.fetch !== 'function') throw new TypeError('Claude fetch must be a function');
  if (options.compatibilityProfile !== undefined && options.compatibilityProfile !== 'subscription') throw new TypeError('unsupported Claude compatibilityProfile');
  if (options.compatibilityProfile !== undefined && options.endpoint === undefined) throw new TypeError('Claude compatibilityProfile requires an explicit endpoint');
  if (options.endpoint !== undefined) {
    let endpoint;
    try { endpoint = new URL(options.endpoint); } catch { throw new TypeError('Claude endpoint must be an absolute HTTP URL'); }
    if (!['https:', 'http:'].includes(endpoint.protocol) || endpoint.username || endpoint.password || endpoint.hash) throw new TypeError('Claude endpoint must be an absolute HTTP URL without credentials or fragment');
  }
  if ((options.durability === undefined) !== (options.durabilityId === undefined)) throw new TypeError('durability and durabilityId must be supplied together');
  for (const key of ['sessionId', 'durabilityId']) if (options[key] !== undefined && (typeof options[key] !== 'string' || !options[key])) throw new TypeError(`Claude ${key} must be non-empty`);
  for (const key of ['maxTokens', 'contextWindowTokens', 'autoCompactWindowTokens']) if (options[key] !== undefined && (!Number.isSafeInteger(options[key]) || options[key] < 1)) throw new TypeError(`Claude ${key} must be a positive safe integer`);
  if (options.maxTokens > 4294967295) throw new TypeError('Claude maxTokens exceeds uint32');
  for (const key of ['adaptiveThinking', 'keepThinking', 'parallelTools', 'clientToolSearch', 'autoCompact']) if (options[key] !== undefined && typeof options[key] !== 'boolean') throw new TypeError(`Claude ${key} must be boolean`);
  if (options.autoCompact === false) throw new TypeError('disabling Claude autoCompact is unsupported');
  for (const key of ['instructions', 'workspace']) if (options[key] !== undefined && typeof options[key] !== 'string') throw new TypeError(`Claude ${key} must be a string`);
  if (options.thinking !== undefined && !['none', 'low', 'medium', 'high', 'xhigh', 'max'].includes(options.thinking)) throw new TypeError('unsupported Claude thinking');
  if (options.cache !== undefined && !['off', '5m', '1h'].includes(options.cache)) throw new TypeError('unsupported Claude cache');
  if (options.instructions !== undefined && options.systemBlocks !== undefined) throw new TypeError('instructions and systemBlocks are mutually exclusive');
  for (const key of ['systemBlocks', 'serverTools']) if (options[key] !== undefined && !Array.isArray(options[key])) throw new TypeError(`Claude ${key} must be an array`);
  if (options.terminalReceiptRetention !== undefined && (options.durability === undefined || !Number.isSafeInteger(options.terminalReceiptRetention) || options.terminalReceiptRetention < 0 || options.terminalReceiptRetention > 4096)) throw new TypeError('terminalReceiptRetention requires durability and must be 0..4096');
  if (options.durabilityId !== undefined && options.sessionId !== undefined && options.durabilityId !== options.sessionId) throw new TypeError('durable Claude sessionId must equal durabilityId');
  if (options.subscriptionIdentity !== undefined) {
    const identity = options.subscriptionIdentity;
    if (options.compatibilityProfile !== 'subscription' || !identity || typeof identity !== 'object' || Array.isArray(identity)) throw new TypeError('subscriptionIdentity requires subscription compatibility');
    for (const [key,value] of Object.entries(identity)) {
      if (!['installId','accountUuid','userId','platform','arch','version'].includes(key) || typeof value !== 'string' || !value || value.length > 8192 || /[\u0000-\u001f\u007f]/.test(value)) throw new TypeError('invalid subscriptionIdentity');
    }
  }
  const config = {};
  for (const key of OPTION_KEYS) if (!['codeEvaluator', 'auth', 'fetch', 'tools', 'module', 'durability', 'compatibilityProfile', 'harness', 'harnesses', 'subagents'].includes(key) && options[key] !== undefined) config[key] = options[key];
  if (options.compatibilityProfile !== undefined) {
    config.subscriptionCompatibility = true;
    config.subscriptionIdentity = { ...config.subscriptionIdentity };
    const process = globalThis.process;
    if (typeof process?.platform === 'string') config.subscriptionIdentity.platform ??= process.platform;
    if (typeof process?.arch === 'string') config.subscriptionIdentity.arch ??= process.arch;
    if (typeof process?.env?.PI_AI_CLAUDE_CODE_VERSION === 'string' && process.env.PI_AI_CLAUDE_CODE_VERSION) config.subscriptionIdentity.version ??= process.env.PI_AI_CLAUDE_CODE_VERSION;
  }
  // Snapshot caller-owned nested native definitions before any asynchronous loading.
  return JSON.parse(JSON.stringify(config));
}

/** Shared host lifecycle; loader selects the actual Nanoclaude WASM class. */
export async function createClaude(options, load, type, harnessDefaults) {
  const reservation = options?.[CLOUDFLARE_SESSION_RESERVATION];
  const internalRuntime = options?.[Symbol.for("nanocodex.browser.internalRuntime")];
  const config = toClaudeConfig(options);
  config.sessionId ??= options.durabilityId ?? createSessionId();
  const { durability, durabilityId, module } = options;
  const events = createEventChannel();
  const host = createClaudeHost({ auth: options.auth, tools: options.tools, onEvent: events.emit, fetch: options.fetch, endpoint: options.endpoint,
    subagentSessions: internalRuntime?.subagentSessions, subagentRouting: internalRuntime?.subagentRouting,
    toolMode: options.toolMode, codeEvaluator: options.codeEvaluator,
    codeEffectJournal: internalRuntime?.codeEffectJournal, traceTool: internalRuntime?.traceTool });
  const stopJournalEvents = internalRuntime?.codeEffectJournal ? events.subscribe(() => {}) : undefined;
  let harnesses;
  try { harnesses = await prepareHarnesses(options.harnesses, events.emit, { ...harnessDefaults,
    subagentSessions: internalRuntime?.subagentSessions, subagentRouting: internalRuntime?.subagentRouting,
    toolProviders: internalRuntime?.toolProviders,
    codeEffectJournal: internalRuntime?.codeEffectJournal, traceTool: internalRuntime?.traceTool,
  }); }
  catch (error) { stopJournalEvents?.(); host.dispose(); throw error; }
  config.codexHarness = harnesses.codex;
  config.subagentRouting = internalRuntime?.subagentRouting !== undefined;
  if (options.subagents !== undefined) config.subagents = options.subagents.maxConcurrency === undefined ? {} : { max_concurrency: options.subagents.maxConcurrency };
  options = undefined; // Do not retain caller credentials in runtime lifecycle closures.
  // Retain the durable owner so a reconstructed Cloudflare runtime can replace
  // this host without weakening the cross-owner session guard.
  const hostDefinitionId = registerDefinitionHost(host, reservation);
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
    stopJournalEvents?.();
    host.dispose();
    void harnesses.close();
  };
  const runtime = defineRuntime({
    key: `claude-${type}-wasm`, name: 'Nanoclaude WASM', type,
    async create() {
      try {
        if (durability !== undefined) {
          owner = (await loadDurabilityRuntime()).own(host, durability, durabilityId);
          config.durabilityHostId = owner.id;
        }
        activateHost(host);
        const Nanoclaude = await load(module);
        activateHost(host);
        if (typeof Nanoclaude?.create !== 'function') throw new Error('this WASM build does not expose Nanoclaude');
        // Construction acquires the durable fence before adoption replaces
        // the live host route, matching the Codex lifecycle.
        const raw = await Nanoclaude.create(JSON.stringify(config));
        if (!raw || typeof raw.prompt !== 'function') {
          raw?.free?.();
          throw new TypeError('the runtime returned an invalid Nanoclaude handle');
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
    decorate: (agent, raw) => {
      const own = (turn) => {
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
      };
      return agent.extend(() => ({
        events: { watch: (options) => watch(agent, options) },
        session: { compact: () => track(compact(agent)), cancel: () => { host.cancelCodeTurn(raw.sessionId); return raw.cancel(); }, shutdown: () => shutdown(agent) },
        turn: {
          prompt: (options) => {
            const input = options?.input;
            if (typeof input === 'string' ? !input.trim() : !Array.isArray(input) || input.length === 0) {
              throw new TypeError('Claude prompt requires non-empty text or content');
            }
            return own(prompt(agent, options));
          },
          // Live frontends (realtime voice) steer the active turn or start one.
          // Steered input joins a turn that already owns its host routes.
          route: async (options) => {
            if (typeof options?.input !== 'string' || !options.input.trim()) throw new TypeError('Claude live input requires non-empty text');
            const turn = await routePrompt(agent, options);
            return turn === undefined ? undefined : own(turn);
          },
        },
      }));
    },
  });
  try { return await createAgentClient(runtime, { sessionId: config.sessionId }, reservation); }
  catch (error) { cleanup(); throw error; }
}
