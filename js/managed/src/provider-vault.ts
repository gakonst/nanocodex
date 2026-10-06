import type { NamedTool, ToolContext } from "nanocodex";

const ID = /^[A-Za-z0-9_-]{22,64}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const STATUS = new Set(["captured", "pending", "awaiting_card", "awaiting_billing_address", "saved", "ready", "balance_pending", "outcome_unknown", "failed", "refresh_pending", "awaiting_issuer_approval"]);
const ERROR = new Set([
  "invalid_provider_request", "provider_capture_unavailable", "provider_capture_conflict",
  "provider_response_invalid", "provider_unavailable", "provider_outcome_unknown",
  "vault_entry_unavailable", "provider_refresh_limited", "address_not_found", "address_required",
  "capture_expired", "capture_not_found", "capture_operation_conflict", "capture_unavailable",
  "invalid_capture", "invalid_operation", "invalid_operation_id", "invalid_reference", "invalid_store",
  "method_not_allowed", "provider_auth_invalid", "provider_auth_reconciliation_required",
  "provider_card_invalid", "provider_card_mismatch", "provider_operation_failed",
  "provider_refresh_reconciliation_required", "provider_refresh_unavailable", "refresh_operation_conflict",
  "refresh_rate_limited", "store_operation_conflict", "unsupported_capture_schema", "unsupported_provider",
  "vault_entry_deleted", "vault_save_failed",
]);

/** Fixed projection even for corrupted private bindings; never propagate provider text. */
export function safeProviderReceipt(value: unknown): Record<string, unknown> {
  if (!record(value)) return { error: "provider_response_invalid" };
  if (typeof value.error === "string") return { error: ERROR.has(value.error) ? value.error : "provider_unavailable" };
  if (typeof value.status !== "string" || !STATUS.has(value.status)) return { error: "provider_response_invalid" };
  const result: Record<string, unknown> = { status: value.status };
  for (const key of ["capture_id", "vault_id"])
    if (typeof value[key] === "string" && ID.test(value[key])) result[key] = value[key];
  if (value.provider === "laso") result.provider = "laso";
  if (value.kind === "card" || value.kind === "api_key") result.kind = value.kind;
  if (typeof value.last4 === "string" && /^\d{4}$/.test(value.last4)) result.last4 = value.last4;
  if (value.currency === "USD") result.currency = "USD";
  if (["unknown", "stale", "current"].includes(value.freshness)) result.freshness = value.freshness;
  if (typeof value.balance === "number" && Number.isFinite(value.balance) && value.balance >= 0) result.balance = value.balance;
  for (const key of ["observed_at", "provider_updated_at", "retry_after"])
    if (typeof value[key] === "number" && Number.isFinite(value[key]) && value[key] >= 0) result[key] = value[key];
  return result;
}

export async function providerVaultRequest(binding: Pick<Fetcher, "fetch">, owner: string,
  operation: "capture" | "store" | "card", input: unknown, signal?: AbortSignal): Promise<Record<string, unknown>> {
  try {
    const response = await binding.fetch(`https://broker.internal/users/${encodeURIComponent(owner)}/credentials/provider-${operation}`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(input), signal, redirect: "manual",
    });
    const reader = response.body?.getReader();
    if (!reader) return { error: "provider_response_invalid" };
    let length = 0; const chunks: Uint8Array[] = [];
    try { for (;;) {
      const { done, value } = await reader.read(); if (done) break;
      length += value.byteLength;
      if (length > 4096) { await reader.cancel(); return { error: "provider_response_invalid" }; }
      chunks.push(value);
    } } finally { reader.releaseLock(); }
    const bytes = new Uint8Array(length); let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    return safeProviderReceipt(JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes)));
  } catch { return { error: "provider_outcome_unknown" }; }
}

export function createProviderVaultTools(binding: Pick<Fetcher, "fetch">, owner: string,
  authorize: (context: ToolContext) => void): NamedTool[] {
  return [{
    name: "vault_store",
    description: "Save a host-captured provider-issued key or card directly to your private Vault without typing or exposing its secret. Supply only the capture_id returned by a trusted private capture and a stable operation_id UUID. Identical retries recover the save receipt; never repeat a purchase to retry saving. An optional address_vault_id supplies a real saved billing ZIP when required. Pending does not mean saved. Direct account only.",
    parameters: { type: "object", additionalProperties: false, required: ["capture_id", "operation_id"], properties: {
      capture_id: { type: "string" }, operation_id: { type: "string" }, name: { type: "string", maxLength: 120 }, address_vault_id: { type: "string" },
    } },
    handler: async (input, context) => {
      authorize(context);
      if (!record(input) || Object.keys(input).some(key => !["capture_id", "operation_id", "name", "address_vault_id"].includes(key))
        || typeof input.capture_id !== "string" || !ID.test(input.capture_id)
        || typeof input.operation_id !== "string" || !UUID.test(input.operation_id)
        || (input.name !== undefined && (typeof input.name !== "string" || !input.name.trim() || input.name.length > 120 || /[\u0000-\u001f\u007f]/.test(input.name)))
        || (input.address_vault_id !== undefined && (typeof input.address_vault_id !== "string" || !ID.test(input.address_vault_id)))) return { error: "invalid_provider_request" };
      return providerVaultRequest(binding, owner, "store", input, context.signal);
    },
  }, {
    name: "provider_card",
    description: "Read status or balance of a privately captured or saved Laso card. Supply its capture_id or vault_id. Tokens and card details stay in the private broker. Balance includes observation time and may be cached by the issuer. Refresh requests an asynchronous issuer update (at most once per five minutes); it does not imply funding was credited. This tool never issues, funds, or authenticates a new card. Direct account only.",
    parameters: { type: "object", additionalProperties: false, required: ["operation"], properties: {
      operation: { type: "string", enum: ["status", "balance", "refresh"] }, capture_id: { type: "string" }, vault_id: { type: "string" }, operation_id: { type: "string", description: "Stable UUID required for refresh." },
    } },
    handler: async (input, context) => {
      authorize(context);
      if (!record(input) || Object.keys(input).some(key => !["operation", "capture_id", "vault_id", "operation_id"].includes(key))
        || !["status", "balance", "refresh"].includes(String(input.operation))
        || (input.operation === "refresh" && (typeof input.operation_id !== "string" || !UUID.test(input.operation_id)))
        || (input.operation_id !== undefined && (typeof input.operation_id !== "string" || !UUID.test(input.operation_id)))
        || (input.capture_id === undefined) === (input.vault_id === undefined)
        || !ID.test(String(input.capture_id ?? input.vault_id))) return { error: "invalid_provider_request" };
      return providerVaultRequest(binding, owner, "card", input, context.signal);
    },
  }];
}
function record(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
