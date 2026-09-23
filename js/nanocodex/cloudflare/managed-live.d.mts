import type { ManagedAgentSettings } from "./agent-settings.mjs";
import type { AdmissionPrincipal } from "./managed-auth.mjs";
export function nativeLiveRequest(request: Request): boolean;
export function liveAgentSettings(request: Request): ManagedAgentSettings | Response;
export function liveAgentFailure(request: Request, principal: AdmissionPrincipal | undefined): Response | undefined;
export function liveAgentRequest(request: Request, principal: AdmissionPrincipal, settings: ManagedAgentSettings, agentId: string, clientIngressColo: string | null): Request;
export function newManagedAgentId(): string;
