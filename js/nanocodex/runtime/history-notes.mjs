// Pinned Codex history/notes HTTP boundary. Credentials never enter tool arguments.
const CODEX_BASE = "https://chatgpt.com/backend-api/codex";
const PATHS = new Set([
  "alpha/history/v2/list_windows",
  "alpha/history/v2/list_items",
  "alpha/history/v2/search_contents",
  "alpha/history/v2/read_item",
  "alpha/notes/v2/list_files_by_prefix",
  "alpha/notes/v2/search_contents",
  "alpha/notes/v2/read_file",
  "alpha/notes/v2/write_file",
  "alpha/notes/v2/append_to_file",
  "alpha/notes/v2/thread_hint",
]);
const ENCRYPTED = new Set([
  "alpha/history/v2/search_contents", "alpha/notes/v2/search_contents",
  "alpha/notes/v2/write_file", "alpha/notes/v2/append_to_file",
]);

export function historyNotesHost({ direct = false, broker, apiBaseUrl, fetch: requestFetch = globalThis.fetch } = {}) {
  const active = new Map();
  return Object.freeze({
    async capability(threadId, baseUrl) {
      if (broker && baseUrl === apiBaseUrl) {
        return await broker.available(threadId) ? "host_managed" : "none";
      }
      return direct && baseUrl.replace(/\/$/, "") === CODEX_BASE ? "direct" : "none";
    },
    async request(threadId, encoded, bearer, accountId, fedramp) {
      const request = JSON.parse(encoded);
      if (!PATHS.has(request.path)) {
        throw new Error("Invalid history/notes request");
      }
      const controller = new AbortController();
      const owned = active.get(threadId) ?? new Set();
      active.set(threadId, owned);
      owned.add(controller);
      const timeout = setTimeout(() => controller.abort(), 35_000);
      try {
        let response;
        if (broker && request.baseUrl === apiBaseUrl) {
          if (bearer !== "host-managed" || accountId || fedramp) {
            throw new Error("History/notes broker requires host-managed authentication");
          }
          response = await broker.request({
            path: request.path, body: request.body, budget: request.budget,
            threadId, signal: controller.signal,
          });
        } else {
          if (!direct || request.baseUrl.replace(/\/$/, "") !== CODEX_BASE) {
            throw new Error("History/notes endpoint is unavailable");
          }
          const headers = new Headers({
            authorization: `Bearer ${bearer}`,
            "content-type": "application/json",
            originator: "nanocodex",
            "user-agent": "nanocodex-js",
            "x-openai-tool-output-truncation-policy": JSON.stringify(request.budget),
          });
          if (accountId) headers.set("ChatGPT-Account-ID", accountId);
          if (fedramp) headers.set("X-OpenAI-Fedramp", "true");
          if (ENCRYPTED.has(request.path)) headers.set("x-openai-encrypted-tool-arguments", "true");
          response = await requestFetch(`${CODEX_BASE}/${request.path}`, {
            method: "POST", headers, body: JSON.stringify(request.body),
            redirect: "error", cache: "no-store", signal: controller.signal,
          });
        }
        if (!response.ok) {
          await response.body?.cancel();
          return JSON.stringify({ status: response.status, body: null });
        }
        return JSON.stringify({ status: response.status, body: await response.json() });
      } catch {
        throw new Error("The authenticated history/notes request failed");
      } finally {
        clearTimeout(timeout);
        owned.delete(controller);
        if (owned.size === 0) active.delete(threadId);
      }
    },
    cancel(threadId) {
      if (threadId === undefined) {
        for (const requests of active.values()) for (const controller of requests) controller.abort();
        return;
      }
      for (const controller of active.get(threadId) ?? []) controller.abort();
    },
  });
}

/** Credential-free same-origin protocol owned by the Vite subscription plugin. */
export function sameOriginHistoryNotes(websocketUrl, location = globalThis.location) {
  if (!location?.href || !websocketUrl) return undefined;
  const endpoint = new URL(websocketUrl, location.href);
  endpoint.protocol = endpoint.protocol === "wss:" ? "https:" : "http:";
  if (endpoint.origin !== new URL(location.href).origin || endpoint.search || endpoint.hash) return undefined;
  endpoint.pathname += "/context";
  const post = (body, signal) => fetch(endpoint, {
    method: "POST", credentials: "same-origin", redirect: "error", cache: "no-store",
    headers: { "content-type": "application/json" }, body: JSON.stringify(body), signal,
  });
  return Object.freeze({
    async available() {
      try {
        const response = await post({});
        if (!response.ok) { await response.body?.cancel(); return false; }
        return (await response.json())?.enabled === true;
      } catch { return false; }
    },
    request({ path, body, budget, threadId, signal }) {
      return post({ path, body, budget, threadId }, signal);
    },
  });
}
