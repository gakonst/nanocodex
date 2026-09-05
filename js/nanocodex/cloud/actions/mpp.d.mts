import type { Client } from "../Client.mjs";
import type { Connection, Hex, MppCharge } from "../types.mjs";

export declare namespace getBalance {
  type Options = Readonly<{
    grantId: Hex;
    signal?: AbortSignal | undefined;
  }>;
  type ReturnType = Promise<Connection>;
  type ErrorType = Error;
}

/** Refreshes the grant's Tempo balances independently from authentication. */
export function getBalance(client: Client, options: getBalance.Options): getBalance.ReturnType;

export declare namespace charge {
  type Options = Readonly<{
    grantId: Hex;
    amount: bigint;
    origin: string;
    memo?: string | undefined;
    signal?: AbortSignal | undefined;
  }>;
  type ReturnType = Promise<MppCharge>;
  type ErrorType = Error;
}

export function charge(client: Client, options: charge.Options): charge.ReturnType;
