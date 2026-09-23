export type RemoteICEEnv = {
  NANOCODEX_TURN_KEY_ID?: string;
  NANOCODEX_TURN_API_TOKEN?: string;
};

type ICE = { urls: string[]; username?: string; credential?: string };
// Credential lifetime is distinct from the shorter server-side lookup cache.
// Preserve the original conservative issuance time on every cache hit.
type Credentials = { iceServers: ICE[]; expires_at: number };
const credentialTTLSeconds = 3600;
const cache = new Map<string, { expires: number; credentials: Credentials }>();
// Host and viewer request credentials together during setup. Share only the
// credential generation work, never a consumable Response or another owner's
// credentials. Failed requests are removed so reconnect can retry immediately.
const pending = new Map<string, Promise<Credentials>>();
const unavailable = () => Response.json({ error: "remote_relay_unavailable" }, {
  status: 503, headers: { "cache-control": "no-store" },
});

/** Only authenticated owners receive short-lived TURN credentials, never the API token. */
export async function remoteICE(env: RemoteICEEnv, owner: string): Promise<Response> {
  const headers = { "cache-control": "no-store" };
  if (!env.NANOCODEX_TURN_KEY_ID || !env.NANOCODEX_TURN_API_TOKEN) {
    return Response.json({ iceServers: [{ urls: ["stun:stun.cloudflare.com:3478"] }], relay: false }, { headers });
  }
  const key = `${env.NANOCODEX_TURN_KEY_ID}:${owner}`;
  const cached = cache.get(key);
  if (cached && cached.expires > Date.now()) return Response.json({ ...cached.credentials, relay: true }, { headers });
  let work = pending.get(key);
  if (!work) {
    // Keep in-flight work bounded across owners without evicting a request that
    // is still being awaited (which would allow duplicate upstream requests).
    if (pending.size >= 256) return unavailable();
    work = generateICE(env.NANOCODEX_TURN_KEY_ID, env.NANOCODEX_TURN_API_TOKEN, owner).then(credentials => {
      if (cache.size >= 256) cache.delete(cache.keys().next().value!);
      cache.set(key, { credentials, expires: Date.now() + 10 * 60_000 });
      return credentials;
    }).finally(() => { pending.delete(key); });
    pending.set(key, work);
  }
  try {
    return Response.json({ ...await work, relay: true }, { headers });
  } catch { return unavailable(); }
}

async function generateICE(keyID: string, token: string, owner: string): Promise<Credentials> {
  // Start before the network call so transport/provider latency cannot extend
  // the advertised lifetime beyond the credentials issued upstream.
  const expires_at = Date.now() + credentialTTLSeconds * 1000;
  const response = await fetch(`https://rtc.live.cloudflare.com/v1/turn/keys/${encodeURIComponent(keyID)}/credentials/generate-ice-servers`, {
    method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ ttl: credentialTTLSeconds, customIdentifier: owner }), redirect: "manual", signal: AbortSignal.timeout(5000),
  });
  if (!response.ok) throw new Error("TURN credentials unavailable");
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
  return { iceServers: servers, expires_at };
}
