// Providers contribute passive data, never tool content or image artifacts.
type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type ScreenObservation = {
  schemaVersion: 1; capturedAt: number;
  providers: { id: string; status: "ok" | "partial" | "unavailable" | "error" | "timeout";
    capturedAt: number; ageMs?: number; freshness: "fresh" | "stale" | "unknown";
    scope?: "requested_context" | "active_window" | "none"; foreground_verified?: boolean;
    error?: string; data?: { [key: string]: Json } }[];
};
const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const integer = (value: unknown): value is number => Number.isSafeInteger(value) && Number(value) >= 0;
const bounded = (value: unknown, max: number): value is string => typeof value === "string" && new TextEncoder().encode(value).length <= max;
const mediaKeys = new Set(["__proto__", "constructor", "prototype", "image_url", "imageUrl", "audio_url", "video_url", "file_url", "file_data", "mimeType", "mime_type", "mime", "blob"]);
function dataObject(value: unknown): { data: { [key: string]: Json }; stripped: boolean } | undefined {
  if (!record(value)) return;
  let nodes = 0;
  let stripped = false;
  function copy(value: unknown, depth: number): Json {
    if (++nodes > 2048 || depth > 8) throw new Error("Observation data budget");
    if (value === null || typeof value === "boolean") return value;
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (bounded(value, 512)) return value;
    if (Array.isArray(value)) return value.map(item => copy(item, depth + 1));
    if (!record(value)) throw new Error("Invalid observation data");
    const output: { [key: string]: Json } = {};
    for (const [key, item] of Object.entries(value)) {
      // Type tags can promote provider state into media when passed through generic renderers.
      if (mediaKeys.has(key) || (key === "type" && typeof item === "string"
        && /^(?:(?:input_|output_)?(?:image|audio|video|file)|image_url|resource|resource_link)$/.test(item))) { stripped = true; continue; }
      if (!bounded(key, 512)) throw new Error("Observation key budget");
      output[key] = copy(item, depth + 1);
    }
    return output;
  }
  try {
    const result = copy(value, 0) as { [key: string]: Json };
    return bounded(JSON.stringify(result), 8192) ? { data: result, stripped } : undefined;
  } catch { return; }
}
/** Allowlist the versioned wire contract. Bad optional context must not lose a screenshot. */
export function screenObservation(value: unknown): ScreenObservation | undefined {
  if (!record(value) || value.schemaVersion !== 1 || !integer(value.capturedAt) || !Array.isArray(value.providers)) return;
  const providers: ScreenObservation["providers"] = [];
  for (const provider of value.providers.slice(0, 5)) {
    if (!record(provider) || !bounded(provider.id, 128) || !provider.id || !integer(provider.capturedAt)
      || !["ok", "partial", "unavailable", "error", "timeout"].includes(String(provider.status))
      || !["fresh", "stale", "unknown"].includes(String(provider.freshness))) continue;
    const data = dataObject(provider.data);
    const successful = provider.status === "ok" || provider.status === "partial";
    const invalidData = !data && (Object.hasOwn(provider, "data") || successful);
    const status = invalidData ? "error" : data?.stripped && successful ? "partial" : provider.status;
    providers.push({ id: provider.id, status: status as ScreenObservation["providers"][number]["status"],
      capturedAt: provider.capturedAt, freshness: provider.freshness as ScreenObservation["providers"][number]["freshness"],
      ...(["requested_context", "active_window", "none"].includes(String(provider.scope)) ? { scope: provider.scope as "requested_context" | "active_window" | "none" } : {}),
      ...(typeof provider.foreground_verified === "boolean" ? { foreground_verified: provider.foreground_verified } : {}),
      ...(integer(provider.ageMs) ? { ageMs: provider.ageMs } : {}),
      ...(invalidData ? { error: "invalid_provider_data" } : bounded(provider.error, 512) ? { error: provider.error } : {}),
      ...(data ? { data: data.data } : {}) });
  }
  return { schemaVersion: 1, capturedAt: value.capturedAt, providers };
}
