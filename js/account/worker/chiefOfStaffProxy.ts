export type ChiefOfStaffProxyEnv = {
  CHIEF_OF_STAFF?: Readonly<{ fetch(request: Request): Promise<Response> }>;
};

const browserPath = "/api/chief-of-staff/status";

export async function routeChiefOfStaff(
  request: Request,
  env: ChiefOfStaffProxyEnv,
  url: URL,
): Promise<Response | undefined> {
  if (url.pathname !== browserPath) return undefined;
  if (request.method !== "GET") {
    return json({ error: "method_not_allowed" }, { status: 405, headers: { allow: "GET" } });
  }
  if (!env.CHIEF_OF_STAFF) {
    return json({ error: "chief_of_staff_unavailable" }, { status: 503 });
  }
  try {
    return await env.CHIEF_OF_STAFF.fetch(new Request(
      "https://chief-of-staff.internal/v1/readiness",
      { headers: request.headers, method: "GET" },
    ));
  } catch (error) {
    console.error({
      type: "chief_of_staff.backend_failure",
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return json({ error: "chief_of_staff_unavailable" }, { status: 503 });
  }
}

function json(body: unknown, init: ResponseInit): Response {
  return Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...init.headers,
    },
  });
}
