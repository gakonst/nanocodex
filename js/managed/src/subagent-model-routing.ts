import { z } from "zod";
import {
  resolveThreadRoute, ROUTING_CANDIDATES, routingPolicySchema,
  type RoutingAi, type ThreadRoute, type ThreadRoutingPolicy, type RoutingAvailability,
} from "./thread-model-routing";

const requestSchema = z.object({
  parentSessionId: z.string().min(1).max(256),
  role: z.string().max(4096),
  task: z.string().max(65536),
  model: z.string().optional(),
  thinking: z.enum(["low", "medium", "high"]).optional(),
  hostContextRef: z.string().min(1).max(256),
}).strict();
const bindingSchema = z.object({
  parentSessionId: z.string().min(1).max(256),
  sessionId: z.string().min(1).max(256),
  routeId: z.string().min(1).max(256),
  hostContextRef: z.string().min(1).max(256),
}).strict();
const aliases = new Map([
  ["sol", "gpt-5.6-sol"], ["terra", "gpt-5.6-terra"], ["luna", "gpt-5.6-luna"],
  ["astra", "gpt-6-astra"], ["glm-5.3", "@cf/zai-org/glm-5.3"],
]);
export type RetainedChildRoute = {
  routeId: string;
  parentSessionId: string;
  hostContextRef: string;
  route: ThreadRoute;
};
export interface ChildRouteStore {
  read(sessionId: string): RetainedChildRoute | undefined;
  commit(sessionId: string, value: RetainedChildRoute): void;
}

// A maximum-sized batch may spend 64 × 10 seconds classifying before binding.
// Expiry bounds abandoned tickets without invalidating a normal bounded batch.
export const CHILD_ROUTE_TICKET_TTL_MS = 15 * 60_000;

/** One decision per newly spawned child, committed before its first inference. */
export function createSubagentRouteController(options: {
  ai: RoutingAi;
  policy: ThreadRoutingPolicy;
  availability: () => RoutingAvailability | Promise<RoutingAvailability>;
  store: ChildRouteStore;
  authorize: (parentSessionId: string, hostContextRef: string) => void;
  id?: () => string;
  /** Monotonic milliseconds; injected for deterministic expiry tests. */
  now?: () => number;
}) {
  const pending = new Map<string, { binding: RetainedChildRoute; expiresAt: number }>();
  const now = options.now ?? (() => performance.now());
  const expirePending = () => {
    const current = now();
    for (const [id, ticket] of pending) {
      if (ticket.expiresAt <= current) pending.delete(id);
    }
  };
  let resolving = 0;
  return {
    async resolve(raw: unknown) {
      const request = requestSchema.parse(raw);
      options.authorize(request.parentSessionId, request.hostContextRef);
      expirePending();
      if (pending.size + resolving >= 64) throw new Error("Too many pending child routes");
      const model = request.model === undefined ? undefined : aliases.get(request.model) ?? request.model;
      const candidates = ROUTING_CANDIDATES.filter(candidate => (
        (!options.policy.candidates || options.policy.candidates.includes(candidate.id))
        && (model === undefined || candidate.model === model)
        && (request.thinking === undefined || candidate.thinking === request.thinking)
      )).map(candidate => candidate.id);
      if (!candidates.length) throw new Error("Explicit child model/effort is outside eligible routing policy");
      resolving++;
      try {
        const route = await resolveThreadRoute(options.ai, JSON.stringify({ role: request.role, task: request.task }),
          routingPolicySchema.parse({ ...options.policy, strategy: "direct", candidates }), await options.availability());
        // Authority can change while the classifier is in flight. Bind checks it again.
        options.authorize(request.parentSessionId, request.hostContextRef);
        const routeId = options.id ? options.id() : crypto.randomUUID();
        if (!routeId || pending.has(routeId)) throw new Error("Child route reference is not unique");
        pending.set(routeId, { expiresAt: now() + CHILD_ROUTE_TICKET_TTL_MS,
          binding: { routeId, parentSessionId: request.parentSessionId,
            hostContextRef: request.hostContextRef, route } });
        return { model: route.model, thinking: route.thinking, routeId };
      } finally { resolving--; }
    },
    bind(raw: unknown) {
      const request = bindingSchema.parse(raw);
      options.authorize(request.parentSessionId, request.hostContextRef);
      if (request.sessionId === request.parentSessionId) throw new Error("Child cannot replace parent route");
      expirePending();
      const retained = options.store.read(request.sessionId);
      if (retained) {
        if (retained.routeId !== request.routeId || retained.parentSessionId !== request.parentSessionId
          || retained.hostContextRef !== request.hostContextRef) throw new Error("Child route conflicts with retained binding");
        return;
      }
      const proposed = pending.get(request.routeId)?.binding;
      if (!proposed || proposed.parentSessionId !== request.parentSessionId
        || proposed.hostContextRef !== request.hostContextRef) throw new Error("Unknown or unauthorized child route reference");
      options.store.commit(request.sessionId, proposed);
      pending.delete(request.routeId);
    },
    routeForSession(sessionId: string) {
      const retained = options.store.read(sessionId);
      if (!retained) throw new Error("Child route is missing; refusing parent transport");
      return retained.route;
    },
  };
}
