import { hasBearerToken } from "./bearer-auth";
import { RequestError } from "./validation";

export function requireTerminalAuthorization(
  request: Request,
  configuredToken = process.env.NANOCODEX_TERMINAL_TOKEN?.trim(),
): void {
  if (!configuredToken) {
    throw new RequestError(
      "terminal_disabled",
      "workspace terminal is disabled; configure NANOCODEX_TERMINAL_TOKEN",
      503,
    );
  }

  if (!hasBearerToken(request, configuredToken)) {
    throw new RequestError(
      "terminal_unauthorized",
      "workspace terminal token was rejected",
      401,
    );
  }
}
