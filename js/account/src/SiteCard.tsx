import { Check, Copy, ExternalLink, Globe, Link2, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import type { ToolActivity } from "nanocodex-react/agent";
import "./SiteCard.css";

type PublishedSite = { site_id: string; title: string; version: number; files: number; bytes: number; excluded: number };
type SiteLink = { id: string; version: number; url: string; created_at: number; expires_at: number | null };

/** A site version the agent published with `publish_site`. Nothing is public until the owner creates a link. */
export function decodePublishedSite(tool: ToolActivity): PublishedSite | undefined {
  if (tool.name.split(".").at(-1) !== "publish_site" || tool.status !== "completed" || !tool.output) return;
  try {
    const value: unknown = JSON.parse(tool.output);
    if (!value || typeof value !== "object" || Array.isArray(value)) return;
    const site = value as Record<string, unknown>;
    if (site.type !== "nanocodex.site" || typeof site.site_id !== "string" || !/^[a-z0-9][a-z0-9-]{0,62}$/.test(site.site_id)
      || typeof site.title !== "string" || !Number.isSafeInteger(site.version) || (site.version as number) < 1
      || !Number.isSafeInteger(site.files) || !Number.isSafeInteger(site.bytes) || !Number.isSafeInteger(site.excluded)) return;
    return site as PublishedSite;
  } catch { return; }
}

export function SiteCard({ tool, agentId }: { tool: ToolActivity; agentId: string }) {
  const site = decodePublishedSite(tool);
  if (!site) return null;
  return <SiteVersionCard key={`${site.site_id}:${site.version}`} site={site} agentId={agentId} />;
}

function SiteVersionCard({ site, agentId }: { site: PublishedSite; agentId: string }) {
  const endpoint = `/v1/agents/${encodeURIComponent(agentId)}/sites/${encodeURIComponent(site.site_id)}`;
  const [links, setLinks] = useState<SiteLink[]>([]);
  const [created, setCreated] = useState<SiteLink>();
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    void fetch(`${endpoint}/shares`, { credentials: "same-origin", cache: "no-store", signal: controller.signal })
      .then(async response => response.ok ? (await response.json() as { data: SiteLink[] }).data : [])
      .then(data => setLinks(data.filter(link => link.version === site.version)))
      .catch(() => {});
    return () => controller.abort();
  }, [endpoint, site.version]);

  async function preview() {
    if (busy) return;
    // Open the tab inside the click so popup blockers allow it, then point it at the private host.
    const tab = window.open("about:blank", "_blank");
    if (tab) tab.opener = null;
    setBusy(true); setError("");
    try {
      const response = await fetch(`${endpoint}/open`, { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json" }, body: JSON.stringify({ version: site.version }) });
      if (!response.ok) throw new Error(await failure(response, "Couldn’t open the preview. Try again."));
      const { url } = await response.json() as { url: string };
      if (tab) tab.location.replace(url); else window.location.assign(url);
    } catch (cause) { tab?.close(); setError(message(cause)); }
    finally { setBusy(false); }
  }

  async function share() {
    if (busy) return;
    setBusy(true); setError(""); setCopied(false);
    try {
      const response = await fetch(`${endpoint}/shares`, { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json" }, body: JSON.stringify({ version: site.version }) });
      if (!response.ok) throw new Error(await failure(response, "Couldn’t create the link. Check your links before trying again."));
      const link = await response.json() as SiteLink;
      setLinks(current => [...current, link]);
      setCreated(link);
    } catch (cause) { setError(message(cause)); }
    finally { setBusy(false); }
  }

  async function revoke(id: string) {
    if (busy) return;
    setBusy(true); setError("");
    try {
      const response = await fetch(`${endpoint}/shares/${encodeURIComponent(id)}`, { method: "DELETE", credentials: "same-origin" });
      if (!response.ok && response.status !== 404) throw new Error("Couldn’t turn off the link. Try again.");
      setLinks(current => current.filter(link => link.id !== id));
      if (created?.id === id) setCreated(undefined);
    } catch (cause) { setError(message(cause)); }
    finally { setBusy(false); }
  }

  return <section className="vault-intake-card site-card" aria-label={`Published site ${site.title}`}>
    <header className="site-card-header">
      <Globe aria-hidden="true" />
      <div><strong>{site.title}</strong><p>Version {site.version} · {site.files} {site.files === 1 ? "file" : "files"} · {size(site.bytes)}</p></div>
    </header>
    {site.excluded ? <p>Skipped {site.excluded} {site.excluded === 1 ? "file that looked" : "files that looked"} like secrets or dependencies.</p> : null}
    <p>{links.length ? "Anyone with an active link below can open this version." : "Only you can see this version until you create a link."}</p>
    <div className="site-card-actions">
      <button type="button" disabled={busy} onClick={() => { void preview(); }}><ExternalLink aria-hidden="true" /> Open preview</button>
      <button type="button" disabled={busy} onClick={() => { void share(); }}><Link2 aria-hidden="true" /> Create public link</button>
    </div>
    {created ? <div className="site-card-created">
      <label htmlFor={`site-link-${created.id}`}>Public link</label>
      <div>
        <input id={`site-link-${created.id}`} readOnly value={created.url} onFocus={event => event.target.select()} />
        <button type="button" aria-label="Copy site link" onClick={() => { void navigator.clipboard.writeText(created.url).then(() => setCopied(true), () => setError("Couldn’t copy automatically. Select the link and copy it.")); }}>{copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}{copied ? "Copied" : "Copy"}</button>
      </div>
      <small>Search engines won’t index it. Later versions don’t change what this link shows.</small>
    </div> : null}
    {links.length ? <ul className="site-card-links" aria-label="Active links">
      {links.map(link => <li key={link.id}>
        <a href={link.url} target="_blank" rel="noopener noreferrer">{new URL(link.url).hostname.split(".")[0]!.slice(0, 8)}…</a>
        <small>Created {new Date(link.created_at).toLocaleDateString()}</small>
        <button type="button" disabled={busy} aria-label="Turn off link" onClick={() => { void revoke(link.id); }}><Trash2 aria-hidden="true" /> Turn off</button>
      </li>)}
    </ul> : null}
    {error ? <p role="alert">{error}</p> : null}
  </section>;
}

async function failure(response: Response, fallback: string): Promise<string> {
  try { return (await response.json() as { message?: string }).message ?? fallback; } catch { return fallback; }
}
function message(cause: unknown): string { return cause instanceof Error ? cause.message : "Something went wrong. Try again."; }
function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
