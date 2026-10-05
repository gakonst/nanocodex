declare const brand: unique symbol;
/** Opaque provider selection. Credentials are held privately by the SDK. */
export type Codex = Readonly<{ kind: 'codex'; [brand]: 'codex' }>;
export type Claude = Readonly<{ kind: 'claude'; [brand]: 'claude' }>;
export type Backend = Codex | Claude;
export type CodexOptions = Readonly<{
  apiKey: string;
  apiBaseUrl?: string;
  websocketUrl?: string;
  websocketWarmup?: boolean;
}>;
export type ClaudeOptions = Readonly<{ apiKey: string; endpoint?: string }>;
export function codex(options: CodexOptions): Codex;
export function claude(options: ClaudeOptions): Claude;
