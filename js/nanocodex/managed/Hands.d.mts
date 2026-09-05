import type { ObservationSurface, ObservationFrame, ObservationResult } from "nanocodex-tools/observation";
export type ManagedHandFrame = ObservationFrame;
export type ManagedHandSurface = ObservationSurface & Readonly<{
  source: "agent" | "account"; machine_id: string; machine_name: string; route_token: string;
}>;
export type ManagedHands = Readonly<{
  list(options?: { signal?: AbortSignal }): Promise<readonly ManagedHandSurface[]>;
  frames(surface: ManagedHandSurface, options?: { signal?: AbortSignal; intervalMs?: number }): AsyncGenerator<ObservationResult>;
}>;
export function managedHands(client: { json(path: string, options?: { signal?: AbortSignal }): Promise<unknown> }, path: string): ManagedHands;
