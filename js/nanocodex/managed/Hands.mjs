import { normalizeObservationSurfaces, normalizeObservationResult, observationId, OBSERVATION_INTERVAL_MS } from "nanocodex-tools/observation";

function surfaceMetadata(surface) {
  observationId(surface?.id);
  if (!["agent", "account"].includes(surface.source) || typeof surface.route_token !== "string"
    || !surface.route_token || surface.route_token.length > 256) throw new TypeError("invalid managed hand surface");
  return surface;
}
export function managedHands(client, path) {
  return Object.freeze({
    async list({ signal } = {}) {
      const body = await client.json(`${path}/hands`, { signal });
      if (!Array.isArray(body?.surfaces)) throw new TypeError("invalid managed hand surfaces");
      return Object.freeze(body.surfaces.map((surface) => {
        surfaceMetadata(surface);
        const [description] = normalizeObservationSurfaces([{ id: surface.id, name: surface.name, kind: surface.kind }]);
        if (typeof surface.machine_id !== "string" || typeof surface.machine_name !== "string") throw new TypeError("invalid hand machine");
        return Object.freeze({ ...description, source: surface.source, route_token: surface.route_token,
          machine_id: surface.machine_id, machine_name: surface.machine_name });
      }));
    },
    async *frames(surface, { signal, intervalMs = OBSERVATION_INTERVAL_MS } = {}) {
      surfaceMetadata(surface);
      if (!Number.isInteger(intervalMs) || intervalMs < OBSERVATION_INTERVAL_MS || intervalMs > 60_000) throw new TypeError("invalid observation interval");
      const query = new URLSearchParams({ source: surface.source, surface_id: surface.id, route_token: surface.route_token });
      while (!signal?.aborted) {
        const timeout = AbortSignal.timeout(10_000);
        const requestSignal = signal ? AbortSignal.any([signal, timeout]) : timeout;
        const result = normalizeObservationResult(await client.json(`${path}/hands/frame?${query}`, { signal: requestSignal }));
        signal?.throwIfAborted();
        yield result;
        await pause(intervalMs, signal);
      }
    },
  });
}
function pause(ms, signal) {
  if (signal?.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => { clearTimeout(timer); signal?.removeEventListener("abort", done); resolve(); };
    const timer = setTimeout(done, ms);
    signal?.addEventListener("abort", done, { once: true });
  });
}
