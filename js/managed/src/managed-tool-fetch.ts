import { DEFAULT_OPENAI_AGENT_SETTINGS, isAgentModel } from "./agent-settings";
import { createManagedImageFetch } from "./managed-image-fetch";

type ManagedToolPath = "/v1/search" | "/v1/images/generations" | "/v1/images/edits";

/**
 * Managed web search and image tools. `egress` is the Session's scoped model
 * egress: it maps the storage identity to the retained credential subject,
 * applies the retained ChatGPT account selection, and, for session_v1 Sessions,
 * sends these exact routes through the private SessionModelEgress binding with
 * a live local owner assertion. The general broker would otherwise resolve the
 * subject by calling back into the originating Session.
 */
export function managedWebFetch(egress: Pick<Fetcher, "fetch">, storageId: string): typeof fetch {
  return async (input, init) => {
    const incoming = new Request(input, init);
    const value = await incoming.json<{
      commands?: unknown;
      model?: unknown;
      session_id?: unknown;
    }>();
    if (!value.commands || typeof value.commands !== "object" || Array.isArray(value.commands)
      || typeof value.session_id !== "string" || !value.session_id
      || (value.model !== undefined && !isAgentModel(value.model))) {
      return Response.json({ error: "invalid managed web request" }, {
        status: 400, headers: { "cache-control": "no-store" },
      });
    }
    return fetchManagedTool(egress, storageId, "/v1/search", {
      id: value.session_id,
      model: value.model ?? DEFAULT_OPENAI_AGENT_SETTINGS.model,
      commands: value.commands,
      settings: { allowed_callers: ["direct"], external_web_access: true },
      max_output_tokens: 10_000,
    });
  };
}

export function managedImageFetch(egress: Pick<Fetcher, "fetch">, storageId: string): typeof fetch {
  return createManagedImageFetch((path, body) => fetchManagedTool(egress, storageId, path, body));
}

function fetchManagedTool(
  egress: Pick<Fetcher, "fetch">,
  storageId: string,
  path: ManagedToolPath,
  body: unknown,
): Promise<Response> {
  return egress.fetch(new Request(`https://nanocodex.internal${path}`, {
    method: "POST",
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      "content-type": "application/json",
      "user-agent": "nanocodex-managed/0.1.0",
      // The scoped egress requires the storage identity and rewrites it.
      "x-nanocodex-subject": storageId,
    },
    body: JSON.stringify(body),
  }));
}
