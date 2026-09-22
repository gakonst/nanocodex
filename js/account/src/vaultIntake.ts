import type { ToolActivity } from "nanocodex-react/agent";
import { decodeVaultEntries, type VaultEntryKind } from "./vaultEntries.ts";

export type VaultIntake = Readonly<{ operation: "create" | "authorize_origin" | "browser_verification" | "browser_takeover"; vault_id?: string; challenge_id?: string; agent_id?: string; kind: VaultEntryKind; name?: string; origin?: string }>;
export function decodeVaultIntake(tool: ToolActivity): VaultIntake | undefined {
  if (["browser_vault_request_challenge", "browser_vault_request_takeover"].includes(tool.name.split(".").at(-1) ?? "") && tool.status === "completed" && tool.output) {
    try {
      const v = JSON.parse(tool.output);
      if (!v || typeof v !== "object" || Array.isArray(v) || Object.keys(v).some(k => !["type", "status", "challenge_id", "agent_id", "origin", "expires_at"].includes(k))
        || v.type !== (tool.name.split(".").at(-1) === "browser_vault_request_takeover" ? "browser_vault_takeover" : "browser_vault_challenge") || v.status !== "input_required"
        || typeof v.challenge_id !== "string" || !/^[A-Za-z0-9_-]{22,256}$/.test(v.challenge_id)
        || typeof v.agent_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(v.agent_id)
        || typeof v.origin !== "string" || v.origin.length > 2048
        || typeof v.expires_at !== "number" || !Number.isFinite(v.expires_at) || v.expires_at <= 0) return;
      const url = new URL(v.origin);
      if (url.protocol !== "https:" || url.origin !== v.origin || url.username || url.password) return;
      return { operation: v.type === "browser_vault_takeover" ? "browser_takeover" : "browser_verification", kind: "login", challenge_id: v.challenge_id, agent_id: v.agent_id, origin: v.origin };
    } catch { return; }
  }
  if (tool.name.split(".").at(-1) !== "request_vault_intake" || tool.status !== "completed" || !tool.output) return;
  let value: unknown;
  try { value = JSON.parse(tool.output); } catch { return; }
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  const record = value as Record<string, unknown>;
  if (record.type !== "vault_intake" || record.status !== "input_required"
    || !["login", "api_key", "card", "address", "phone"].includes(String(record.kind))
    || Object.keys(record).some(key => !["type", "status", "operation", "vault_id", "kind", "name", "origin", "challenge_id", "agent_id"].includes(key))
    || (record.name !== undefined && (typeof record.name !== "string" || !record.name.trim() || record.name.length > 120 || /[\u0000-\u001f\u007f]/.test(record.name)))) return;
  const operation = record.operation ?? "create";
  if (operation !== "create" && operation !== "authorize_origin" && operation !== "browser_verification") return;
  if (operation === "create" && record.vault_id !== undefined) return;
  if ((operation === "authorize_origin" || operation === "browser_verification") && (record.kind !== "login" || typeof record.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(record.vault_id) || record.origin === undefined)) return;
  if (operation === "browser_verification") {
    if (typeof record.challenge_id !== "string" || !/^[A-Za-z0-9_-]{22,256}$/.test(record.challenge_id)
      || typeof record.agent_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(record.agent_id)) return;
  } else if (record.challenge_id !== undefined || record.agent_id !== undefined) return;
  if (record.origin !== undefined) {
    if (record.kind !== "login" || typeof record.origin !== "string" || record.origin.length > 2048) return;
    try { const url = new URL(record.origin); if (url.protocol !== "https:" || url.origin !== record.origin) return; } catch { return; }
  }
  return { operation, ...(operation === "browser_verification" ? { challenge_id: record.challenge_id as string, agent_id: record.agent_id as string } : {}), ...(typeof record.vault_id === "string" ? { vault_id: record.vault_id } : {}), kind: record.kind as VaultEntryKind, ...(typeof record.name === "string" ? { name: record.name } : {}), ...(typeof record.origin === "string" ? { origin: record.origin } : {}) };
}

/** Never forward arbitrary Vault response properties into the model transcript. */
export function vaultIntakeReceipt(value: unknown, intake: VaultIntake): string {
  const entry = decodeVaultEntries([value])[0]!;
  const origin = (value as Record<string, unknown>).browser_origin;
  if (entry.kind !== intake.kind || (intake.vault_id !== undefined && entry.id !== intake.vault_id)
    || (intake.origin !== undefined && origin !== intake.origin)) throw new Error("Invalid Vault receipt");
  if (origin !== undefined) {
    if (entry.kind !== "login" || typeof origin !== "string") throw new Error("Invalid Vault receipt");
    const url = new URL(origin);
    if (url.protocol !== "https:" || url.origin !== origin) throw new Error("Invalid Vault receipt");
  }
  return JSON.stringify({ type: "vault_intake_receipt", operation: intake.operation, status: "saved", id: entry.id, kind: entry.kind, name: entry.name, ...(origin === undefined ? {} : { browser_origin: origin }) });
}

