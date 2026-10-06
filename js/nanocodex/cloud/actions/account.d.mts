import type { Client } from "../Client.mjs";

export declare namespace logout {
  type ReturnType = Promise<void>;
  type ErrorType = Error;
}

/** Signs out the Nanocodex account without revoking its app grant or access key. */
export function logout(client: Client): logout.ReturnType;

export declare namespace links {
  type Options = Readonly<{
    connect?: "claude" | "chatgpt" | "openai" | "mcp" | "cloudflare" | "github" | "google" | "slack" | "x" | "spotify" | "soundcloud" | "link" | "whatsapp" | undefined;
    add?: "login" | "api_key" | "card" | "address" | "phone" | "totp" | undefined;
  }>;
  type Result = Readonly<{ connections: string; vault: string; wallet: string; access: string }>;
  type ReturnType = Promise<Result>;
}

/** Public navigation only; these URLs do not carry account authorization. */
export function links(client: Client, options?: links.Options): links.ReturnType;
