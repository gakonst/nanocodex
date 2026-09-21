import { CredentialVault, type CredentialVaultEnv, type EncryptedEnvelope } from "./credentialVault.ts";

const BASE = "/api/voice/elevenlabs";
const API = "https://api.elevenlabs.io";
const ID = /^[A-Za-z0-9_-]{1,128}$/;
const MAX_UPLOAD = 20 * 1024 * 1024;
const HEADERS = { "cache-control": "no-store", "x-content-type-options": "nosniff" };
type Fetch = (request: Request) => Promise<Response>;
export type ElevenLabsEnv = CredentialVaultEnv & {
  NANOCODEX_BACKEND?: { fetch: Fetch };
  ELEVENLABS_ACCOUNTS?: DurableObjectNamespace;
  ELEVENLABS_API_KEY?: string;
  ELEVENLABS_ACCOUNT_ID?: string;
};

/** Public boundary: identity comes exclusively from the account service, never request parameters. */
export async function routeElevenLabs(request: Request, env: ElevenLabsEnv, url: URL): Promise<Response | undefined> {
  if (url.pathname !== BASE && url.pathname !== `${BASE}/voices` && url.pathname !== `${BASE}/speech`) return undefined;
  const origin = request.headers.get("origin");
  if ((origin && origin !== url.origin) || request.headers.get("sec-fetch-site") === "cross-site") return json({ error: "forbidden" }, 403);
  if (!env.NANOCODEX_BACKEND || !env.ELEVENLABS_ACCOUNTS) return json({ error: "elevenlabs_unavailable" }, 503);
  try {
    const headers = new Headers({ accept: "application/json" });
    for (const key of ["cookie", "authorization"]) {
      const value = request.headers.get(key);
      if (value) headers.set(key, value);
    }
    const me = await env.NANOCODEX_BACKEND.fetch(new Request(new URL("/v1/me", url), { headers }));
    const account = me.ok ? await me.json() as { authentication?: string; user?: { persistent?: boolean; id?: string } } : null;
    if (!me.ok) await me.body?.cancel();
    if ((account?.authentication !== "account_session" && account?.authentication !== "api_key") || account.user?.persistent !== true || typeof account.user.id !== "string" || !account.user.id) return json({ error: "authentication_required" }, 401);
    const namespace = env.ELEVENLABS_ACCOUNTS;
    const stub = namespace.get(namespace.idFromName(account.user.id));
    const internal = new URL(url.pathname.slice(BASE.length) || "/", "https://elevenlabs.internal");
    internal.search = url.search;
    const forwarded = new Headers();
    for (const key of ["content-type", "content-length"]) {
      const value = request.headers.get(key);
      if (value) forwarded.set(key, value);
    }
    return await stub.fetch(new Request(internal, { method: request.method, headers: forwarded, body: request.body, signal: request.signal, duplex: "half" } as RequestInit));
  } catch { return json({ error: "elevenlabs_unavailable" }, 503); }
}

/** Only the authenticated route above can reach this account-scoped object. */
export class ElevenLabsAccount {
  readonly #storage: DurableObjectStorage;
  readonly #vault: CredentialVault;
  readonly #fetch: Fetch;
  readonly #fallbackKey: string | undefined;
  constructor(state: DurableObjectState, env: ElevenLabsEnv, providerFetch: Fetch = request => fetch(request)) {
    this.#storage = state.storage;
    this.#vault = new CredentialVault(env, `elevenlabs/${state.id.toString()}`);
    this.#fetch = providerFetch;
    // Scope deployment secrets by object identity, never by request headers.
    const key = env.ELEVENLABS_API_KEY?.trim();
    if (env.ELEVENLABS_ACCOUNT_ID && env.ELEVENLABS_ACCOUNTS && key && key.length <= 1024 && !/[^\x21-\x7e]/.test(key)
      && state.id.toString() === env.ELEVENLABS_ACCOUNTS.idFromName(env.ELEVENLABS_ACCOUNT_ID).toString()) {
      this.#fallbackKey = key;
    }
  }

