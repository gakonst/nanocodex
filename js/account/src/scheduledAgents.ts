import { isRecord, responseFailure } from "./accountSessionRequest.ts";

/** Unknown legacy presence remains a candidate until its schedules are read. */
export async function scheduledAgentCandidates(fetcher: typeof fetch = fetch, signal?: AbortSignal): Promise<{ id: string; title: string }[]> {
  const response = await fetcher("/v1/agents", { credentials: "same-origin", signal, headers: { accept: "application/json" } });
  if (!response.ok) throw await responseFailure(response, "Could not load scheduled agents.");
  const body: unknown = await response.json();
  if (!isRecord(body) || !Array.isArray(body.data) || !body.data.every(id => typeof id === "string")) {
    throw new Error("Invalid agent list response.");
  }
  const summaries = isRecord(body.summaries) ? body.summaries : {};
  return body.data.flatMap((id: string) => {
    const summary = summaries[id];
    if (isRecord(summary) && summary.may_have_scheduled_jobs === false) return [];
    return [{ id, title: isRecord(summary) && typeof summary.title === "string" ? summary.title : id }];
  });
}

/** Bound legacy discovery fan-out while retaining ordered, all-or-error results. */
export async function readScheduledAgents<T>(
  candidates: readonly { id: string; title: string }[],
  read: (candidate: { id: string; title: string }) => Promise<T>,
  signal: AbortSignal,
): Promise<T[]> {
  const rows = new Array<T>(candidates.length);
  let cursor = 0;
  let failed = false;
  const workers = Array.from({ length: Math.min(4, candidates.length) }, async () => {
    try {
      while (!failed && cursor < candidates.length) {
        signal.throwIfAborted();
        const index = cursor++;
        rows[index] = await read(candidates[index]!);
      }
    } catch (error) {
      failed = true;
      throw error;
    }
  });
  const results = await Promise.allSettled(workers);
  for (const result of results) if (result.status === "rejected") throw result.reason;
  signal.throwIfAborted();
  return rows;
}
