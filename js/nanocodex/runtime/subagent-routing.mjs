const modelAliases = new Map([
  ['sol', 'sol'], ['gpt-5.6-sol', 'sol'],
  ['terra', 'terra'], ['gpt-5.6-terra', 'terra'],
  ['luna', 'luna'], ['gpt-5.6-luna', 'luna'],
  ['astra', 'astra'], ['gpt-6-astra', 'astra'],
  ['glm-5.3', 'glm-5.3'], ['glm53', 'glm-5.3'], ['@cf/zai-org/glm-5.3', 'glm-5.3'],
]);
const thinkingLevels = new Set(['none', 'low', 'medium', 'high', 'xhigh', 'max']);
function canonicalModel(model) {
  const canonical = modelAliases.get(model);
  if (!canonical) throw new TypeError('subagent router returned an invalid model');
  return canonical;
}
function synchronous(value, operation) {
  if (value && typeof value.then === 'function') {
    // Observe a rejected async implementation without allowing the child to start.
    Promise.resolve(value).catch(() => {});
    throw new TypeError(`subagent route ${operation} must be synchronous`);
  }
  return value;
}

// Host-only routing lifecycle. Retain live public choices, never transports or credentials.
export function createSubagentRouting({ resolve, authorize, load, save }) {
  for (const [name, callback] of Object.entries({ resolve, authorize, load, save })) {
    if (typeof callback !== "function") throw new TypeError(`subagent routing ${name} must be a function`);
  }
  const pending = new Map();
  return Object.freeze({
    async resolve(request) {
      // Capture before the first await: authorization and routing must describe
      // the same immutable request even if the caller reuses its object.
      request = Object.freeze({ ...request,
        ...(request.model === undefined ? {} : { model: canonicalModel(request.model) }) });
      const authority = await authorize(request.parentSessionId, request.hostContextRef);
      const choice = await resolve(request, authority);
      if (!choice || typeof choice.model !== "string" || typeof choice.provider !== "string"
        || typeof choice.thinking !== "string") throw new TypeError("subagent router returned an invalid choice");
      const model = canonicalModel(choice.model);
      if (!choice.provider.trim() || !thinkingLevels.has(choice.thinking)
        || (choice.providerModel !== undefined && (typeof choice.providerModel !== 'string' || !choice.providerModel.trim()))) {
        throw new TypeError('subagent router returned an invalid choice');
      }
      if (request.model !== undefined && request.model !== model) {
        throw new Error("subagent router cannot replace an explicit model override");
      }
      if (request.thinking !== undefined && request.thinking !== choice.thinking) {
        throw new Error("subagent router cannot replace an explicit thinking override");
      }
      // Allowlisted public fields only. Resolution may use credentials, but they
      // must never enter a descriptor, live route, or the Rust bridge.
      const route = Object.freeze({ provider: choice.provider, model,
        thinking: choice.thinking, ...(choice.providerModel === undefined ? {} : { providerModel: choice.providerModel }) });
      const routeId = crypto.randomUUID();
      pending.set(routeId, { route, parentSessionId: request.parentSessionId,
        hostContextRef: request.hostContextRef });
      return { model: route.model, thinking: route.thinking, routeId };
    },
    bind(request) {
      const prepared = pending.get(request.routeId);
      if (!prepared || prepared.parentSessionId !== request.parentSessionId
        || prepared.hostContextRef !== request.hostContextRef) {
        throw new Error("subagent route is not owned by this parent authorization");
      }
      const existing = synchronous(load(request.sessionId), 'load');
      if (existing !== undefined && existing !== null) {
        throw new Error("subagent route is already pinned");
      }
      // save must synchronously pin the live route; a failure keeps the choice available
      // for retry and prevents the Rust child from starting.
      synchronous(save(request.sessionId, prepared.route), 'save');
      pending.delete(request.routeId);
    },
    route(sessionId) {
      const retained = synchronous(load(sessionId), 'load');
      if (!retained) throw new Error("subagent route is missing; refusing to inherit parent transport");
      return retained;
    },
  });
}
