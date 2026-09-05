import type { Client } from "../Client.mjs";

export declare namespace logout {
  type ReturnType = Promise<void>;
  type ErrorType = Error;
}

/** Signs out the Nanocodex account without revoking its app grant or access key. */
export function logout(client: Client): logout.ReturnType;
