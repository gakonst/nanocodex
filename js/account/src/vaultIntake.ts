import type { ToolActivity } from "nanocodex-react/agent";
import { decodeVaultEntries, type VaultEntryKind } from "./vaultEntries.ts";

export type VaultIntake = Readonly<{ operation: "create" | "browser_verification" | "browser_takeover" | "browser_login"; request_id?: string; allowed_origins?: readonly string[]; vault_id?: string; challenge_id?: string; agent_id?: string; kind: VaultEntryKind; name?: string; origin?: string }>;
export function decodeVaultIntake(tool: ToolActivity): VaultIntake | undefined {
  if (["request_browser_login", "request_browser_login_input"].includes(tool.name.split(".").at(-1) ?? "") && tool.status === "completed" && tool.output) {
    try {
      const v = JSON.parse(tool.output);
      const validOrigin = (origin: unknown): origin is string => {
        if (typeof origin !== "string" || origin.length > 2048) return false;
        const url = new URL(origin); return url.protocol === "https:" && url.origin === origin && !url.username && !url.password;
      };
      if (!v || typeof v !== "object" || Array.isArray(v) || Object.keys(v).some(k => !["type", "status", "request_id", "challenge_id", "agent_id", "origin", "allowed_origins", "expires_at", "approved", "login_url"].includes(k))
        || v.type !== "browser_login" || v.status !== "input_required"
        || typeof v.request_id !== "string" || !/^[A-Za-z0-9_-]{22,256}$/.test(v.request_id) || v.challenge_id !== v.request_id
        || typeof v.agent_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(v.agent_id)
        || !validOrigin(v.origin) || !Array.isArray(v.allowed_origins) || v.allowed_origins.length < 1 || v.allowed_origins.length > 8
        || !v.allowed_origins.every(validOrigin) || !v.allowed_origins.includes(v.origin) || new Set(v.allowed_origins).size !== v.allowed_origins.length
        || typeof v.expires_at !== "number" || !Number.isFinite(v.expires_at) || v.expires_at <= Date.now()) return;
      return { operation: "browser_login", kind: "login", request_id: v.request_id, challenge_id: v.challenge_id, agent_id: v.agent_id, origin: v.origin, allowed_origins: v.allowed_origins };
    } catch { return; }
  }
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
    || !["login", "api_key", "card", "address", "phone", "totp"].includes(String(record.kind))
    || Object.keys(record).some(key => !["type", "status", "operation", "vault_id", "kind", "name", "origin", "challenge_id", "agent_id"].includes(key))
    || (record.name !== undefined && (typeof record.name !== "string" || !record.name.trim() || record.name.length > 120 || /[\u0000-\u001f\u007f]/.test(record.name)))) return;
  const operation = record.operation ?? "create";
  // Legacy website-approval requests no longer require an input form.
  if (operation !== "create" && operation !== "browser_verification") return;
  if (operation === "create" && record.vault_id !== undefined) return;
  if (operation === "browser_verification" && (record.kind !== "login" || typeof record.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(record.vault_id) || record.origin === undefined)) return;
  if (operation === "browser_verification") {
    if (typeof record.challenge_id !== "string" || !/^[A-Za-z0-9_-]{22,256}$/.test(record.challenge_id)
      || typeof record.agent_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(record.agent_id)) return;
  } else if (record.challenge_id !== undefined || record.agent_id !== undefined) return;
  if (record.origin !== undefined) {
    if (!["login", "totp"].includes(String(record.kind)) || typeof record.origin !== "string" || record.origin.length > 2048) return;
    try { const url = new URL(record.origin); if (url.protocol !== "https:" || url.origin !== record.origin) return; } catch { return; }
  }
  return { operation, ...(operation === "browser_verification" ? { challenge_id: record.challenge_id as string, agent_id: record.agent_id as string } : {}), ...(typeof record.vault_id === "string" ? { vault_id: record.vault_id } : {}), kind: record.kind as VaultEntryKind, ...(typeof record.name === "string" ? { name: record.name } : {}), ...(typeof record.origin === "string" ? { origin: record.origin } : {}) };
}

