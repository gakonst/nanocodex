import { useEffect, useRef, useState } from "react";
import { Cloud } from "lucide-react";
import { useAccountQuery } from "./useAccountQuery";
import { isRecord, useAccountSession } from "./AccountSession";
import { decodeVaultEntries } from "./vaultEntries";

function keys(value: unknown) {
  if (!isRecord(value)) throw new Error("Invalid Vault response.");
  return decodeVaultEntries(value.vault).filter(entry => entry.kind === "api_key");
}
function status(value: unknown): { id: string; label: string }[] {
  if (!isRecord(value) || !isRecord(value.connectors)) throw new Error("Invalid connector status.");
  const cloudflare = value.connectors.cloudflare;
  if (cloudflare === undefined) return [];
  if (!isRecord(cloudflare)) throw new Error("Invalid Cloudflare status.");
  if (cloudflare.connected === false && cloudflare.connections === undefined) return [];
  if (!Array.isArray(cloudflare.connections)) throw new Error("Invalid Cloudflare connections.");
  return cloudflare.connections.map(connection => {
    if (!isRecord(connection) || typeof connection.id !== "string" || typeof connection.label !== "string") throw new Error("Invalid Cloudflare connection.");
    return { id: connection.id, label: connection.label };
  });
}

export function CloudflareConnection({ accountId, requiresLogin = false }: { accountId: string; requiresLogin?: boolean }) {
  const session = useAccountSession();
  const vault = useAccountQuery(accountId, "/v1/credentials", keys, { enabled: !requiresLogin });
  const connections = useAccountQuery(accountId, "/v1/connectors", status, { enabled: !requiresLogin });
  const [vaultId, setVaultId] = useState("");
  const [owner, setOwner] = useState("");
  const [expanded, setExpanded] = useState(() => new URLSearchParams(window.location.search).get("connect") === "cloudflare");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [uncertain, setUncertain] = useState(false);
  const pending = useRef<AbortController | null>(null);
  useEffect(() => () => { pending.current?.abort(); pending.current = null; }, [accountId, requiresLogin]);

  async function act(id?: string) {
    if (pending.current || requiresLogin || uncertain) return;
    const controller = new AbortController();
    pending.current = controller;
    const active = () => pending.current === controller && !controller.signal.aborted;
    setBusy(true); setError("");
    try {
      const response = await fetch(id ? `/v1/connectors/cloudflare/connections/${encodeURIComponent(id)}` : "/v1/connectors/cloudflare", {
        method: id ? "DELETE" : "POST", credentials: "same-origin", redirect: "error",
        signal: AbortSignal.any([controller.signal, AbortSignal.timeout(30_000)]),
        headers: { "content-type": "application/json", accept: "application/json" },
        ...(id ? {} : { body: JSON.stringify({ vault_id: vaultId, ...(owner.trim() ? { account_id: owner.trim() } : {}) }) }),
      });
      await response.body?.cancel();
      if (!active()) return;
      if (response.status === 401) { await session.refresh(); return; }
      if ([400, 403, 404, 422].includes(response.status)) { setError("Couldn’t update Cloudflare. Check the selected key, account ID, and your access."); return; }
      if (!response.ok) throw new Error("Unconfirmed update");
      setOwner(""); setVaultId(""); setExpanded(false);
      await connections.refresh({ throwOnError: true });
    } catch {
      if (active()) { setUncertain(true); setError("The update could not be confirmed. Check the connection status before trying again."); }
    } finally {
      if (active()) { pending.current = null; setBusy(false); }
    }
  }

  async function refresh() {
    try {
      await connections.refresh({ throwOnError: true });
      await vault.refresh({ throwOnError: true });
      setUncertain(false); setError("");
    } catch { setError("Couldn’t load Cloudflare or Vault. Try checking again."); }
  }

  return <div className="account-service-row" data-provider="cloudflare" role="listitem">
    <div className="account-service-summary">
      <span className="connector-logo"><Cloud aria-hidden="true" /></span>
      <div className="account-service-copy"><strong>Cloudflare</strong><span>{connections.query.data?.length ? "Connected" : "Workers, storage, and account services"}</span></div>
      <button type="button" aria-expanded={expanded} disabled={busy || requiresLogin} onClick={() => setExpanded(value => !value)}>{expanded ? "Close" : connections.query.data?.length ? "Add account" : "Connect"}</button>
    </div>
    {expanded ? <form className="cloudflare-connection-form" onSubmit={event => { event.preventDefault(); void act(); }}>
      <label>Vault API key<select aria-label="Cloudflare Vault API key" value={vaultId} disabled={busy || requiresLogin} onChange={event => setVaultId(event.target.value)}><option value="">Choose an API key</option>{vault.query.data?.map(entry => <option key={entry.id} value={entry.id}>{entry.name}</option>)}</select></label>
      <label>Account ID (optional)<input aria-label="Cloudflare account ID" value={owner} pattern="[a-f0-9]{32}" disabled={busy || requiresLogin} onChange={event => setOwner(event.target.value)} /></label>
      <p>Account-owned tokens require an account ID. Leave blank for user tokens.</p>
      {vault.query.data?.length === 0 ? <p><a href="/account/vault?add=api_key">Save an API key in Vault</a> to connect.</p> : null}
      <button disabled={busy || uncertain || requiresLogin || !vault.query.data?.some(entry => entry.id === vaultId)} type="submit">{busy ? "Updating…" : "Connect Cloudflare"}</button>
    </form> : null}
    {connections.query.data?.map(connection => <div className="account-service-identity" key={connection.id}><div><strong>{connection.label}</strong><span>Cloudflare account</span></div><button disabled={busy || uncertain || requiresLogin} type="button" onClick={() => void act(connection.id)}>Revoke {connection.label}</button></div>)}
    {error || vault.query.error || connections.query.error ? <div className="cloudflare-connection-form" role="alert"><p>{error || "Couldn’t load Cloudflare or Vault."}</p><button type="button" disabled={busy} onClick={() => void refresh()}>Check status</button></div> : null}
  </div>;
}
