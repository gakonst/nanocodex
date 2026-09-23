/** Trusted metadata retained from the initial user ingress, never a public header. */
export interface IngressPlacement { trustedClientIngressColo?: string | null }
export type PlacementRegion = "wnam" | "enam" | "sam" | "weur" | "eeur" | "apac" | "oc";
export const TRUSTED_INGRESS_HEADER: "x-nanocodex-placement-colo";
export function ingressColo(value: unknown): string | null;
export function placementRegion(value: unknown): PlacementRegion | undefined;
/** Best-effort first-use hint. Does not rename or move an existing object. */
export function durablePlacementOptions(colo: unknown): { locationHint: PlacementRegion } | undefined;
export function placementHeaders(headers: HeadersInit | undefined, colo: unknown): Headers;
type PlacementBinding = { fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> };
/** Scope only a request's environment; keep retained DO environments unchanged. */
export function withIngressPlacement<T extends { NANOCODEX?: PlacementBinding }>(env: T, colo: unknown): T & IngressPlacement;
