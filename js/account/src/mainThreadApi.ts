import { isRecord, responseFailure } from "./accountSessionRequest.ts";

export type ProjectNavigationItem = Readonly<{
  id: string;
  name: string;
  coordinator_agent_id: string | null;
}>;

async function request(path: string, method: "GET" | "PUT", fetcher: typeof fetch, signal?: AbortSignal): Promise<unknown> {
  const response = await fetcher(path, {
    method, signal, credentials: "same-origin", cache: "no-store",
    headers: { accept: "application/json" },
  });
  if (path === "/v1/main-thread" && method === "GET" && response.status === 404) return { agent_id: null };
  if (!response.ok) throw await responseFailure(response, "Couldn’t load your workspace.");
  return response.json();
}

export async function mainThread(method: "GET" | "PUT", fetcher: typeof fetch = fetch, signal?: AbortSignal): Promise<string | null> {
  const body = await request("/v1/main-thread", method, fetcher, signal);
  if (isRecord(body) && (typeof body.agent_id === "string" && body.agent_id.length > 0 || method === "GET" && body.agent_id === null)) return body.agent_id as string | null;
  throw new Error("Invalid Main Thread response.");
}

export async function listProjects(fetcher: typeof fetch = fetch, signal?: AbortSignal): Promise<ProjectNavigationItem[]> {
  const body = await request("/v1/projects", "GET", fetcher, signal);
  if (!isRecord(body) || !Array.isArray(body.data)) throw new Error("Invalid projects response.");
  return body.data.map((project: unknown) => {
    if (!isRecord(project) || typeof project.id !== "string" || !project.id || typeof project.name !== "string"
      || !(project.coordinator_agent_id === null || typeof project.coordinator_agent_id === "string" && project.coordinator_agent_id.length > 0)) throw new Error("Invalid project response.");
    return { id: project.id, name: project.name, coordinator_agent_id: project.coordinator_agent_id as string | null };
  });
}
