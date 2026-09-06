// Codex ac192cd793 history/notes protocol; local credentials stay in the Vite owner.
const PATHS = new Set([
  "alpha/history/v2/list_windows", "alpha/history/v2/list_items",
  "alpha/history/v2/read_item", "alpha/history/v2/search_contents",
  "alpha/notes/v2/list_files_by_prefix", "alpha/notes/v2/read_file",
  "alpha/notes/v2/search_contents", "alpha/notes/v2/write_file",
  "alpha/notes/v2/append_to_file", "alpha/notes/v2/thread_hint",
]);
const ID = /^[A-Za-z0-9._:-]{1,200}$/;

function eligible(auth) {
  try {
    const claims = JSON.parse(Buffer.from(auth.accessToken.split(".")[1], "base64url").toString());
    return ["plus", "pro", "prolite"].includes(claims?.["https://api.openai.com/auth"]?.chatgpt_plan_type);
  } catch { return false; }
}

export async function proxyHistoryNotes(request, response, loadAuth, requestFetch = fetch) {
  const reply = (status, body) => {
    response.writeHead(status, { "content-type": "application/json", "cache-control": "no-store", "x-content-type-options": "nosniff" });
    response.end(JSON.stringify(body));
  };
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 35_000);
  const closed = () => controller.abort();
  response.once("close", closed);
  try {
    const chunks = [];
    let size = 0;
    for await (const chunk of request) {
      size += chunk.length;
      if (size > 32 * 1024 * 1024) { reply(413, { error: "request_too_large" }); return; }
      chunks.push(chunk);
    }
    let input;
    try { input = JSON.parse(Buffer.concat(chunks).toString()); }
    catch { reply(400, { error: "invalid_request" }); return; }
    let auth = await loadAuth();
    if (input && Object.keys(input).length === 0) {
      reply(200, { enabled: eligible(auth) });
      return;
    }
    if (!eligible(auth)) { reply(409, { error: "experimental_context_unavailable" }); return; }
    const { path, body, budget, threadId } = input ?? {};
    if (!PATHS.has(path) || !ID.test(threadId ?? "")
      || !ID.test(body?.context?.session_id ?? "")
      || !/^\/root(?:\/[A-Za-z0-9_-]+)*$/.test(body?.context?.current_agent_name ?? "")
      || body.context.current_agent_name.length > 1024
      || !["tokens", "bytes"].includes(budget?.mode) || !Number.isSafeInteger(budget?.limit)
      || budget.limit < 1 || budget.limit > (budget.mode === "bytes" ? 4000 : 128_000)) {
      reply(400, { error: "invalid_request" });
      return;
    }
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const headers = new Headers({
        authorization: `Bearer ${auth.accessToken}`, "ChatGPT-Account-ID": auth.accountId,
        "content-type": "application/json", originator: "nanocodex", "user-agent": "nanocodex-vite",
        "x-openai-tool-output-truncation-policy": JSON.stringify({ mode: budget.mode, limit: budget.limit }),
      });
      if (auth.fedramp) headers.set("X-OpenAI-Fedramp", "true");
      if (path.endsWith("/search_contents") || path.endsWith("/write_file") || path.endsWith("/append_to_file")) {
        headers.set("x-openai-encrypted-tool-arguments", "true");
      }
      const upstream = await requestFetch(`https://chatgpt.com/backend-api/codex/${path}`, {
        method: "POST", headers, body: JSON.stringify(body), redirect: "error", signal: controller.signal,
      });
      if (upstream.status === 401 && attempt === 0) {
        await upstream.body?.cancel();
        auth = await loadAuth();
        if (!eligible(auth)) break;
        continue;
      }
      if (!upstream.ok) { await upstream.body?.cancel(); break; }
      reply(200, await upstream.json());
      return;
    }
    reply(502, { error: "history_notes_request_failed" });
  } catch {
    if (!response.headersSent) reply(503, { error: "history_notes_unavailable" });
  } finally {
    clearTimeout(timeout);
    response.off("close", closed);
  }
}
