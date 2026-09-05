export type HostManagedWebSocketOptions = Readonly<{
  WebSocketImpl?: typeof WebSocket | undefined;
  timeoutMs?: number | undefined;
}>;

export function openHostManagedWebSocket(
  endpoint: string | URL,
  sessionId: string,
  options?: HostManagedWebSocketOptions,
): Promise<WebSocket>;

export function defaultHostManagedWebSocketUrl(
  location?: Pick<Location, "href">,
): string;
