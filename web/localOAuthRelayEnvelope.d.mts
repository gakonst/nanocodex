export type LocalOAuthProvider = "github" | "gmail" | "gdrive" | "x" | "slack";
export type LocalOAuthFlow = "connect" | "managed";
export type LocalOAuthRelayEnvelope = Readonly<{
  v: 1;
  p: LocalOAuthProvider;
  o: string;
  f: LocalOAuthFlow;
  s: string;
  i: number;
  e: number;
  n: string;
}>;
export type LocalMcpOAuthRelayEnvelope = Readonly<{
  v: 1;
  c: string;
  o: string;
  f: LocalOAuthFlow;
  s: string;
  i: number;
  e: number;
  n: string;
}>;

export const LOCAL_OAUTH_RELAY_HOST: "127.0.0.1";
export const LOCAL_OAUTH_RELAY_PORT: 47891;
export const LOCAL_OAUTH_RELAY_ORIGIN: "http://127.0.0.1:47891";

export function localOAuthRelayCallbackUrl(provider: string): string | undefined;
export function localMcpOAuthRelayCallbackUrl(connectionId: string): string | undefined;
export function isLocalNanocodexOrigin(value: string): boolean;
export function signLocalOAuthRelayState(
  value: Readonly<{
    provider: string;
    connectionId?: undefined;
    targetOrigin: string;
    flow: string;
    state: string;
  }>,
  secret: string,
  options?: Readonly<{ now?: number; nonce?: string }>,
): Promise<string>;
export function signLocalOAuthRelayState(
  value: Readonly<{
    provider?: undefined;
    connectionId: string;
    targetOrigin: string;
    flow: string;
    state: string;
  }>,
  secret: string,
  options?: Readonly<{ now?: number; nonce?: string }>,
): Promise<string>;
export function verifyLocalOAuthRelayState(
  value: unknown,
  expectedProvider: string,
  secret: string,
  options?: Readonly<{ now?: number }>,
): Promise<LocalOAuthRelayEnvelope | undefined>;
export function verifyLocalMcpOAuthRelayState(
  value: unknown,
  expectedConnectionId: string,
  secret: string,
  options?: Readonly<{ now?: number }>,
): Promise<LocalMcpOAuthRelayEnvelope | undefined>;
export function localOAuthRelayCallbackRedirect(
  url: URL,
  secret: string,
  options?: Readonly<{ now?: number }>,
): Promise<URL | undefined>;
export function localOAuthRelayChallengeProof(challenge: string, secret: string): Promise<string>;
