import type { FunctionCallOutput, FunctionCallOutputReceipt } from "../types.mjs";

/** Private host adapter; not exported through the package's public Agent namespace. */
export function functionCallOutputCapability(agent: object, callId: string): Readonly<{
  submit(options: Readonly<{
    output: FunctionCallOutput;
    operationId: string;
  }>): Promise<FunctionCallOutputReceipt>;
  /** Source-turn durable receipt only; this cannot confirm idle wake uptake. */
  activeStatus(options: Readonly<{ originalTurnId: string; operationId: string }>): Promise<Readonly<{
    state: "accepted_unbound" | "bound_unconfirmed" | "confirmed" | "discarded" | "pruned_or_unknown";
    model_call_index?: number;
    response_id?: string;
  }>>;
}>;

/** Host-only typed pending result; never made available to model code. */
export function stageFunctionCallOutput(output: string): object;
