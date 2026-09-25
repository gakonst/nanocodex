/** A bounded API key selects its owner locally; no account/DO lookup on turns. */
export type Principal = Readonly<{ sub: string }>;

let cached: { source: string; owners: Record<string, string> } | undefined;

export async function authenticate(request: Request, keyHashes: string | undefined): Promise<Principal | undefined> {
  const match = /^Bearer (ncx2_[A-Za-z0-9_-]{43})$/.exec(request.headers.get("authorization") ?? "");
  if (!match || !keyHashes) return undefined;
  try {
    if (cached?.source !== keyHashes) cached = { source: keyHashes, owners: JSON.parse(keyHashes) };
    const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(match[1]!)));
    const digest = btoa(String.fromCharCode(...hash)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
    const sub = cached!.owners[digest];
    return typeof sub === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(sub) ? { sub } : undefined;
  } catch { return undefined; }
}
