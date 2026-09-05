import {
  assertCookieJarFence,
  validateCookieJar,
  type BrowserCookieJarFence,
  type BrowserCookieJarV1,
  type CookieJarMetadata,
  type CookieSyncTransport,
} from "./cookie-sync.ts";

const COOKIE_JAR_ID = /^[A-Za-z0-9_-]{22,64}$/;
const COOKIE_JARS_PATH = "/v1/browser-cookie-jars";

export function createAuthenticatedCookieSyncTransport(
  authenticatedFetch: (input: string, init?: RequestInit) => Promise<Response>,
): CookieSyncTransport {
  return Object.freeze({
    async list(fence: BrowserCookieJarFence): Promise<readonly CookieJarMetadata[]> {
      const query = new URLSearchParams({
        origin: fence.origin,
        profile_id: fence.profile_id,
        store_id: fence.store_id,
      });
      const value = await cookieSyncJson(authenticatedFetch, `${COOKIE_JARS_PATH}?${query}`, {
        method: "GET",
      });
      const records = asObject(value).browser_cookie_jars;
      if (!Array.isArray(records)) throw new Error("Cookie sync returned invalid metadata.");
      return records.map(validateCookieJarMetadata);
    },
    async replace(jarId: string, jar: BrowserCookieJarV1): Promise<CookieJarMetadata> {
      const id = validCookieJarId(jarId);
      const value = await cookieSyncJson(authenticatedFetch, `${COOKIE_JARS_PATH}/${id}`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(jar),
      });
      const metadata = validateCookieJarMetadata(value);
      if (metadata.id !== id) throw new Error("Cookie sync returned another jar identity.");
      assertCookieJarFence(jar, metadata);
      return metadata;
    },
    async materialize(jarId: string, fence: BrowserCookieJarFence): Promise<BrowserCookieJarV1> {
      const id = validCookieJarId(jarId);
      const value = await cookieSyncJson(authenticatedFetch, `${COOKIE_JARS_PATH}/${id}/materialize`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(fence),
      });
      const record = asObject(value);
      if (record.id !== id) throw new Error("Cookie sync returned another jar identity.");
      const jar = validateCookieJar({
        schema_version: record.schema_version,
        origin: record.origin,
        profile_id: record.profile_id,
        store_id: record.store_id,
        revision: record.revision,
        cookies: record.cookies,
      });
      assertCookieJarFence(jar, fence);
      return jar;
    },
    async delete(jarId: string, fence: BrowserCookieJarFence, revision: number): Promise<void> {
      const id = validCookieJarId(jarId);
      await cookieSyncJson(authenticatedFetch, `${COOKIE_JARS_PATH}/${id}`, {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ ...fence, revision }),
      });
    },
  });
}

async function cookieSyncJson(
  authenticatedFetch: (input: string, init?: RequestInit) => Promise<Response>,
  path: string,
  init: RequestInit,
): Promise<unknown> {
  const headers = new Headers(init.headers);
  headers.set("accept", "application/json");
  const response = await authenticatedFetch(path, {
    ...init,
    cache: "no-store",
    credentials: "omit",
    redirect: "error",
    headers,
  });
  if (!response.ok) throw new Error(`Cookie sync request failed (${response.status}).`);
  if (response.status === 204) return {};
  return response.json();
}

function validateCookieJarMetadata(value: unknown): CookieJarMetadata {
  const metadata = asObject(value);
  const id = validCookieJarId(String(metadata.id ?? ""));
  if (typeof metadata.origin !== "string" || typeof metadata.profile_id !== "string"
    || typeof metadata.store_id !== "string" || !Number.isSafeInteger(metadata.revision)
    || !Number.isSafeInteger(metadata.cookie_count)) {
    throw new Error("Cookie sync returned invalid metadata.");
  }
  return {
    id,
    origin: metadata.origin,
    profile_id: metadata.profile_id,
    store_id: metadata.store_id,
    revision: metadata.revision as number,
    cookie_count: metadata.cookie_count as number,
    ...(Number.isSafeInteger(metadata.updated_at) ? { updated_at: metadata.updated_at as number } : {}),
  };
}

function validCookieJarId(value: string): string {
  if (!COOKIE_JAR_ID.test(value)) throw new Error("Cookie jar identity is invalid.");
  return value;
}

function asObject(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
}
