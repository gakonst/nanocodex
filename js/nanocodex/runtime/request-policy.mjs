import { freezeJson } from '../internal.mjs';

const handles = new WeakSet();
const encoder = new TextEncoder();
const FORMAT = 'nanocodex-request-policy-v1';
const MAX_REQUESTS = 16;
const MAX_CHECKPOINT_BYTES = 16 * 1024 * 1024;

const copy = value => JSON.parse(JSON.stringify(value));
const frozen = value => freezeJson(copy(value));
function nonempty(value, name) {
  if (typeof value !== 'string' || !value.trim()) throw new TypeError(`${name} must be nonempty`);
  return value;
}
function positive(value, name) {
  if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${name} must be a positive safe integer`);
  return value;
}
function modelCatalog(models) {
  if (!Array.isArray(models) || !models.length) throw new TypeError('physical models are required');
  const names = new Set();
  return frozen(models.map(model => {
    nonempty(model.model, 'physical model');
    if (!['claude', 'codex'].includes(model.family) || names.has(`${model.family}:${model.model}`)) throw new TypeError('invalid or duplicate physical model');
    names.add(`${model.family}:${model.model}`);
    positive(model.contextTokens, 'contextTokens');
    positive(model.maxOutputTokens, 'maxOutputTokens');
    if (model.maxOutputTokens > model.contextTokens) throw new TypeError('output capacity exceeds context capacity');
    if (model.switchGroup !== undefined) nonempty(model.switchGroup, 'switchGroup');
    return model;
  }));
}
function nativeTools(request) {
  const declaration = Array.isArray(request.input) ? request.input.find(item => item.type === 'additional_tools') : undefined;
  return declaration ? declaration.tools ?? [] : request.tools ?? [];
}
function toolName(tool) { return tool.name ?? tool.function?.name; }
function configuration(initial, patches) {
  const next = copy(initial);
  for (const patch of patches) {
    if (!patch || !['set_section', 'remove_section', 'set_tool', 'remove_tool'].includes(patch.kind)) throw new TypeError('invalid configuration patch');
    const section = patch.kind.endsWith('section');
    const list = section ? next.sections : next.tools;
    const value = section ? patch.section : patch.tool;
    const name = nonempty(patch.kind.startsWith('set') ? value?.name : patch.name, 'configuration name');
    if (patch.kind === 'set_section' && typeof value.text !== 'string') throw new TypeError('section text must be a string');
    if (patch.kind === 'set_tool' && (!value.definition || typeof value.definition !== 'object' || toolName(value.definition) !== name)) throw new TypeError('tool name must match its native definition');
    const index = list.findIndex(item => item.name === name);
    if (patch.kind.startsWith('remove')) { if (index >= 0) list.splice(index, 1); }
    else if (index < 0) list.push(copy(value));
    else list[index] = copy(value);
  }
  return next;
}
function opaque(value) {
  if (!value || typeof value !== 'object') return false;
  if (['image', 'input_image', 'input_audio', 'audio', 'document', 'video', 'encrypted_content', 'redacted_thinking'].includes(value.type)
    || value.encrypted_content !== undefined || value.signature !== undefined) return true;
  return Object.values(value).some(child => typeof child === 'object' && opaque(child));
}
function continuation(request, family) {
  if (request.previous_response_id) return true;
  const items = family === 'claude' ? request.messages : request.input;
  const last = Array.isArray(items) ? items.at(-1) : undefined;
  return last?.type === 'function_call_output'
    || (last?.role === 'user' && Array.isArray(last.content) && last.content.some(block => block.type === 'tool_result'));
}
function render(original, config, physical, family, outputTokens) {
  const request = copy(original);
  request.model = physical.model;
  // The current native catalog is the authorization ceiling on every request.
  const allowed = nativeTools(original);
  const definitions = config.tools.filter(tool => allowed.some(candidate => JSON.stringify(candidate) === JSON.stringify(tool.definition))).map(tool => tool.definition);
  const liteIndex = family === 'codex' && Array.isArray(request.input) ? request.input.findIndex(item => item.type === 'additional_tools') : -1;
  if (liteIndex >= 0) {
    if (JSON.stringify(request.input[liteIndex].tools) !== JSON.stringify(definitions)) {
      request.input[liteIndex].tools = definitions;
      delete request.input[liteIndex].id;
    }
    if (config.sections.length) {
      const developer = request.input.findIndex(item => item.type === 'message' && item.role === 'developer');
      request.input.splice(developer >= 0 ? developer + 1 : liteIndex + 1, 0,
        { type: 'message', role: 'developer', content: [{ type: 'input_text', text: config.sections.map(section => section.text).join('\n\n') }] });
    }
  } else if (allowed.length || config.tools.length) request.tools = definitions;
  if (family === 'claude') {
    const system = typeof request.system === 'string' ? [{ type: 'text', text: request.system }] : request.system ?? [];
    if (!Array.isArray(system)) throw new TypeError('unsupported Claude system blocks');
    if (config.sections.length) request.system = [...system, ...config.sections.map(section => ({ type: 'text', text: section.text }))];
    request.max_tokens = outputTokens;
  } else {
    if (request.instructions !== undefined && typeof request.instructions !== 'string') throw new TypeError('OpenAI instructions must be text');
    if (liteIndex < 0 && config.sections.length) request.instructions = [request.instructions ?? '', ...config.sections.map(section => section.text)].filter(Boolean).join('\n\n');
    request.max_output_tokens = outputTokens;
  }
  return request;
}
function warmOptions(options) {
  if (options === undefined || options.enabled === false) return undefined;
  if (options.enabled !== true) throw new TypeError('cacheWarm requires explicit enabled: true');
  if (![300, 3600].includes(options.ttlSeconds)) throw new TypeError('cacheWarm TTL must be 300 or 3600 seconds');
  for (const key of ['maxSpendUsd', 'estimatedWriteUsd', 'estimatedReadUsd', 'estimatedUncachedUsd']) {
    if (!Number.isFinite(options[key]) || options[key] < 0) throw new TypeError(`cacheWarm ${key} must be finite and nonnegative`);
  }
  if (options.maxSpendUsd === 0 || options.estimatedWriteUsd === 0) throw new TypeError('cacheWarm spend and write estimate must be positive');
  if (!Number.isFinite(options.reuseProbability) || options.reuseProbability <= 0 || options.reuseProbability > 1) throw new TypeError('cacheWarm reuseProbability must be in (0,1]');
  for (const key of ['inputUsdPerMillion', 'outputUsdPerMillion', 'cacheWriteUsdPerMillion', 'cacheReadUsdPerMillion']) {
    if (!Number.isFinite(options[key]) || options[key] < 0) throw new TypeError(`cacheWarm ${key} must be finite and nonnegative`);
  }
  positive(options.expectedReuseCount, 'expectedReuseCount');
  positive(options.expectedReuseWithinSeconds, 'expectedReuseWithinSeconds');
  if (options.expectedReuseWithinSeconds > options.ttlSeconds) throw new TypeError('expected reuse must be within cache TTL');
  const savings = options.reuseProbability * options.expectedReuseCount * (options.estimatedUncachedUsd - options.estimatedReadUsd);
  if (savings <= options.estimatedWriteUsd || options.estimatedWriteUsd > options.maxSpendUsd) throw new TypeError('cacheWarm expected savings must exceed write cost within spend limit');
  return frozen(options);
}

/** Persisted, branch-local policy using the existing durability acquire/replace contract. */
export async function create(options) {
  const { durability: store, durabilityId: stateId } = options ?? {};
  nonempty(stateId, 'policy durabilityId');
  if (!store || typeof store.acquire !== 'function' || typeof store.replace !== 'function') throw new TypeError('request policy requires a durability store');
  const models = modelCatalog(options.models);
  const selection = nonempty(options.selection, 'virtual model selection');
  if (typeof options.route !== 'function') throw new TypeError('virtual model route callback is required');
  const cacheWarm = warmOptions(options.cacheWarm);
  for (const key of ['estimateInputTokens', 'switchSafe', 'authorize', 'requestContext']) if (options[key] !== undefined && typeof options[key] !== 'function') throw new TypeError(`${key} must be a callback`);
  const ownerId = `request-policy:${crypto.randomUUID()}`;
  const owned = await store.acquire(stateId, { ownerId });
  let revision = owned.revision;
  let state = owned.payload === null ? { format: FORMAT, selection, configuration: { sections: [], tools: [] }, history: [], pending: [], routerState: options.initialState ?? null, requests: [], warms: [], reservedWarmUsd: 0, actualWarmUsd: 0 } : JSON.parse(owned.payload);
  if (encoder.encode(JSON.stringify(state)).length > MAX_CHECKPOINT_BYTES) throw new Error('request policy current checkpoint exceeds 16 MiB');
  if (state.format !== FORMAT || state.selection !== selection) throw new Error('incompatible request policy checkpoint');
  let chain = Promise.resolve();
  const serial = operation => {
    const result = chain.then(operation);
    chain = result.catch(() => {});
    return result;
  };
  async function save(next, importedRecords = []) {
    const changes = {};
    for (const field of ['requests', 'history', 'warms']) {
      changes[field] = next[field].filter(item => !state[field].some(old => JSON.stringify(old) === JSON.stringify(item)));
    }
    const key = 'request-policy/' + ownerId + '/' + revision;
    const record = { parent: state.historyTip ?? null, changes,
      configuration: next.configuration, pending: next.pending, routerState: next.routerState,
      reservedWarmUsd: next.reservedWarmUsd, actualWarmUsd: next.actualWarmUsd };
    next = { ...next, historyTip: { stateId, key },
      requests: next.requests.slice(-MAX_REQUESTS), history: next.history.slice(-MAX_REQUESTS), warms: next.warms.slice(-MAX_REQUESTS) };
    const payload = JSON.stringify(next);
    if (encoder.encode(payload).length > MAX_CHECKPOINT_BYTES) throw new Error('request policy current checkpoint exceeds 16 MiB');
    const result = await store.replace(stateId, { ownerId, fence: owned.fence, expectedRevision: revision, payload, records: [...importedRecords, { key, value: JSON.stringify(record) }] });
    if (result.status !== 'replaced') throw new Error(`request policy checkpoint ${result.status}; dispatch stopped`);
    revision = result.revision;
    state = next;
  }
  function physicalFor(family, model) {
    const physical = models.find(item => item.family === family && item.model === model);
    if (!physical) throw new Error('route selected an unapproved physical model');
    return physical;
  }
  function limits(request, physical, inputTokens, outputTokens) {
    positive(inputTokens, 'inputTokens'); positive(outputTokens, 'outputTokens');
    if (outputTokens > physical.maxOutputTokens || inputTokens > physical.contextTokens - outputTokens) throw new Error('rendered request exceeds dispatched model limits');
  }
  const api = Object.freeze({
    configure(patches) {
      const exact = copy(patches);
      return serial(async () => {
        if (!Array.isArray(exact)) throw new TypeError('patches must be an array');
        configuration(state.configuration, [...state.pending, ...exact]);
        await save({ ...state, pending: [...state.pending, ...exact] });
      });
    },
    snapshot() { return serial(() => frozen(state)); },
    history(cursor) {
      return serial(async () => {
        const selected = cursor ?? state.historyTip;
        if (!selected) return null;
        const value = await store.readRecord(selected.stateId, selected.key);
        if (value === null) throw new Error('request policy history record is unavailable');
        return frozen({ cursor: selected, ...JSON.parse(value) });
      });
    },
    prepare(original, context) {
      const exact = copy(original);
      const inputs = copy(context);
      return serial(async () => {
        if (!['claude', 'codex'].includes(inputs.family)) throw new TypeError('unsupported provider family');
        nonempty(inputs.requestId, 'requestId');
        const saved = state.requests.find(item => item.requestId === inputs.requestId);
        if (saved) {
          if (JSON.stringify(saved.original) !== JSON.stringify(exact) || JSON.stringify(saved.context) !== JSON.stringify(inputs)) throw new Error('request identity reused with changed input');
          const physical = physicalFor(saved.family, saved.dispatched);
          limits(saved.request, physical, saved.inputTokens, saved.outputTokens);
          return frozen(saved);
        }
        const previous = state.requests.at(-1);
        if (continuation(exact, inputs.family) && previous && inputs.continuationOf === undefined) throw new Error('tool continuation requires continuationOf');
        const predecessor = inputs.continuationOf === undefined ? undefined : previous?.requestId === inputs.continuationOf ? previous : undefined;
        if (inputs.continuationOf !== undefined && !predecessor) throw new Error('unknown continuation request');
        if (predecessor && state.pending.length) throw new Error('configuration changes require a new request boundary');
        let choice = predecessor ? { model: predecessor.dispatched, state: state.routerState } : await options.route(frozen({ original: exact, context: inputs, selection, state: state.routerState, previous: previous?.dispatched ?? null, models }));
        if (!choice || typeof choice.model !== 'string' || !Object.hasOwn(choice, 'state')) throw new TypeError('route must return model and JSON state');
        choice = copy(choice);
        const physical = physicalFor(inputs.family, choice.model);
        if (previous && previous.dispatched !== physical.model) {
          const prior = physicalFor(previous.family, previous.dispatched);
          if (predecessor || inputs.switchSafe !== true || inputs.family === 'claude' || opaque(exact) || prior.family !== physical.family || !physical.switchGroup || prior.switchGroup !== physical.switchGroup) throw new Error('native transcript does not permit physical model switch');
        }
        const initial = state.requests.length ? state.configuration : { sections: [], tools: nativeTools(exact).map(definition => ({ name: nonempty(toolName(definition), 'native tool name'), definition })) };
        const config = configuration(initial, state.pending);
        // A patch cannot introduce a declaration absent from the current native catalog.
        if (state.pending.some(patch => patch.kind === 'set_tool' && !nativeTools(exact).some(tool => JSON.stringify(tool) === JSON.stringify(patch.tool.definition)))) throw new Error('configuration includes an unauthorized tool declaration');
        const outputTokens = inputs.outputTokens ?? exact.max_tokens ?? exact.max_output_tokens ?? physical.maxOutputTokens;
        const request = render(exact, config, physical, inputs.family, outputTokens);
        if (opaque(request) && inputs.inputTokens === undefined && !options.estimateInputTokens) throw new Error('opaque or multimodal requests require estimateInputTokens');
        const inputTokens = inputs.inputTokens ?? (options.estimateInputTokens ? await options.estimateInputTokens(frozen(request), inputs.family) : encoder.encode(JSON.stringify(request)).length);
        limits(request, physical, inputTokens, outputTokens);
        const receipt = { requestId: inputs.requestId, family: inputs.family, selected: selection, dispatched: physical.model, original: exact, context: inputs, request, inputTokens, outputTokens, configuration: config, routerState: choice.state, status: 'prepared', usage: null };
        await save({ ...state, configuration: config, history: [...state.history, { requestId: inputs.requestId, patches: state.pending }], pending: [], routerState: choice.state, requests: [...state.requests, receipt] });
        return frozen(receipt);
      });
    },
    dispatch(requestId, send) {
      return serial(async () => {
        const receipt = state.requests.find(item => item.requestId === requestId);
        if (!receipt || receipt.status !== 'prepared') throw new Error('request is already dispatched or unavailable; reconcile its receipt before retry');
        if (typeof send !== 'function') throw new TypeError('dispatch transport is required');
        if (options.authorize) await options.authorize(frozen(receipt));
        await save({ ...state, requests: state.requests.map(item => item.requestId === requestId ? { ...item, status: 'dispatched' } : item) });
        // Authentication belongs only to the transport closure, never the checkpoint.
        return send(frozen(receipt.request), frozen(receipt));
      });
    },
    observe(requestId, usage, status = 'completed') {
      const exact = usage === null ? null : copy(usage);
      return serial(async () => {
        const saved = state.requests.find(item => item.requestId === requestId);
        if (!saved || saved.status === 'prepared') throw new Error('usage requires a dispatched request');
        if (!['completed', 'failed'].includes(status)) throw new TypeError('invalid response status');
        await save({ ...state, requests: state.requests.map(item => item.requestId === requestId ? { ...item, usage: exact, status } : item) });
      });
    },
    async fork({ durability, durabilityId }) {
      const source = copy(await api.snapshot());
      const records = [];
      if (durability !== store) {
        let cursor = source.historyTip;
        while (cursor) {
          const record = await api.history(cursor);
          records.push({ key: cursor.key, value: JSON.stringify({ ...record, cursor: undefined,
            parent: record.parent ? { ...record.parent, stateId: durabilityId } : null }) });
          cursor = record.parent;
        }
        if (source.historyTip) source.historyTip = { ...source.historyTip, stateId: durabilityId };
      }
      const forked = await create({ ...options, durability, durabilityId });
      await forked.importBranch(source, records);
      return forked;
    },
    importBranch(source, records = []) {
      const exact = copy(source);
      return serial(async () => {
        if (state.requests.length || state.history.length || state.pending.length) throw new Error('fork destination is not empty');
        if (exact.format !== FORMAT || exact.selection !== selection) throw new Error('incompatible fork source');
        await save(exact, records);
      });
    },
    async forkSession(sessionId) {
      const durabilityId = stateId + "/fork/" + sessionId;
      // Ephemeral native forks still get their own durable policy namespace.
      // Portable memory stores require explicit initialization of a new state.
      try { await store.load(durabilityId); } catch (error) {
        if (typeof store.importState !== "function") throw error;
        await store.importState(durabilityId, { revision: "0", payload: null });
      }
      return api.fork({ durability: store, durabilityId });
    },
    warm(requestId, send) {
      return serial(async () => {
        if (!cacheWarm) throw new Error('cache warming is disabled');
        const receipt = state.requests.find(item => item.requestId === requestId);
        if (!receipt || receipt.family !== 'claude') throw new Error('cache warming requires a prepared Claude request');
        if (typeof send !== 'function') throw new TypeError('cache warm transport is required');
        const request = { ...copy(receipt.request), max_tokens: 1, stream: false, tool_choice: { type: 'none' } };
        if (request.thinking && request.thinking.type !== 'disabled') throw new Error('cache warm does not support thinking requests');
        if (options.authorize) await options.authorize(frozen(receipt));
        if (state.warms.some(item => item.requestId === requestId)) throw new Error('cache warm already admitted; reconcile its receipt');
        if (state.reservedWarmUsd + cacheWarm.estimatedWriteUsd > cacheWarm.maxSpendUsd) throw new Error('cache warm spend limit exceeded');
        const system = typeof receipt.request.system === 'string' ? [] : receipt.request.system ?? [];
        const blocks = [...system, ...nativeTools(receipt.request), ...(receipt.request.messages ?? []).flatMap(message => Array.isArray(message.content) ? message.content : [])];
        const ttl = cacheWarm.ttlSeconds === 3600 ? '1h' : '5m';
        if (![receipt.request, ...blocks].some(block => block.cache_control?.type === 'ephemeral' && (block.cache_control.ttl ?? '5m') === ttl)) throw new Error('cache warm requires an existing native cache breakpoint with matching TTL');
        const warm = { requestId, status: 'dispatched', ttlSeconds: cacheWarm.ttlSeconds, estimatedWriteUsd: cacheWarm.estimatedWriteUsd, usage: null, actualUsd: null };
        await save({ ...state, warms: [...state.warms, warm], reservedWarmUsd: state.reservedWarmUsd + cacheWarm.estimatedWriteUsd });
        const response = await send(frozen(request), frozen(warm));
        if (!(response instanceof Response) || !response.ok) throw new Error('cache warm response failed; retained as uncertain');
        const body = await response.json();
        if (!body.usage || typeof body.usage !== 'object') throw new Error('cache warm response omitted actual usage; retained as uncertain');
        const actualUsd = warmCost(body.usage, cacheWarm);
        await save({ ...state, reservedWarmUsd: state.reservedWarmUsd - cacheWarm.estimatedWriteUsd + actualUsd, actualWarmUsd: state.actualWarmUsd + actualUsd, warms: state.warms.map(item => item.requestId === requestId ? { ...item, status: 'completed', usage: copy(body.usage), actualUsd } : item) });
        return frozen(body.usage);
      });
    },
    fetch(fetchImpl, family, boundary) {
      if (!['claude', 'codex'].includes(family)) throw new TypeError('unsupported provider family');
      if (typeof fetchImpl !== 'function') throw new TypeError('fetch transport is required');
      return async (input, init) => {
        const incoming = new Request(input, init);
        if (incoming.method !== 'POST') throw new Error('request policy supports model POST requests only');
        const original = await incoming.json();
        const supplied = boundary ? copy(await boundary(frozen(original), family))
          : options.requestContext ? copy(await options.requestContext(frozen(original), family)) : {};
        // Public Agent hosts supply the native operation/model boundary automatically.
        const requestId = nonempty(supplied.requestId, 'native request boundary requestId');
        const snapshot = await api.snapshot();
        const previous = snapshot.requests.at(-1);
        const isContinuation = continuation(original, family);
        const context = { ...supplied, family, requestId, ...(isContinuation && previous && supplied.continuationOf === undefined ? { continuationOf: previous.requestId } : {}), switchSafe: !isContinuation && options.switchSafe ? await options.switchSafe(frozen(original), family) === true : supplied.switchSafe === true };
        const receipt = await api.prepare(original, context);
        if (cacheWarm && family === 'claude' && !isContinuation) {
          const warmSnapshot = await api.snapshot();
          if (!warmSnapshot.warms.some(item => item.requestId === requestId)) {
            await api.warm(requestId, request => fetchImpl(new Request(incoming.url, {
              method: incoming.method, headers: incoming.headers, signal: incoming.signal,
              redirect: incoming.redirect, body: JSON.stringify(request),
            })));
          } else if (warmSnapshot.warms.find(item => item.requestId === requestId).status !== 'completed') {
            throw new Error('cache warm charge is uncertain; reconcile before another dispatch');
          }
        }
        const response = await api.dispatch(receipt.requestId, request => fetchImpl(new Request(incoming.url, { method: incoming.method, headers: incoming.headers, signal: incoming.signal, redirect: incoming.redirect, body: JSON.stringify(request) })));
        if (!(response instanceof Response)) throw new TypeError('request policy transport must return Response');
        if (!response.ok) { await api.observe(requestId, null, 'failed'); return response; }
        return observeResponse(response, family, (usage, status) => api.observe(requestId, usage, status));
      };
    },
  });
  handles.add(api);
  return api;
}
export function assertRequestPolicy(handle) {
  if (!handles.has(handle)) throw new TypeError('requestPolicy must be created by RequestPolicy.create');
  return handle;
}

function observeResponse(response, family, observe) {
  const reader = response.body?.getReader();
  if (!reader) throw new Error('model response omitted body');
  const sse = response.headers.get('content-type')?.includes('text/event-stream');
  const decoder = new TextDecoder();
  let buffer = '', usage = null, terminal = false, settled = false;
  function event(value) {
    if (family === 'claude') {
      if (value.type === 'message_start' && value.message?.usage) usage = { ...value.message.usage };
      if (value.type === 'message_delta' && value.usage) usage = { ...usage, ...value.usage };
      if (value.type === 'message_stop') terminal = true;
    } else if (['response.completed', 'response.failed', 'response.incomplete'].includes(value.type)) {
      usage = value.response?.usage ?? null;
      terminal = value.type === 'response.completed';
    }
  }
  function consume(text, done) {
    buffer += text;
    if (buffer.length > 4 * 1024 * 1024) throw new Error('usage observation frame exceeds 4 MiB');
    if (!sse) { if (done) { const body = JSON.parse(buffer); usage = body.usage ?? null; terminal = family === 'claude' || body.status === 'completed'; } return; }
    const lines = buffer.split(/\r?\n/);
    buffer = done ? '' : lines.pop();
    for (const line of lines) if (line.startsWith('data:') && line.slice(5).trim() !== '[DONE]') { const data = line.slice(5).trim(); if (data) event(JSON.parse(data)); }
  }
  const observed = new Response(new ReadableStream({
    async pull(controller) {
      try {
        const { done, value } = await reader.read();
        consume(decoder.decode(value, { stream: !done }), done);
        if ((terminal || done) && !settled) { await observe(usage, terminal ? 'completed' : 'failed'); settled = true; }
        if (done) { controller.close(); }
        else controller.enqueue(value);
      } catch (error) { controller.error(error); await reader.cancel().catch(() => {}); if (!settled) await observe(usage, 'failed').catch(() => {}); }
    },
    async cancel(reason) { await reader.cancel(reason); if (!settled) { await observe(usage, terminal ? 'completed' : 'failed'); settled = true; } },
  }), { status: response.status, statusText: response.statusText, headers: response.headers });
  // reqwest WASM reads the fetch response URL when constructing its response.
  Object.defineProperty(observed, 'url', { value: response.url });
  return observed;
}

function warmCost(usage, prices) {
  const counts = ['input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'].map(key => usage[key] ?? 0);
  if (counts.some(value => !Number.isSafeInteger(value) || value < 0)) throw new Error('cache warm usage is invalid; retained as uncertain');
  return counts.reduce((sum, count, index) => sum + count * [prices.inputUsdPerMillion, prices.outputUsdPerMillion, prices.cacheWriteUsdPerMillion, prices.cacheReadUsdPerMillion][index] / 1_000_000, 0);
}
