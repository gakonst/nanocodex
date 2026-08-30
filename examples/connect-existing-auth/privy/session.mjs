/** Verify Privy's host token and bridge only its user DID into Connect. */
export function createPrivySessionRoute({
  privy,
  sessions,
  readAccessToken = privyCookie,
}) {
  if (!privy || typeof privy.verifyAuthToken !== "function") {
    throw new TypeError("Privy example requires a Privy server client");
  }
  if (typeof readAccessToken !== "function") {
    throw new TypeError("Privy example requires an access-token reader");
  }
  return sessions.handler({
    async authenticate(request) {
      const token = await readAccessToken(request);
      if (!token) return undefined;
      const claims = await privy.verifyAuthToken(token);
      return claims?.userId ? { subject: claims.userId } : undefined;
    },
  });
}

function privyCookie(request) {
  const cookie = request.headers.get("cookie") ?? "";
  for (const part of cookie.split(";")) {
    const [name, ...value] = part.trim().split("=");
    if (name === "privy-token") return decodeURIComponent(value.join("="));
  }
  return undefined;
}
