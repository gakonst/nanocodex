import { BrowserTakeoverCard } from "./BrowserTakeoverCard";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ToolActivity } from "nanocodex-react/agent";
import { useAccountSession } from "./AccountSession";
import { VaultEntryDialog } from "./Vault";
import { decodeVaultEntries, vaultEntryPath, type VaultEntryMetadata, type VaultEntryKind } from "./vaultEntries";
import { decodeVaultIntake, submitBrowserVerification, vaultIntakeReceipt, type VaultIntake } from "./vaultIntake";

export function VaultIntakeCard({ tool, onReceipt }: { tool: ToolActivity; onReceipt(receipt: string): void }) {
  const account = useAccountSession();
  const intake = decodeVaultIntake(tool);
  if (!intake) return null;
  // Account changes unmount any open form and discard all of its values.
  if (intake.operation === "browser_takeover") return <BrowserTakeoverCard key={`${account.account?.id}:${tool.callId}`} intake={intake} authenticated={account.account?.persistent === true} onReceipt={onReceipt} />;
  if (intake.operation === "browser_verification") return <BrowserVerificationCard key={`${account.account?.id}:${tool.callId}`} intake={intake} authenticated={account.account?.persistent === true} onReceipt={onReceipt} />;
  return <IntakeCard key={`${account.account?.persistent ? account.account.id : "signed-out"}:${tool.callId}`} intake={intake} authenticated={account.account?.persistent === true} onReceipt={onReceipt} />;
}

function IntakeCard({ intake, authenticated, onReceipt }: { intake: VaultIntake; authenticated: boolean; onReceipt(receipt: string): void }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [uncertain, setUncertain] = useState(false);
  const [error, setError] = useState<string>();
  const [entry, setEntry] = useState<VaultEntryMetadata>();
  const authorizing = intake.operation === "authorize_origin";
  const trigger = useRef<HTMLButtonElement>(null);
  const alive = useRef(true);
  const submitting = useRef(false);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);
  useEffect(() => {
    if (!authorizing || !authenticated) return;
    let current = true;
    void (async () => {
      try {
        const response = await fetch("/v1/credentials", { credentials: "same-origin", cache: "no-store", redirect: "error", referrerPolicy: "no-referrer", headers: { accept: "application/json" } });
        if (!response.ok) { await response.body?.cancel(); throw new Error(); }
        const data = await response.json() as { vault?: unknown };
        const actual = decodeVaultEntries(data.vault).find(item => item.id === intake.vault_id && item.kind === "login");
        if (!actual) throw new Error();
        if (current) setEntry(actual);
      } catch { if (current) setError("Couldn’t verify this login. Open your Vault to check the item."); }
    })();
    return () => { current = false; };
  }, [authenticated, authorizing, intake.vault_id]);
  const save = async (kind: VaultEntryKind, values: Record<string, string>) => {
    if (submitting.current || busy || uncertain || saved || !authenticated || (authorizing && !entry)) return;
    submitting.current = true;
    setBusy(true);
    setError(undefined);
    try {
      const response = await fetch(authorizing ? `${vaultEntryPath("login", intake.vault_id)}/origin` : vaultEntryPath(kind), {
        method: authorizing ? "PUT" : "POST", credentials: "same-origin", cache: "no-store", redirect: "error", referrerPolicy: "no-referrer",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify(authorizing ? { browser_origin: intake.origin } : values),
      });
      if (!response.ok) {
        await response.body?.cancel();
        if (![400, 401, 403, 422].includes(response.status)) throw new Error("unknown");
        if (alive.current) setError(response.status === 401 ? "Sign in to your account before saving." : "Couldn’t save. Check the fields and try again.");
        return;
      }
      const receipt = vaultIntakeReceipt(await response.json(), { ...intake, origin: authorizing ? intake.origin : values.browser_origin });
      if (!alive.current) return;
      setSaved(true);
      setOpen(false);
      onReceipt(receipt);
    } catch {
      if (alive.current) {
        setUncertain(true);
        setOpen(false);
        setError("The save could not be confirmed. Check your Vault before adding this item again.");
      }
    } finally { submitting.current = false; if (alive.current) setBusy(false); }
  };
  return <section className="vault-intake-card" aria-label="Secure Vault intake">
    <strong>{saved ? "Saved to Vault" : (authorizing ? `Approve website for ${entry?.name ?? "saved login"}` : `Add ${intake.name ?? intake.kind.replace("_", " ")} to Vault`)}</strong>
    <p>Credential values go directly to your encrypted Vault and stay out of this conversation.</p>
    {authenticated ? <button ref={trigger} type="button" disabled={saved || uncertain || busy || (authorizing && !entry)} onClick={() => setOpen(true)}>{saved ? "Saved" : authorizing ? "Review website approval" : "Open secure form"}</button> : <a href="/vault">Sign in to add to Vault</a>}
    {error && !open ? <p role="alert">{error}</p> : null}
    {open && authorizing ? <div className="vault-intake-approval" role="group" aria-label="Website approval">
      <p>Allow the saved login <strong>{entry?.name}</strong> ({entry?.id}) to be used at <strong>{intake.origin}</strong>?</p>
      <p>This replaces the login’s approved website. Your password remains in Vault.</p>
      {error ? <p role="alert">{error}</p> : null}
      <button type="button" disabled={busy} onClick={() => { setOpen(false); setError(undefined); }}>Cancel</button>
      <button type="button" disabled={busy} onClick={() => void save("login", {})}>{busy ? "Saving…" : `Approve ${intake.origin}`}</button>
    </div> : null}
    {open && !authorizing ? createPortal(<VaultEntryDialog busy={busy} kind={intake.kind} name={intake.name} origin={intake.origin} returnFocusRef={trigger} onClose={() => { setOpen(false); setError(undefined); }} onSave={save} error={error} description="Values are sent directly to your encrypted Vault, outside chat and tool messages." />, document.body) : null}
  </section>;
}

