type RateLimitBinding = {
  limit(options: { key: string }): Promise<{ success: boolean }>;
};

export type PublicSecurityEnv = {
  ENVIRONMENT: string;
  AUTH_START_LIMIT?: RateLimitBinding;
  AUTH_GLOBAL_LIMIT?: RateLimitBinding;
  SESSION_POLL_LIMIT?: RateLimitBinding;
  AGENT_SOCKET_LIMIT?: RateLimitBinding;
  AGENT_TOOL_LIMIT?: RateLimitBinding;
  AGENT_IMAGE_LIMIT?: RateLimitBinding;
  MULTIPLAYER_CREATE_LIMIT?: RateLimitBinding;
  MULTIPLAYER_GLOBAL_LIMIT?: RateLimitBinding;
  MULTIPLAYER_ROUTE_LIMIT?: RateLimitBinding;
};

export type MeteredOperation = "socket" | "search" | "image";

export async function limitLoginStart(
  request: Request,
  env: PublicSecurityEnv,
): Promise<Response | undefined> {
  const fingerprint = await digestKey([
    request.headers.get("cf-connecting-ip") ?? "unknown-ip",
    request.headers.get("user-agent") ?? "unknown-agent",
  ].join("\n"));
  return await enforce(env, env.AUTH_GLOBAL_LIMIT, "login:global")
    ?? await enforce(env, env.AUTH_START_LIMIT, `login:${fingerprint}`);
}

export async function limitMultiplayerCreate(
  request: Request,
  env: PublicSecurityEnv,
): Promise<Response | undefined> {
  const fingerprint = await digestKey([
    request.headers.get("cf-connecting-ip") ?? "unknown-ip",
  ].join("\n"));
  return await enforce(env, env.MULTIPLAYER_CREATE_LIMIT, `multiplayer:${fingerprint}`)
    ?? await enforce(env, env.MULTIPLAYER_GLOBAL_LIMIT, "multiplayer:global");
}

export async function limitMultiplayerRoute(
  request: Request,
  env: PublicSecurityEnv,
): Promise<Response | undefined> {
  const actor = await digestKey(request.headers.get("cf-connecting-ip") ?? "unknown-ip");
  return enforce(env, env.MULTIPLAYER_ROUTE_LIMIT, `multiplayer-route:${actor}`);
}

export async function limitSessionPoll(
  env: PublicSecurityEnv,
  sessionId: string,
): Promise<Response | undefined> {
  return enforce(env, env.SESSION_POLL_LIMIT, `poll:${sessionId}`);
}

export async function limitAgentOperation(
  env: PublicSecurityEnv,
  actorId: string,
  operation: MeteredOperation,
): Promise<Response | undefined> {
  const actor = await digestKey(actorId);
  const binding = operation === "socket"
    ? env.AGENT_SOCKET_LIMIT
    : operation === "image"
      ? env.AGENT_IMAGE_LIMIT
      : env.AGENT_TOOL_LIMIT;
  return enforce(env, binding, `${operation}:${actor}`);
}

export async function apiKeyActorId(apiKey: string): Promise<string> {
  return `api-key:${await digestKey(apiKey)}`;
}

async function enforce(
  env: PublicSecurityEnv,
  binding: RateLimitBinding | undefined,
  key: string,
): Promise<Response | undefined> {
  if (!binding) {
    if (env.ENVIRONMENT === "production" || env.ENVIRONMENT === "preview") {
      return unavailable();
    }
    return undefined;
  }
  try {
    const { success } = await binding.limit({ key });
    return success ? undefined : rateLimited();
  } catch {
    return unavailable();
  }
}

function rateLimited(): Response {
  return Response.json(
    { error: "rate_limit_exceeded" },
    {
      status: 429,
      headers: {
        "cache-control": "no-store",
        "retry-after": "60",
      },
    },
  );
}

function unavailable(): Response {
  return Response.json(
    { error: "abuse_protection_unavailable" },
    { status: 503, headers: { "cache-control": "no-store" } },
  );
}

async function digestKey(value: string): Promise<string> {
  const digest = new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)),
  );
  let binary = "";
  for (const byte of digest) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