  async fetch(request: Request): Promise<Response> {
    try { return await this.#route(request); }
    catch (error) {
      if (error instanceof InputError) return json({ error: error.message }, error.status);
      return json({ error: "elevenlabs_unavailable" }, 502);
    }
  }

  async #route(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/" && request.method === "GET") {
      return json({ configured: Boolean(await this.#storage.get("credential") || await this.#fallback()) });
    }
    if (url.pathname === "/" && request.method === "DELETE") {
      // Persist the opt-out even if deployment secrets are added or rotated later.
      await this.#storage.put("fallbackDisabled", true);
      await this.#storage.delete("credential");
      return json({ configured: false });
    }
    if (url.pathname === "/" && request.method === "PUT") {
      const body = await readJson(request, 4096);
      const apiKey = typeof body.api_key === "string" ? body.api_key.trim() : "";
      if (!apiKey || apiKey.length > 1024 || /[^\x21-\x7e]/.test(apiKey)) throw new InputError("invalid_api_key");
      // Validate before replacing a working key. Never return or log provider errors.
      const checked = await this.#provider("/v2/voices?page_size=1", apiKey, request);
      if (!checked.ok) return providerError(checked);
      await checked.body?.cancel();
      await this.#storage.put("credential", await this.#vault.seal({ apiKey }));
      // A successful explicit reconnect restores fallback eligibility.
      await this.#storage.delete("fallbackDisabled");
      return json({ configured: true });
    }
    const list = url.pathname === "/voices" && request.method === "GET";
    const clone = url.pathname === "/voices" && request.method === "POST";
    const speech = url.pathname === "/speech" && request.method === "POST";
    if (!list && !clone && !speech) return json({ error: "method_not_allowed" }, 405);
    const envelope = await this.#storage.get<EncryptedEnvelope>("credential");
    let apiKey: string | undefined;
    if (envelope) {
      const opened = await this.#vault.open<{ apiKey: string }>(envelope);
      if (opened.reseal) await this.#storage.put("credential", await this.#vault.seal(opened.value));
      apiKey = opened.value.apiKey;
    } else {
      apiKey = await this.#fallback();
    }
    if (!apiKey) return json({ error: "elevenlabs_not_configured" }, 409);
    if (list) {
      const query = new URLSearchParams({ page_size: "100" });
      const cursor = url.searchParams.get("next_page_token");
      if (cursor) {
        if (cursor.length > 2048) throw new InputError("invalid_page_token");
        query.set("next_page_token", cursor);
      }
      const upstream = await this.#provider(`/v2/voices?${query}`, apiKey, request);
      if (!upstream.ok) return providerError(upstream);
      const body = await readProviderJson(upstream);
      if (!Array.isArray(body.voices)) throw new Error("invalid provider response");
      return json({
        voices: body.voices.filter(record).filter(v => typeof v.voice_id === "string" && ID.test(v.voice_id)).map(v => ({
          voice_id: v.voice_id,
          name: typeof v.name === "string" ? v.name.slice(0, 200) : v.voice_id,
          category: typeof v.category === "string" ? v.category.slice(0, 100) : null,
          preview_url: safePreview(v.preview_url),
        })),
        has_more: body.has_more === true,
        next_page_token: typeof body.next_page_token === "string" ? body.next_page_token : null,
      });
    }
    if (speech) {
      const body = await readJson(request, 48 * 1024);
      if (typeof body.voice_id !== "string" || !ID.test(body.voice_id)) throw new InputError("invalid_voice_id");
      if (typeof body.text !== "string" || !body.text.trim() || body.text.length > 10_000) throw new InputError("text_must_be_1_to_10000_characters");
      const format = body.output_format ?? "mp3_44100_128";
      if (format !== "mp3_44100_128" && format !== "pcm_24000") throw new InputError("invalid_output_format");
      const upstream = await this.#provider(`/v1/text-to-speech/${body.voice_id}/stream?output_format=${format}`, apiKey, request, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ text: body.text, model_id: "eleven_flash_v2_5" }),
      });
      if (!upstream.ok) return providerError(upstream);
      return new Response(upstream.body, { headers: { ...HEADERS, "content-type": format === "pcm_24000" ? "audio/pcm;rate=24000;channels=1" : "audio/mpeg" } });
    }
    if (!request.headers.get("content-type")?.startsWith("multipart/form-data")) throw new InputError("expected_multipart", 415);
    const bytes = await boundedBody(request, MAX_UPLOAD);
    let form: FormData;
    try { form = await new Response(bytes, { headers: { "content-type": request.headers.get("content-type")! } }).formData(); }
    catch { throw new InputError("invalid_multipart"); }
    if (form.get("consent") !== "true") throw new InputError("voice_cloning_consent_required");
    const name = form.get("name");
    if (typeof name !== "string" || !name.trim() || name.length > 100) throw new InputError("invalid_voice_name");
    const files = form.getAll("files");
    if (!files.length || files.length > 5) throw new InputError("provide_1_to_5_audio_files");
    const outgoing = new FormData();
    outgoing.set("name", name.trim());
    const types = new Set(["audio/mpeg", "audio/mp3", "audio/wav", "audio/x-wav", "audio/mp4", "audio/x-m4a", "audio/aac", "audio/ogg", "audio/webm", "audio/flac"]);
    for (const [i, file] of files.entries()) {
      if (typeof file === "string" || !file.size || !types.has(file.type) || file.size > 10 * 1024 * 1024) throw new InputError("invalid_audio_file");
      // Strip client paths and names; files are forwarded transiently, never persisted.
      const ext = file.type === "audio/mp4" || file.type === "audio/x-m4a" ? "m4a" : file.type.replace("audio/", "").replace("x-", "");
      outgoing.append("files", file, `sample-${i + 1}.${ext}`);
    }
    const upstream = await this.#provider("/v1/voices/add", apiKey, request, { method: "POST", body: outgoing });
    if (!upstream.ok) return providerError(upstream);
    const body = await readProviderJson(upstream);
    if (typeof body.voice_id !== "string" || !ID.test(body.voice_id) || typeof body.requires_verification !== "boolean") throw new Error("invalid provider response");
    return json({ voice_id: body.voice_id, requires_verification: body.requires_verification }, 201);
  }

