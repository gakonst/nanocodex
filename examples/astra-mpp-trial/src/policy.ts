export const MACH = "0x20c000000000000000000000f37de3740adec032" as const;
export const USDC_E = "0x20c000000000000000000000b9537d11c60e8b50" as const;
export const TIP20_CHANNEL_ESCROW = "0x33b901018174ddabe4841042ab76ba85d4e24f25" as const;
export const TEMPO_CHAIN_ID = 4_217;
export const ASTRA_SETTINGS = Object.freeze({
  model: "gpt-6-astra" as const,
  thinking: "max" as const,
  reasoningMode: "standard" as const,
  fastMode: false,
});

export type TrialPhase =
  | "available"
  | "payment_pending"
  | "paid"
  | "running"
  | "completed"
  | "failed";

export type TrialState = Readonly<{
  agentId?: string;
  error?: string;
  finalMessage?: string;
  paymentReference?: string;
  phase: TrialPhase;
  prompt?: string;
  promptHash?: string;
  requestKey?: string;
  turnId?: string;
  updatedAt: number;
}>;

export function paymentAmount(environment: string | undefined): "0" | "50" {
  const value = environment?.trim().toLowerCase();
  return value === "development" || value === "local" || value === "test" ? "0" : "50";
}

export function reservePrompt(
  current: TrialState | undefined,
  promptHash: string,
  requestKey: string,
  now = Date.now(),
): TrialState | "conflict" {
  if (!current || current.phase === "available") {
    return { phase: "payment_pending", promptHash, requestKey, updatedAt: now };
  }
  return current.promptHash === promptHash && current.requestKey === requestKey
    ? current
    : "conflict";
}

export function publicTrialState(state: TrialState | undefined): Record<string, unknown> {
  const current = state ?? { phase: "available", updatedAt: 0 };
  return {
    phase: current.phase,
    ...(current.finalMessage ? { final_message: current.finalMessage } : {}),
    ...(current.error ? { error: current.error } : {}),
    ...(current.paymentReference ? { payment_reference: current.paymentReference } : {}),
  };
}
