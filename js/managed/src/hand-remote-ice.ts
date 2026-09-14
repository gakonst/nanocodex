export type RemoteICEEnv = {
  NANOCODEX_TURN_KEY_ID?: string;
  NANOCODEX_TURN_API_TOKEN?: string;
};

type ICE = { urls: string[]; username?: string; credential?: string };
const cache = new Map<string, { expires: number; servers: ICE[] }>();

/** Only authenticated owners receive short-lived TURN credentials, never the API token. */
export async function remoteICE(env: RemoteICEEnv, owner: string): Promise<Response> {
  const headers = { "cache-control": "no-store" };
  if (!env.NANOCODEX_TURN_KEY_ID || !env.NANOCODEX_TURN_API_TOKEN) {
    return Response.json({ iceServers: [{ urls: ["stun:stun.cloudflare.com:3478"] }], relay: false }, { headers });
  }
  const key = `${env.NANOCODEX_TURN_KEY_ID}:${owner}`;
  const cached = cache.get(key);
  if (cached && cached.expires > Date.now()) return Response.json({ iceServers: cached.servers, relay: true }, { headers });
  try {
    const response = await fetch(`https://rtc.live.cloudflare.com/v1/turn/keys/${encodeURIComponent(env.NANOCODEX_TURN_KEY_ID)}/credentials/generate-ice-servers`, {
      method: "POST", headers: { authorization: `Bearer ${env.NANOCODEX_TURN_API_TOKEN}`, "content-type": "application/json" },
      body: JSON.stringify({ ttl: 3600, customIdentifier: owner }), redirect: "manual", signal: AbortSignal.timeout(5000),
    });
    if (!response.ok) return Response.json({ error: "remote_relay_unavailable" }, { status: 503, headers });
    const body = await response.json<{ iceServers?: unknown }>();
    const values = Array.isArray(body.iceServers) ? body.iceServers : [body.iceServers];
    const servers: ICE[] = values.map(value => {
      if (!value || typeof value !== "object") throw new Error("Invalid TURN response");
      const entry = value as Record<string, unknown>;
      const urls = Array.isArray(entry.urls) ? entry.urls : [entry.urls];
      if (!urls.length || urls.length > 16 || !urls.every(url => typeof url === "string" && /^(stun|turn|turns):/.test(url) && url.length <= 1024)
        || (entry.username !== undefined && typeof entry.username !== "string")
        || (entry.credential !== undefined && typeof entry.credential !== "string")) throw new Error("Invalid TURN response");
      return { urls: urls as string[], ...(typeof entry.username === "string" ? { username: entry.username } : {}),
        ...(typeof entry.credential === "string" ? { credential: entry.credential } : {}) };
    });
    if (cache.size >= 256) cache.delete(cache.keys().next().value!);
    cache.set(key, { servers, expires: Date.now() + 10 * 60_000 });
    return Response.json({ iceServers: servers, relay: true }, { headers });
  } catch { return Response.json({ error: "remote_relay_unavailable" }, { status: 503, headers }); }
}
