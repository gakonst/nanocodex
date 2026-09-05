import type { ObservationProvider } from "./observation.mjs";
export function createScreenObservation(options?: { source?: "desktop"; name?: string } | { source: "android"; device: string; name?: string }): ObservationProvider;