/** Never forward arbitrary Vault response properties into the model transcript. */
export function vaultIntakeReceipt(value: unknown, intake: VaultIntake): string {
  const entry = decodeVaultEntries([value])[0]!;
  const origin = (value as Record<string, unknown>)[entry.kind === "totp" ? "origin" : "browser_origin"];
  if (entry.kind !== intake.kind || (intake.vault_id !== undefined && entry.id !== intake.vault_id)
    || (intake.origin !== undefined && origin !== intake.origin)) throw new Error("Invalid Vault receipt");
  if (origin !== undefined) {
    if (!["login", "totp"].includes(entry.kind) || typeof origin !== "string") throw new Error("Invalid Vault receipt");
    const url = new URL(origin);
    if (url.protocol !== "https:" || url.origin !== origin) throw new Error("Invalid Vault receipt");
  }
  return JSON.stringify({ type: "vault_intake_receipt", operation: intake.operation, status: "saved", id: entry.id, kind: entry.kind, name: entry.name, ...(origin === undefined ? {} : { [entry.kind === "totp" ? "origin" : "browser_origin"]: origin }) });
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

export type BrowserTakeoverAction = { action: "observe" | "click" | "type" | "key" | "scroll" | "finish" | "cancel" | "approve" | "touch" | "edit"; x?: number; y?: number; text?: string; key?: "Enter" | "Tab" | "Backspace" | "Escape"; delta_y?: number; phase?: "start" | "move" | "end" | "cancel"; delete_backward?: number; viewport?: { width: number; height: number; mobile: boolean } };
export type BrowserKeyboard = { type: "text" | "email" | "url" | "tel" | "number" | "password"; multiline: boolean };
export type BrowserInputRegion = BrowserKeyboard & { x: number; y: number; width: number; height: number };
export type BrowserTakeoverFrame = { status: "active"; image: string; width: number; height: number; origin?: string; keyboard?: BrowserKeyboard; inputs?: BrowserInputRegion[] } | { status: "finished" | "cancelled"; request_id?: string } | { status: "approved" };
export async function browserTakeover(intake: VaultIntake, action: BrowserTakeoverAction, request: typeof fetch = fetch, signal?: AbortSignal): Promise<BrowserTakeoverFrame> {
  if ((intake.operation !== "browser_takeover" && intake.operation !== "browser_login") || !/^[A-Za-z0-9_-]{1,128}$/.test(intake.agent_id ?? "") || !/^[A-Za-z0-9_-]{22,256}$/.test(intake.challenge_id ?? "")) throw new Error("Invalid takeover");
  const response = await request(`/v1/agents/${intake.agent_id}/browser-vault/takeover`, { method: "POST", credentials: "same-origin", cache: "no-store", redirect: "error", referrerPolicy: "no-referrer", headers: { "content-type": "application/json", accept: "application/json" }, signal, body: JSON.stringify({ challenge_id: intake.challenge_id, ...action }) });
  if (!response.ok) { await response.body?.cancel(); throw new Error("Takeover unavailable"); }
  const raw: unknown = await response.json();
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("Invalid takeover frame");
  const v = raw as Record<string, unknown>;
  if (intake.operation === "browser_login") {
    if (action.action === "approve" && v.status === "approved" && Object.keys(v).length === 1) return { status: "approved" };
    if ((action.action === "finish" || action.action === "cancel") && v.type === "browser_login_receipt" && v.status === (action.action === "finish" ? "finished" : "cancelled") && v.request_id === intake.request_id && Object.keys(v).length === 3) return { status: v.status as "finished" | "cancelled", request_id: intake.request_id };
    if (["finish", "cancel", "approve"].includes(action.action)) throw new Error("Invalid login receipt");
  }
  if (action.action === "finish" && v?.status === "finished" && Object.keys(v).length === 1) return { status: "finished" };
  if (action.action === "finish" || v?.status !== "active" || Object.keys(v).some(key => !["status", "image", "width", "height", "keyboard", "inputs", "native_form", ...(intake.operation === "browser_login" ? ["origin"] : [])].includes(key)) || typeof v.image !== "string" || v.image.length > 16 * 1024 * 1024 || !/^data:image\/png;base64,[A-Za-z0-9+/]+=*$/.test(v.image) || typeof v.width !== "number" || typeof v.height !== "number" || !Number.isInteger(v.width) || !Number.isInteger(v.height) || v.width < 1 || v.height < 1 || v.width > 16384 || v.height > 16384) throw new Error("Invalid takeover frame");
  if (v.origin !== undefined && (typeof v.origin !== "string" || !intake.allowed_origins?.includes(v.origin))) throw new Error("Unapproved login site");
  const keyboard = (value: unknown, region = false): boolean => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return false;
    const r = value as Record<string, unknown>;
    return Object.keys(r).every(k => ["type", "multiline", ...(region ? ["x", "y", "width", "height"] : [])].includes(k)) && ["text", "email", "url", "tel", "number", "password"].includes(String(r.type)) && typeof r.multiline === "boolean";
  };
  if (v.keyboard !== undefined && !keyboard(v.keyboard)) throw new Error("Invalid keyboard hint");
  if (v.inputs !== undefined && (!Array.isArray(v.inputs) || v.inputs.length > 32 || !v.inputs.every(r => keyboard(r, true) && ["x", "y", "width", "height"].every(k => typeof r[k] === "number" && Number.isFinite(r[k]) && r[k] >= 0 && r[k] <= 1) && r.width > 0 && r.height > 0 && r.x + r.width <= 1.000001 && r.y + r.height <= 1.000001))) throw new Error("Invalid input regions");
  // Native clients use this optional descriptor. Validate and discard it here so
  // the existing screenshot controls remain compatible with newer servers.
  if (v.native_form !== undefined) {
    const form = v.native_form as Record<string, unknown>;
    if (!form || typeof form !== "object" || Array.isArray(form)
      || Object.keys(form).some(k => !["document_id", "fields"].includes(k))
      || typeof form.document_id !== "string" || !/^[0-9a-f-]{36}$/.test(form.document_id)
      || !Array.isArray(form.fields) || form.fields.length < 1 || form.fields.length > 32
      || !form.fields.every(f => f && typeof f === "object" && !Array.isArray(f)
        && Object.keys(f).every(k => ["ref", "label", "type", "multiline", "autocomplete", "inputmode"].includes(k))
        && typeof f.ref === "string" && /^[0-9a-f-]{36}$/.test(f.ref)
        && typeof f.label === "string" && f.label.length <= 160 && !/[\u0000-\u001f\u007f]/.test(f.label)
        && keyboard({type:f.type,multiline:f.multiline})
        && (f.autocomplete === undefined || ["username", "current-password", "new-password", "one-time-code", "email", "tel", "cc-number", "cc-exp", "cc-exp-month", "cc-exp-year", "cc-csc", "name", "given-name", "family-name", "street-address", "postal-code"].includes(f.autocomplete))
        && (f.inputmode === undefined || ["text", "email", "url", "tel", "numeric", "decimal", "search"].includes(f.inputmode)))
      || new Set(form.fields.map(f => f.ref)).size !== form.fields.length) throw new Error("Invalid native form");
  }
  return { status: "active", image: v.image, width: v.width, height: v.height, ...(typeof v.origin === "string" ? {origin:v.origin} : {}), ...(v.keyboard === undefined ? {} : { keyboard: v.keyboard as BrowserKeyboard }), ...(v.inputs === undefined ? {} : { inputs: v.inputs as BrowserInputRegion[] }) };
}
