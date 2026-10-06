import { tempo } from "mppx/client";
import { z } from "zod";
import { mcpPayment, type PaidMcpPayment } from "nanocodex/tempo";
type QuoteClient = { callTool: (params: { name: string; arguments: unknown }, schema?: unknown,
  options?: { signal?: AbortSignal; timeout?: number }) => Promise<{
    isError?: boolean; structuredContent?: unknown; content?: Array<{ type: string; text?: string }>;
  }> };

/** Mercator stays a normal default MCP server. Only its paid create_job call
 * can ask the private account broker to sign the validated MPP charge. No wallet
 * material or generic signing capability enters the model or MCP transport. */
export function mercatorMcpPayment(
  broker: Pick<Fetcher, "fetch">, owner: string, authorize: (context: unknown) => void,
): PaidMcpPayment {
  const context = z.custom<{
    tool: "create_job";
    input: Record<string, unknown>;
    signal?: AbortSignal;
  }>((value) => isRecord(value) && value.tool === "create_job" && isRecord(value.input)
    && typeof value.input.idempotency_key === "string" && isRecord(value.input.plan)
    && typeof value.input.approved_total === "string");
  return mcpPayment({
    context: async ({ name, arguments: input }: { name: string; arguments: unknown }, call?: { signal?: AbortSignal }, client?: QuoteClient) => {
      if (name === "create_job") {
        authorize(call);
        if (!isRecord(input) || typeof input.idempotency_key !== "string"
          || !isRecord(input.plan) || typeof input.approved_total !== "string" || !client) {
          throw new Error("Mercator create_job needs a plan and quoted total");
        }
        // The quote is a free call to the same default MCP, not another model
        // tool or a separate REST payment integration. Fail closed on drift.
        const quote = await client.callTool({ name: "quote_plan", arguments: {
          plan: input.plan, ...(typeof input.id === "string" ? { id: input.id } : {}),
        } }, undefined,
          { signal: call?.signal, timeout: 30_000 });
        const data = quote.structuredContent ?? quote.content?.find(block => block.type === "text")?.text;
        let parsed: unknown;
        try { parsed = typeof data === "string" ? JSON.parse(data) : data; } catch { /* invalid quote */ }
        if (quote.isError || !isRecord(parsed) || typeof parsed.totalAmount !== "string"
          || typeof parsed.validUntil !== "string" || !Number.isFinite(Date.parse(parsed.validUntil))
          || Date.parse(parsed.validUntil) <= Date.now()
          || amountMicros(parsed.totalAmount) === null
          || amountMicros(parsed.totalAmount) !== amountMicros(input.approved_total)) {
          throw new Error("Mercator quote is unavailable, expired, or differs from the submitted total");
        }
      }
      call?.signal?.throwIfAborted();
      if (name === "create_job") authorize(call);
      return { tool: name, input, signal: call?.signal, authorization: call };
    },
    methods: [{
      ...tempo.charge(),
      context,
      async createCredential({ challenge, context: call }: { challenge: unknown; context: { input: Record<string, unknown>; signal?: AbortSignal; authorization?: unknown } }) {
        call.signal?.throwIfAborted();
        authorize(call.authorization);
        const response = await broker.fetch(`https://broker.internal/users/${encodeURIComponent(owner)}/wallet/mercator/credential`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ ...call.input, challenge }),
          signal: call.signal,
        });
        if (!response.ok) {
          void response.body?.cancel().catch(() => {});
          throw new Error(response.status === 404 ? "Account wallet is not configured" :
            response.status === 400 ? "Mercator payment challenge or quoted amount is invalid" :
              response.status === 409 ? "Mercator operation key is already bound to another plan; reconcile before retrying" :
                response.status === 422 ? "Wallet could not sign the payment; fund it or retry the same job key after recovery" :
                "Mercator payment outcome is uncertain; retry only the identical operation key and plan");
        }
        const result: unknown = await response.json();
        if (!isRecord(result) || typeof result.credential !== "string" || result.credential.length > 32_768) {
          throw new Error("Mercator broker returned an invalid payment credential");
        }
        call.signal?.throwIfAborted();
        authorize(call.authorization);
        return result.credential;
      },
    }],
  });
}
function isRecord(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function amountMicros(value: string): bigint | null {
  if (!/^\d{1,72}(?:\.\d{1,6})?$/.test(value)) return null;
  const [whole, fraction = ""] = value.split(".");
  return BigInt(whole!) * 1_000_000n + BigInt(fraction.padEnd(6, "0"));
}
