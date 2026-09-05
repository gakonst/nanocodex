import type { Client } from "../Client.mjs";
import type { Grant, Hex } from "../types.mjs";

export declare namespace revoke {
  type Options = Readonly<{ grantId: Hex; signal?: AbortSignal | undefined }>;
  type ReturnType = Promise<Grant>;
  type ErrorType = Error;
}

export function revoke(client: Client, options: revoke.Options): revoke.ReturnType;