  async #fallback(): Promise<string | undefined> {
    return this.#fallbackKey && !await this.#storage.get("fallbackDisabled") ? this.#fallbackKey : undefined;
  }

  #provider(path: string, apiKey: string, request: Request, init: RequestInit = {}): Promise<Response> {
    const headers = new Headers(init.headers);
    headers.set("xi-api-key", apiKey);
    return this.#fetch(new Request(`${API}${path}`, { ...init, headers, redirect: "manual", signal: request.signal }));
  }
}

class InputError extends Error {
  status: number;
  constructor(message: string, status = 400) { super(message); this.status = status; }
}
function json(body: unknown, status = 200): Response { return Response.json(body, { status, headers: HEADERS }); }
function record(value: unknown): value is Record<string, unknown> { return value !== null && typeof value === "object" && !Array.isArray(value); }
function safePreview(value: unknown): string | null {
  if (typeof value !== "string" || value.length > 4096) return null;
  try { const url = new URL(value); return url.protocol === "https:" && !url.username && !url.password ? url.href : null; } catch { return null; }
}
async function providerError(response: Response): Promise<Response> {
  await response.body?.cancel();
  if (response.status === 401 || response.status === 403) return json({ error: "elevenlabs_credentials_or_permissions_invalid" }, 403);
  if (response.status === 429) return json({ error: "elevenlabs_rate_or_quota_limit" }, 429);
  if (response.status === 400 || response.status === 404 || response.status === 422) return json({ error: "elevenlabs_request_rejected" }, 400);
  return json({ error: "elevenlabs_unavailable" }, 502);
}
async function readJson(request: Request, limit: number): Promise<Record<string, unknown>> {
  if (!request.headers.get("content-type")?.startsWith("application/json")) throw new InputError("expected_json", 415);
  const bytes = await boundedBody(request, limit);
  try { const body: unknown = JSON.parse(new TextDecoder().decode(bytes)); if (record(body)) return body; } catch { /* invalid input */ }
  throw new InputError("invalid_json");
}
async function readProviderJson(response: Response): Promise<Record<string, unknown>> {
  const bytes = await boundedBody(response, 2 * 1024 * 1024);
  const body: unknown = JSON.parse(new TextDecoder().decode(bytes));
  if (!record(body)) throw new Error("invalid provider response");
  return body;
}
async function boundedBody(request: Request | Response, limit: number): Promise<Uint8Array<ArrayBuffer>> {
  if (Number(request.headers.get("content-length")) > limit) { await request.body?.cancel(); throw new InputError("request_too_large", 413); }
  const reader = request.body?.getReader();
  if (!reader) return new Uint8Array();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) { await reader.cancel(); throw new InputError("request_too_large", 413); }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  const result = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.byteLength; }
  return result;
}
