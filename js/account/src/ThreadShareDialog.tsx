import { Check, Copy, Link2, Trash2, X } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import "./ThreadSharing.css";

type ShareLink = { id: string; permission: "read" | "write"; createdAt?: string; created_at?: number };
type CreatedLink = ShareLink & { url: string };

export function ThreadShareDialog({ agentId, onClose }: { agentId: string; onClose(): void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [links, setLinks] = useState<ShareLink[]>([]);
  const [comments, setComments] = useState<Array<{ id: string; input: string; created_at: number }>>([]);
  const [commentsError, setCommentsError] = useState("");
  const [olderCommentsCursor, setOlderCommentsCursor] = useState<string | null>(null);
  const [permission, setPermission] = useState<"read" | "write">("read");
  const [created, setCreated] = useState<CreatedLink | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const endpoint = `/v1/agents/${encodeURIComponent(agentId)}/share-links`;
  const refreshComments = async (signal?: AbortSignal, before?: string) => {
    try {
      const response = await fetch(`/v1/agents/${encodeURIComponent(agentId)}/share-comments${before ? `?before=${encodeURIComponent(before)}` : ""}`, { credentials: "same-origin", cache: "no-store", signal });
      if (!response.ok) throw new Error("Couldn’t load guest comments.");
      const result = await response.json() as { data: typeof comments; has_more?: boolean; next_cursor?: string };
      if (!signal?.aborted) {
        setComments((current) => before ? [...result.data, ...current] : result.data);
        setOlderCommentsCursor(result.has_more ? result.next_cursor ?? null : null);
        setCommentsError("");
      }
    } catch (cause) { if (!signal?.aborted) setCommentsError(errorText(cause)); }
  };
  useEffect(() => {
    const node = dialog.current;
    node?.showModal();
    const controller = new AbortController();
    void refreshComments(controller.signal);
    void fetch(endpoint, { credentials: "same-origin", cache: "no-store", signal: controller.signal })
      .then(async (response) => {
        if (!response.ok) throw new Error("Couldn’t load this thread’s links.");
        return response.json() as Promise<{ data: ShareLink[] }>;
      })
      .then(({ data }) => setLinks(data))
      .catch((cause: unknown) => { if (!controller.signal.aborted) setError(errorText(cause)); });
    return () => { controller.abort(); node?.close(); };
  }, [endpoint]);

  async function create(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    setBusy(true); setError(""); setCreated(null); setCopied(false);
    try {
      const response = await fetch(endpoint, { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json" }, body: JSON.stringify({ permission }) });
      if (!response.ok) throw new Error("Couldn’t create the link. Please try again.");
      const link = await response.json() as CreatedLink;
      setLinks((current) => [link, ...current]);
      setCreated(link);
    } catch (cause) { setError(errorText(cause)); }
    finally { setBusy(false); }
  }

  async function revoke(linkId: string) {
    if (busy) return;
    setBusy(true); setError("");
    try {
      const response = await fetch(`${endpoint}/${encodeURIComponent(linkId)}`, { method: "DELETE", credentials: "same-origin" });
      if (!response.ok) throw new Error("Couldn’t revoke the link. Please try again.");
      setLinks((current) => current.filter(({ id }) => id !== linkId));
      if (created?.id === linkId) setCreated(null);
    } catch (cause) { setError(errorText(cause)); }
    finally { setBusy(false); }
  }

  return <dialog ref={dialog} className="thread-share-dialog" aria-label="Share thread"
    onCancel={(event) => { event.preventDefault(); onClose(); }}
    onClose={onClose}
    onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <div className="thread-share-panel">
      <header><div className="thread-share-icon"><Link2 aria-hidden="true" /></div>
        <div><h2>Share this thread</h2><p>Invite someone to follow this conversation without sharing your account.</p></div>
        <button type="button" className="thread-share-close" aria-label="Close sharing" onClick={onClose}><X aria-hidden="true" /></button>
      </header>
      <form onSubmit={create}>
        <label htmlFor="thread-share-permission">Access</label>
        <div className="thread-share-create"><select id="thread-share-permission" value={permission} onChange={(event) => setPermission(event.target.value as "read" | "write")}>
          <option value="read">Can view</option><option value="write">Can view and comment</option>
        </select><button type="submit" disabled={busy}>Create {permission === "read" ? "view" : "comment"} link</button></div>
        <p className="thread-share-help">Anyone with this link can {permission === "read" ? "read the shared messages" : "read and leave comments"}. Comments never send a message to the AI.</p>
      </form>
      {created ? <div className="thread-share-created"><label htmlFor="thread-share-new-link">New share link · copy it now</label>
        <div><input id="thread-share-new-link" aria-label="New share link" readOnly value={created.url} onFocus={(event) => event.target.select()} />
          <button type="button" aria-label="Copy share link" onClick={() => { void navigator.clipboard.writeText(created.url).then(() => setCopied(true), () => setError("Couldn’t copy automatically. Select the link and copy it.")); }}><span>{copied ? "Copied" : "Copy"}</span>{copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}</button></div>
        <small>For your privacy, the full link won’t appear again after you close this dialog.</small></div> : null}
      {error ? <p className="thread-share-error" role="alert">{error}</p> : null}
      <section aria-label="Active links" className="thread-share-active"><h3>Active links</h3>
        {links.length ? <ul>{links.map((link) => <li key={link.id}><span><Link2 aria-hidden="true" /><span><strong>{link.permission === "write" ? "Can view and comment" : "Can view"}</strong><small>{dateLabel(link.createdAt ?? link.created_at)}</small></span></span><button type="button" aria-label="Revoke link" disabled={busy} onClick={() => { void revoke(link.id); }}><Trash2 aria-hidden="true" /> Revoke</button></li>)}</ul>
          : <p>No active links yet.</p>}
      </section>
      <section aria-label="Guest comments" className="thread-share-comments"><div><h3>Guest comments <span>{comments.length}</span></h3><button type="button" onClick={() => { void refreshComments(); }}>Refresh</button></div>
        {commentsError ? <p role="alert" className="thread-share-error">{commentsError}</p> : null}
        {olderCommentsCursor ? <button type="button" className="thread-share-older" onClick={() => { void refreshComments(undefined, olderCommentsCursor); }}>Load earlier comments</button> : null}
        {comments.length ? <ul>{comments.map((comment) => <li key={comment.id}><small>Guest · {dateLabel(new Date(comment.created_at).toISOString())}</small><p>{comment.input}</p></li>)}</ul> : <p>No guest comments yet.</p>}
      </section>
    </div>
  </dialog>;
}

function dateLabel(value?: string | number) {
  if (!value) return "Shared link";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "Shared link" : `Created ${date.toLocaleDateString()}`;
}
function errorText(cause: unknown) { return cause instanceof Error ? cause.message : "Something went wrong. Please try again."; }
