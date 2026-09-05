import {
  localConnectorCallbackReturn,
} from "../localConnectorCallback.ts";

export type LocalConnectorCallbackRelayEnv = Readonly<{
  ENVIRONMENT?: string;
  NANOCODEX_BACKEND?: Fetcher;
  NANOCODEX_CONNECT_API?: Fetcher;
}>;

export async function routeLocalConnectorCallbackReturn(
  request: Request,
  env: LocalConnectorCallbackRelayEnv,
  url: URL,
): Promise<Response | undefined> {
  if (env.ENVIRONMENT !== "development" || request.method !== "GET") return undefined;
  const returned = localConnectorCallbackReturn(url);
  if (!returned) return undefined;
  const backend = returned.flow === "connect" ? env.NANOCODEX_CONNECT_API : env.NANOCODEX_BACKEND;
  if (!backend) return unavailable(returned.flow);
  try {
    return await backend.fetch(new Request(returned.callbackUrl, request));
  } catch (error) {
    console.error({
      type: "connector_callback.backend_failure",
      flow: returned.flow,
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return unavailable(returned.flow);
  }
}

function unavailable(flow: "connect" | "managed"): Response {
  return Response.json({ error: `${flow}_service_unavailable` }, {
    status: 503,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;
