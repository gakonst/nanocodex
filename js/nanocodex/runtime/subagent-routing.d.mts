import type { Model, Thinking } from '../types.mjs';
export type ChildRouteModel = Model | 'sol' | 'luna' | 'astra' | 'glm-5.3' | 'glm53' | 'kimi' | 'mimo' | 'opus' | 'sonnet' | 'haiku' | 'fable' | 'claude-opus-5-5' | 'claude-sonnet-5-5' | 'claude-haiku-5-5' | 'claude-fable-5-1' | 'claude-opus-4-6' | 'claude-sonnet-4-6' | 'claude-haiku-4-5';
export type ChildRoute = Readonly<{
  harness?: 'codex' | 'claude';
  provider: string;
  model: ChildRouteModel;
  thinking: Thinking;
  providerModel?: string;
}>;
export type ChildRouteRequest = Readonly<{
  parentSessionId: string;
  harness?: 'codex' | 'claude';
  role: string;
  task: string;
  model?: ChildRoute['model'];
  thinking?: Thinking;
  hostContextRef?: string;
}>;
export type ChildRouteBinding = Readonly<{
  parentSessionId: string;
  sessionId: string;
  routeId: string;
  hostContextRef?: string;
}>;
export type SubagentRouting = Readonly<{
  resolve(request: ChildRouteRequest): Promise<Readonly<{ harness?: 'codex' | 'claude'; model: ChildRoute['model']; thinking: Thinking; routeId: string; statelessHttp?: boolean }>>;
  bind(request: ChildRouteBinding): void;
  route(sessionId: string): ChildRoute;
}>;
/**
 * Host-owned routing lifecycle. Authorize must restrict the resolver to eligible
 * providers and credentials. Public model aliases are normalized before resolution.
 * Load and save must be synchronous; save must pin the live route before returning.
 */
export function createSubagentRouting<Authority>(options: {
  authorize(parentSessionId: string, hostContextRef?: string): Authority | Promise<Authority>;
  resolve(request: ChildRouteRequest, authority: Authority): ChildRoute | Promise<ChildRoute>;
  load(sessionId: string): ChildRoute | undefined | null;
  save(sessionId: string, route: ChildRoute): void;
}): SubagentRouting;
