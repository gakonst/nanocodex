export const MAX_OBSERVATION_IMAGE_BYTES = 180_000;
export const OBSERVATION_TIMEOUT_MS = 5_000;
export const OBSERVATION_INTERVAL_MS = 250;

export function observationId(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) {
    throw new TypeError("invalid observation identifier");
  }
  return value;
}
function keys(value, allowed) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).some((key) => !allowed.includes(key))) throw new TypeError("invalid observation fields");
}
export function normalizeObservationSurfaces(value) {
  if (!Array.isArray(value) || value.length < 1 || value.length > 8) throw new TypeError("expected 1-8 observation surfaces");
  const ids = new Set();
  return Object.freeze(value.map((surface) => {
    keys(surface, ["id", "name", "kind"]);
    const id = observationId(surface.id);
    if (ids.has(id) || typeof surface.name !== "string" || !surface.name.trim()
      || new TextEncoder().encode(surface.name).length > 128
      || !["desktop", "browser", "phone"].includes(surface.kind)) throw new TypeError("invalid observation surface");
    ids.add(id);
    return Object.freeze({ id, name: surface.name, kind: surface.kind });
  }));
}
export function normalizeObservationProvider(provider, machines) {
  if (provider === undefined) return undefined;
  if (machines.length !== 1 || typeof provider?.capture !== "function") throw new TypeError("observation requires one machine and a capture function");
  return Object.freeze({ surfaces: normalizeObservationSurfaces(provider.surfaces), capture: provider.capture.bind(provider) });
}
export function normalizeObservationFrame(frame) {
  keys(frame, ["captured_at", "width", "height", "mime_type", "data"]);
  if (!Number.isSafeInteger(frame.captured_at) || frame.captured_at < 0
    || ![frame.width, frame.height].every((n) => Number.isInteger(n) && n >= 1 && n <= 8192)
    || !["image/jpeg", "image/png"].includes(frame.mime_type)
    || typeof frame.data !== "string" || frame.data.length === 0
    || frame.data.length > MAX_OBSERVATION_IMAGE_BYTES * 4 / 3
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(frame.data)) {
    throw new TypeError("invalid observation frame");
  }
  return Object.freeze({ ...frame });
}
export function normalizeObservationResult(result) {
  if (result?.status === "frame") {
    keys(result, ["status", "frame"]);
    return Object.freeze({ status: "frame", frame: normalizeObservationFrame(result.frame) });
  }
  keys(result, ["status", "message"]);
  if (result.status !== "unavailable" || typeof result.message !== "string" || result.message.length > 256) throw new TypeError("invalid observation result");
  return Object.freeze({ status: "unavailable", message: result.message });
}
