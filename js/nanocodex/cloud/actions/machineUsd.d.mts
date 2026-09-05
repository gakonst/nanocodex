import type { Client } from "../Client.mjs";
import type { Hex, MachineUsdConfig, MachineUsdFunding } from "../types.mjs";

export declare namespace getConfig {
  type Options = Readonly<{ signal?: AbortSignal | undefined }>;
  type ReturnType = Promise<MachineUsdConfig>;
  type ErrorType = Error;
}

export function getConfig(client: Client, options?: getConfig.Options | undefined): getConfig.ReturnType;

export declare namespace fund {
  type Options = Readonly<{
    grantId: Hex;
    accountAddress: Hex;
    usdAmountCents: number;
    signal?: AbortSignal | undefined;
  }>;
  type ReturnType = Promise<MachineUsdFunding>;
  type ErrorType = Error;
}

export function fund(client: Client, options: fund.Options): fund.ReturnType;
