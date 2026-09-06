import type { UserCredentialSnapshot } from "./broker";

export const HISTORY_NOTES_PATHS = [
  "alpha/history/v2/list_windows", "alpha/history/v2/list_items",
  "alpha/history/v2/read_item", "alpha/history/v2/search_contents",
  "alpha/notes/v2/list_files_by_prefix", "alpha/notes/v2/read_file",
  "alpha/notes/v2/search_contents", "alpha/notes/v2/write_file",
  "alpha/notes/v2/append_to_file", "alpha/notes/v2/thread_hint",
] as const;

export function historyNotesEligible(credential: UserCredentialSnapshot & { source?: string }): boolean {
  if (credential.kind !== "chatgpt" || credential.source === "sponsored") return false;
  try {
    const payload = credential.secret.split(".")[1];
    if (!payload) return false;
    const claims = JSON.parse(atob(payload.replace(/-/g, "+").replace(/_/g, "/")));
    return ["plus", "pro", "prolite"].includes(claims?.["https://api.openai.com/auth"]?.chatgpt_plan_type);
  } catch { return false; }
}

export function historyNotesHeaders(request: Request, body: Uint8Array | null): Headers {
  const budget = JSON.parse(request.headers.get("x-openai-tool-output-truncation-policy") ?? "null");
  if (!budget || typeof budget !== "object" || Array.isArray(budget)
    || Object.keys(budget).some((key) => key !== "mode" && key !== "limit")
    || !["tokens", "bytes"].includes(budget.mode) || !Number.isSafeInteger(budget.limit)
    || budget.limit < 1 || budget.limit > (budget.mode === "bytes" ? 4000 : 128_000)) {
    throw new Error("invalid_history_notes_budget");
  }
  if (!body) throw new Error("invalid_history_notes_context");
  const input = JSON.parse(new TextDecoder().decode(body));
  const context = input?.context;
  if (!context || typeof context.session_id !== "string"
    || !/^[A-Za-z0-9._:-]{1,200}$/.test(context.session_id)
    || context.session_id !== request.headers.get("session-id")
    || typeof context.current_agent_name !== "string"
    || !/^\/root(?:\/[A-Za-z0-9_-]+)*$/.test(context.current_agent_name)
    || context.current_agent_name.length > 1024) {
    throw new Error("invalid_history_notes_context");
  }
  const headers = new Headers({
    "x-openai-tool-output-truncation-policy": JSON.stringify(budget),
  });
  const path = new URL(request.url).pathname;
  if (path.endsWith("/search_contents") || path.endsWith("/write_file") || path.endsWith("/append_to_file")) {
    headers.set("x-openai-encrypted-tool-arguments", "true");
  }
  return headers;
}
