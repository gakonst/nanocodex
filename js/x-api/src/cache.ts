// Public provider results only. Never store user credentials or failed lookups.
export type CacheStatus = "hit" | "miss" | "bypass";

export function buildCacheKey(parts: Record<string, string | number>): string {
  return new URLSearchParams(Object.entries(parts).sort(([a], [b]) => a.localeCompare(b))
    .map(([key, value]) => [key, String(value)])).toString();
}

export async function withCache<T>(key: string, nocache: boolean, fetchValue: () => Promise<T>):
  Promise<{ value: T; status: CacheStatus }> {
  if (nocache) return { value: await fetchValue(), status: "bypass" };
  const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(key));
  const digest = [...new Uint8Array(hash)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  const request = new Request(`https://nanocodex-x.cache/v1/${digest}`);
  try {
    const cached = await caches.default.match(request);
    if (cached) return { value: await cached.json<T>(), status: "hit" };
  } catch { /* Cache availability must not determine provider availability. */ }
  const value = await fetchValue();
  await caches.default.put(request, Response.json(value, {
    headers: { "cache-control": "public, max-age=3600" },
  })).catch(() => {});
  return { value, status: "miss" };
}

export function cacheControlHeader(): string {
  return "no-store";
}