function BrowserVerificationCard({ intake, authenticated, onReceipt }: { intake: VaultIntake; authenticated: boolean; onReceipt(receipt: string): void }) {
  const [open, setOpen] = useState(false);
  const [attempted, setAttempted] = useState(false);
  const [status, setStatus] = useState("");
  const alive = useRef(true);
  const submitting = useRef(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => { alive.current = true; return () => { alive.current = false; if (input.current) input.current.value = ""; }; }, []);
  useEffect(() => {
    const clear = () => { if (document.visibilityState !== "visible") { if (input.current) input.current.value = ""; setOpen(false); } };
    document.addEventListener("visibilitychange", clear);
    return () => document.removeEventListener("visibilitychange", clear);
  }, []);
  return <section className="vault-intake-card" aria-label="Secure browser verification">
    <strong>Verify browser login</strong><p>{intake.origin}</p>
    <p>The code goes directly to this browser session. It stays out of chat and is not saved to Vault.</p>
    <button type="button" disabled={!authenticated || attempted} onClick={() => setOpen(true)}>Enter verification code securely</button>
    {open && !attempted ? <form onSubmit={event => {
      event.preventDefault();
      if (submitting.current || !input.current?.value) return;
      submitting.current = true;
      const code = input.current.value; input.current.value = "";
      setAttempted(true); setOpen(false); setStatus("Submitting…");
      void submitBrowserVerification(intake, code).then(receipt => {
        if (!alive.current) return;
        setStatus("Code submitted. Browser verification is pending."); onReceipt(receipt);
      }).catch(() => { if (alive.current) setStatus("Couldn’t confirm submission. Request a new secure form before trying again."); });
    }}>
      <label>Verification code <input ref={input} type="password" autoComplete="one-time-code" autoFocus inputMode="numeric" pattern="[0-9]{4,10}" minLength={4} maxLength={10} required spellCheck={false} autoCapitalize="none" /></label>
      <button type="button" onClick={() => { if (input.current) input.current.value = ""; setOpen(false); }}>Cancel</button>
      <button type="submit">Submit code</button>
    </form> : null}
    {status ? <p role="status">{status}</p> : null}
  </section>;
}
