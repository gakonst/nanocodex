import { ConvertError } from "./errors.js";

const MAX_PROVIDER_BYTES = 2 * 1024 * 1024;

export async function providerJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  signal?.throwIfAborted();
  let response: Response;
  try {
    response = await fetch(url, {
      headers: { accept: "application/json", "user-agent": "nanocodex-x/1.0" },
      redirect: "manual",
      signal,
    });
  } catch (error) {
    signal?.throwIfAborted();
    console.warn("X provider fetch failed", {
      provider: new URL(url).hostname,
      message: error instanceof Error ? error.message : "unknown fetch failure",
    });
    throw new ConvertError(502, "X data provider could not be reached.", "provider_unavailable");
  }
  if (!response.ok) {
    await response.body?.cancel();
    const retry = response.headers.get("retry-after");
    const seconds = retry && /^\d+$/.test(retry) ? Number(retry)
      : retry ? Math.ceil((Date.parse(retry) - Date.now()) / 1000) : NaN;
    const retryAfter = Number.isFinite(seconds) ? Math.max(1, Math.min(seconds, 86400)) : 30;
    if (response.status === 429 || response.status === 503) {
      throw new ConvertError(response.status, "X data provider is temporarily unavailable.",
        response.status === 429 ? "rate_limited" : "provider_unavailable", retryAfter);
    }
    throw new ConvertError(response.status === 404 ? 404 : 502,
      "X resource is unavailable upstream.", response.status === 404 ? "not_found" : "provider_error");
  }
  const reader = response.body?.getReader();
  if (!reader) throw new ConvertError(502, "Empty X provider response.", "invalid_response");
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      signal?.throwIfAborted();
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_PROVIDER_BYTES) {
        throw new ConvertError(502, "X provider response is too large.", "response_too_large");
      }
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    const data: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error("invalid JSON object");
    return data as T;
  } catch (error) {
    await reader.cancel().catch(() => {});
    signal?.throwIfAborted();
    if (error instanceof ConvertError) throw error;
    throw new ConvertError(502, "Invalid X provider response.", "invalid_response");
  } finally {
    reader.releaseLock();
  }
}
