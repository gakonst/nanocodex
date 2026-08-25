const CLIENT_NETWORK_FAILURE = /(?:load failed|failed to fetch|fetch failed|network request failed|networkerror|dynamically imported module|agent connection (?:timed out|rejected|returned an invalid handshake)|websocket (?:connection failed|closed during connection)|responses? websockets? handshake)/i;

export function isClientNetworkFailure(cause: unknown): boolean {
  const message = cause instanceof Error ? cause.message : String(cause ?? "");
  return CLIENT_NETWORK_FAILURE.test(message);
}

export function clientFailureMessage(cause: unknown, fallback: string): string {
  if (isClientNetworkFailure(cause)) return fallback;
  return cause instanceof Error && cause.message ? cause.message : fallback;
}
