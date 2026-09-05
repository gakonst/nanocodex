import { timingSafeEqual } from "node:crypto";

export function hasBearerToken(request: Request, configuredToken: string): boolean {
  const header = request.headers.get("authorization");
  const suppliedToken = header?.startsWith("Bearer ")
    ? header.slice("Bearer ".length)
    : "";
  const configured = Buffer.from(configuredToken);
  const supplied = Buffer.from(suppliedToken);
  return supplied.length === configured.length && timingSafeEqual(supplied, configured);
}
