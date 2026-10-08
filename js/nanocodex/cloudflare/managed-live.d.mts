import type { ManagedAgentSettings } from "./agent-settings.mjs";
import type { AdmissionPrincipal } from "./managed-auth.mjs";
export function nativeLiveRequest(request: Request): boolean;
export function liveAgentSettings(request: Request): ManagedAgentSettings | Response;
export function liveAgentFailure(request: Request, principal: AdmissionPrincipal | undefined): Response | undefined;
export function liveAgentRequest(request: Request, principal: AdmissionPrincipal, settings: ManagedAgentSettings, agentId: string, clientIngressColo: string | null): Request;
export function newManagedAgentId(): string;

export function nativeRunRequest(request: Request): boolean;
export type NativeRunBody = { key: string; settings: ManagedAgentSettings; input: string };
export function nativeRunBody(request: Request): Promise<NativeRunBody | undefined>;
export function idempotentAgentId(userId: string, requestKey: string): Promise<string>;
export function runAgentRequest(request: Request, principal: AdmissionPrincipal, run: NativeRunBody, clientIngressColo: string | null): Promise<{ request: Request; agentId: string; turnId: string }>;
export function ingressColo(value: unknown): string | null;
