import type { FunctionCallOutput, FunctionCallOutputReceipt } from "../types.mjs";

/** Private host adapter; not exported through the package's public Agent namespace. */
export function functionCallOutputCapability(agent: object, callId: string): Readonly<{
  submit(options: Readonly<{
    output: FunctionCallOutput;
    operationId: string;
  }>): Promise<FunctionCallOutputReceipt>;
}>;
