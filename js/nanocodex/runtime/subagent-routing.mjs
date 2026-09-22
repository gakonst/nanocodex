// Host-only routing lifecycle. Persist public choices, never transports or credentials.
export function createSubagentRouting({ resolve, authorize, load, save }) {
  for (const [name, callback] of Object.entries({ resolve, authorize, load, save })) {
    if (typeof callback !== "function") throw new TypeError(`subagent routing ${name} must be a function`);
  }
  const pending = new Map();
  return Object.freeze({
    async resolve(request) {
      const authority = await authorize(request.parentSessionId, request.hostContextRef);
      const choice = await resolve(Object.freeze({ ...request }), authority);
      if (!choice || typeof choice.model !== "string" || typeof choice.provider !== "string"
        || typeof choice.thinking !== "string") throw new TypeError("subagent router returned an invalid choice");
      if (request.model !== undefined && request.model !== choice.model) {
        throw new Error("subagent router cannot replace an explicit model override");
      }
      if (request.thinking !== undefined && request.thinking !== choice.thinking) {
        throw new Error("subagent router cannot replace an explicit thinking override");
      }
      // Allowlisted public fields only. Resolution may use credentials, but they
      // must never enter a descriptor, durable route, or the Rust bridge.
      const route = Object.freeze({ provider: choice.provider, model: choice.model,
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
      const existing = load(request.sessionId);
      if (existing !== undefined && existing !== null) {
        throw new Error("subagent route is already pinned");
      }
      // save must be synchronous and durable; a failure keeps the choice available
      // for retry and prevents the Rust child from starting.
      const saved = save(request.sessionId, prepared.route);
      if (saved && typeof saved.then === "function") throw new TypeError("subagent route save must be synchronous");
      pending.delete(request.routeId);
    },
    route(sessionId) {
      const retained = load(sessionId);
      if (!retained) throw new Error("subagent route is missing; refusing to inherit parent transport");
      return retained;
    },
  });
}