/** Direct, ephemeral browser submission; never use transcript transport for code values. */
export async function submitBrowserVerification(intake: VaultIntake, code: string, request: typeof fetch = fetch): Promise<string> {
  if (intake.operation !== "browser_verification" || !/^[A-Za-z0-9_-]{1,128}$/.test(intake.agent_id ?? "")
    || !/^[A-Za-z0-9_-]{22,256}$/.test(intake.challenge_id ?? "") || !/^[0-9]{4,10}$/.test(code)) throw new Error("Invalid verification request");
  const response = await request(`/v1/agents/${intake.agent_id}/browser-vault/challenge`, {
    method: "POST", credentials: "same-origin", cache: "no-store", redirect: "error", referrerPolicy: "no-referrer",
    headers: { "content-type": "application/json", accept: "application/json" }, body: JSON.stringify({ challenge_id: intake.challenge_id, code }),
  });
  if (!response.ok) { await response.body?.cancel(); throw new Error("Verification could not be confirmed"); }
  const value: unknown = await response.json();
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length !== 3 || (value as {type?: unknown}).type !== "browser_vault_challenge_receipt" || (value as {challenge_id?: unknown}).challenge_id !== intake.challenge_id || (value as {status?: unknown}).status !== "submitted") throw new Error("Invalid verification receipt");
  return JSON.stringify({ type: "browser_vault_challenge_receipt", status: "submitted", challenge_id: intake.challenge_id });
}

export type BrowserTakeoverAction = { action: "observe" | "click" | "type" | "key" | "scroll" | "finish" | "touch" | "edit"; x?: number; y?: number; text?: string; key?: "Enter" | "Tab" | "Backspace" | "Escape"; delta_y?: number; phase?: "start" | "move" | "end" | "cancel"; delete_backward?: number; viewport?: { width: number; height: number; mobile: boolean } };
export type BrowserKeyboard = { type: "text" | "email" | "url" | "tel" | "number" | "password"; multiline: boolean };
export type BrowserInputRegion = BrowserKeyboard & { x: number; y: number; width: number; height: number };
export type BrowserTakeoverFrame = { status: "active"; image: string; width: number; height: number; keyboard?: BrowserKeyboard; inputs?: BrowserInputRegion[] } | { status: "finished" };
export async function browserTakeover(intake: VaultIntake, action: BrowserTakeoverAction, request: typeof fetch = fetch, signal?: AbortSignal): Promise<BrowserTakeoverFrame> {
  if (intake.operation !== "browser_takeover" || !/^[A-Za-z0-9_-]{1,128}$/.test(intake.agent_id ?? "") || !/^[A-Za-z0-9_-]{22,256}$/.test(intake.challenge_id ?? "")) throw new Error("Invalid takeover");
  const response = await request(`/v1/agents/${intake.agent_id}/browser-vault/takeover`, { method: "POST", credentials: "same-origin", cache: "no-store", redirect: "error", referrerPolicy: "no-referrer", headers: { "content-type": "application/json", accept: "application/json" }, signal, body: JSON.stringify({ challenge_id: intake.challenge_id, ...action }) });
  if (!response.ok) { await response.body?.cancel(); throw new Error("Takeover unavailable"); }
  const raw: unknown = await response.json();
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("Invalid takeover frame");
  const v = raw as Record<string, unknown>;
  if (action.action === "finish" && v?.status === "finished" && Object.keys(v).length === 1) return { status: "finished" };
  if (action.action === "finish" || v?.status !== "active" || Object.keys(v).some(key => !["status", "image", "width", "height", "keyboard", "inputs"].includes(key)) || typeof v.image !== "string" || v.image.length > 16 * 1024 * 1024 || !/^data:image\/png;base64,[A-Za-z0-9+/]+=*$/.test(v.image) || typeof v.width !== "number" || typeof v.height !== "number" || !Number.isInteger(v.width) || !Number.isInteger(v.height) || v.width < 1 || v.height < 1 || v.width > 16384 || v.height > 16384) throw new Error("Invalid takeover frame");
  const keyboard = (value: unknown, region = false): boolean => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return false;
    const r = value as Record<string, unknown>;
    return Object.keys(r).every(k => ["type", "multiline", ...(region ? ["x", "y", "width", "height"] : [])].includes(k)) && ["text", "email", "url", "tel", "number", "password"].includes(String(r.type)) && typeof r.multiline === "boolean";
  };
  if (v.keyboard !== undefined && !keyboard(v.keyboard)) throw new Error("Invalid keyboard hint");
  if (v.inputs !== undefined && (!Array.isArray(v.inputs) || v.inputs.length > 32 || !v.inputs.every(r => keyboard(r, true) && ["x", "y", "width", "height"].every(k => typeof r[k] === "number" && Number.isFinite(r[k]) && r[k] >= 0 && r[k] <= 1) && r.width > 0 && r.height > 0 && r.x + r.width <= 1.000001 && r.y + r.height <= 1.000001))) throw new Error("Invalid input regions");
  return { status: "active", image: v.image, width: v.width, height: v.height, ...(v.keyboard === undefined ? {} : { keyboard: v.keyboard as BrowserKeyboard }), ...(v.inputs === undefined ? {} : { inputs: v.inputs as BrowserInputRegion[] }) };
}
