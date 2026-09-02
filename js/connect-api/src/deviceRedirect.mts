const productionApiOrigin = "https://nanocodex-connect-api.gakonst.workers.dev";
const productionSiteOrigin = "https://nanocodex.gakonst.workers.dev";

export function deviceVerificationUrl(apiOrigin: string, userCode?: string): URL {
  const siteOrigin = apiOrigin === productionApiOrigin
    ? productionSiteOrigin
    : isLocalDevelopmentOrigin(apiOrigin)
      ? apiOrigin
      : undefined;
  if (!siteOrigin) throw new Error("The Connect API origin is not allowed.");
  const url = new URL("/connect", siteOrigin);
  url.searchParams.set("api_origin", apiOrigin);
  if (userCode) url.searchParams.set("user_code", userCode);
  return url;
}

export function connectAuthOrigin(apiOrigin: string): string {
  if (
    apiOrigin === productionApiOrigin
    || isLocalDevelopmentOrigin(apiOrigin)
  ) return apiOrigin;
  throw new Error("The Connect API origin is not allowed.");
}

export function isLocalDevelopmentOrigin(value: string): boolean {
  try {
    const url = new URL(value);
    const hostname = url.hostname.toLowerCase();
    return url.origin === value
      && (url.protocol === "http:" || url.protocol === "https:")
      && (
        hostname === "localhost"
        || hostname === "127.0.0.1"
        || hostname === "[::1]"
        || hostname === "nanocodex.localhost"
        || /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/.test(hostname)
      );
  } catch {
    return false;
  }
}
