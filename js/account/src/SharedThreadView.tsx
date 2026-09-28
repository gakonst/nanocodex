import { ArrowUp, LockKeyhole, MessageCircle, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import "./ThreadSharing.css";

type SharedMetadata = { agent_id?: string; id?: string; title?: string; permission: "read" | "write" };
type SharedEvent = { cursor: string; type: "turn_accepted" | "turn_completed"; input?: string; final_message?: string };
type Comment = { id: string; input: string; createdAt?: string | number; created_at?: string | number };

// This route deliberately never opens the owner session or agent SDK. The fragment stays
// in the address bar for reload and bookmarking; it is never sent with HTTP or put in storage.
export function SharedThreadView({ agentId }: { agentId: string }) {
  const [token] = useState(() => {
    const fragment = new URLSearchParams(window.location.hash.slice(1));
    const value = fragment.get("token") ?? "";
    return /^nsl_[A-Za-z0-9_-]{43}$/.test(value) ? value : "";
  });
  const [meta, setMeta] = useState<SharedMetadata | null>(null);
  const [events, setEvents] = useState<SharedEvent[]>([]);
  const [comments, setComments] = useState<Comment[]>([]);
  const [olderCursor, setOlderCursor] = useState<string | null>(null);
  const [olderPending, setOlderPending] = useState(false);
  const historyExhausted = useRef(false);
  const [olderCommentsCursor, setOlderCommentsCursor] = useState<string | null>(null);
  const [olderCommentsPending, setOlderCommentsPending] = useState(false);
  const commentsExhausted = useRef(false);
  const [draft, setDraft] = useState("");
  const [pending, setPending] = useState(false);
  const pendingComment = useRef<{ id: string; input: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const base = `/v1/shared/${encodeURIComponent(agentId)}`;
  const read = useCallback(async (path: string, signal?: AbortSignal) => {
    const response = await fetch(base + path, { headers: { Authorization: `Bearer ${token}` }, credentials: "omit", cache: "no-store", signal });
    if (!response.ok) throw new Error(response.status === 403 || response.status === 404 ? "This link is invalid or has been revoked." : "The shared thread is unavailable. Try again.");
    return response.json() as Promise<unknown>;
  }, [base, token]);
  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (!token) { setError("This link is missing its access token."); setLoading(false); return; }
    setLoading(true); setError("");
    try {
      const metadata = await read("", signal) as SharedMetadata;
      const [history, annotations] = await Promise.all([read("/events/history", signal), read("/comments", signal)]) as [{ data: SharedEvent[]; has_more: boolean; next_cursor?: string }, { data: Comment[]; has_more?: boolean; next_cursor?: string }];
      if (signal?.aborted) return;
      setMeta(metadata);
      setEvents((previous) => {
        const next = Array.isArray(history.data) ? history.data : [];
        if (!previous.length) return next;
        const byCursor = new Map(previous.map((item) => [item.cursor, item]));
        for (const item of next) byCursor.set(item.cursor, item);
        return [...byCursor.values()].sort((a, b) => BigInt(a.cursor ?? "0") < BigInt(b.cursor ?? "0") ? -1 : 1);
      });
      setOlderCursor((current) => current ?? (!historyExhausted.current && history.has_more ? history.next_cursor ?? null : null));
      setComments((previous) => {
        const byId = new Map(previous.map((item) => [item.id, item]));
        for (const item of annotations.data ?? []) byId.set(item.id, item);
        return [...byId.values()].sort((a, b) => Number(a.created_at ?? a.createdAt ?? 0) - Number(b.created_at ?? b.createdAt ?? 0));
      });
      setOlderCommentsCursor((current) => current ?? (!commentsExhausted.current && annotations.has_more ? annotations.next_cursor ?? null : null));
    } catch (cause) {
      if (!signal?.aborted) { setMeta(null); setEvents([]); setComments([]); setOlderCursor(null); setOlderCommentsCursor(null); setError(cause instanceof Error ? cause.message : "Couldn’t open this thread."); }
    } finally { if (!signal?.aborted) setLoading(false); }
  }, [read, token]);
  useEffect(() => {
    const controller = new AbortController();
    void refresh(controller.signal);
    const timer = window.setInterval(() => { if (document.visibilityState === "visible") void refresh(controller.signal); }, 15_000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, [refresh]);

  function showGuestError(cause: unknown, fallback: string) {
    const message = cause instanceof Error ? cause.message : fallback;
    if (message === "This link is invalid or has been revoked." || message === "Comment access is no longer available.") {
      setMeta(null); setEvents([]); setComments([]); setOlderCursor(null); setOlderCommentsCursor(null);
    }
    setError(message);
  }

  async function loadOlder() {
    if (!olderCursor || olderPending) return;
    setOlderPending(true); setError("");
    try {
      const history = await read(`/events/history?before=${encodeURIComponent(olderCursor)}`) as { data: SharedEvent[]; has_more: boolean; next_cursor?: string };
      setEvents((current) => [...history.data, ...current]);
      if (!history.has_more) historyExhausted.current = true;
      setOlderCursor(history.has_more ? history.next_cursor ?? null : null);
    } catch (cause) { showGuestError(cause, "Couldn’t load earlier messages."); }
    finally { setOlderPending(false); }
  }

  async function loadOlderComments() {
    if (!olderCommentsCursor || olderCommentsPending) return;
    setOlderCommentsPending(true); setError("");
    try {
      const page = await read(`/comments?before=${encodeURIComponent(olderCommentsCursor)}`) as { data: Comment[]; has_more: boolean; next_cursor?: string };
      setComments((current) => [...page.data, ...current]);
      if (!page.has_more) commentsExhausted.current = true;
      setOlderCommentsCursor(page.has_more ? page.next_cursor ?? null : null);
    } catch (cause) { showGuestError(cause, "Couldn’t load earlier comments."); }
    finally { setOlderCommentsPending(false); }
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const input = draft.trim();
    if (!input || pending || meta?.permission !== "write") return;
    // Reuse the exact intended write after an uncertain response. Never issue a
    // second POST automatically; a user retry retains its ID and body.
    const candidate = pendingComment.current?.input === input
      ? pendingComment.current : { id: crypto.randomUUID(), input };
    pendingComment.current = candidate;
    setPending(true); setError("");
    const accept = (comment: Comment) => {
      setComments((current) => current.some((item) => item.id === comment.id)
        ? current : [...current, comment]);
      pendingComment.current = null;
      setDraft("");
    };
    try {
      const response = await fetch(`${base}/comments`, { method: "POST", credentials: "omit", headers: { Authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(candidate) });
      if (!response.ok) throw new Error(response.status === 403 || response.status === 404 ? "Comment access is no longer available." : "Couldn’t confirm your comment. Try again.");
      accept(await response.json() as Comment);
    } catch (cause) {
      // An accepted POST can lose its response. Check read-only state before
      // suggesting a manual retry; the next submit still uses this same ID.
      try {
        const page = await read("/comments") as { data: Comment[] };
        const confirmed = page.data.find((item) => item.id === candidate.id && item.input === candidate.input);
        if (confirmed) { accept(confirmed); return; }
      } catch { /* The original error is more useful than a second read error. */ }
      showGuestError(cause, "Couldn’t confirm your comment. Try again.");
    } finally { setPending(false); }
  }

  return <main className="shared-thread-page">
    <header className="shared-thread-header"><a href="/" rel="noreferrer" className="shared-thread-brand"><span className="paradigm-mark" aria-hidden="true" />Nanocodex</a><span><LockKeyhole aria-hidden="true" /> Shared thread</span></header>
    <div className="shared-thread-container">
      {loading && !meta ? <p role="status" className="shared-thread-state">Opening shared thread…</p> : null}
      {error && !meta ? <div role="alert" className="shared-thread-state"><h1>Can’t open this thread</h1><p>{error}</p><button type="button" onClick={() => { void refresh(); }}>Try again</button></div> : null}
      {meta ? <><div className="shared-thread-intro"><div><span className="shared-thread-eyebrow">Shared conversation · {meta.permission === "write" ? "comments enabled" : "view only"}</span><h1>{meta.title || "Shared thread"}</h1><p>You’re viewing a shared copy of this thread. Comments are separate from the agent and won’t start an AI turn.</p></div><button type="button" onClick={() => { void refresh(); }} disabled={loading} aria-label="Refresh shared thread"><RefreshCw aria-hidden="true" /> Refresh</button></div>
        {error ? <p className="shared-thread-error" role="alert">{error}</p> : null}
        <div className="shared-thread-timeline" aria-label="Thread messages">{olderCursor ? <button type="button" className="shared-thread-older" disabled={olderPending} onClick={() => { void loadOlder(); }}>{olderPending ? "Loading…" : "Load earlier messages"}</button> : null}{events.flatMap((event, index) => {
          const content = displayEvent(event);
          return content ? [<article className={`shared-thread-message is-${content.kind}`} key={event.cursor ?? index}><span className="shared-thread-author">{content.kind === "user" ? "You" : "Nanocodex"}</span><p>{content.text}</p></article>] : [];
        })}{!events.length ? <p className="shared-thread-empty">No messages have been shared yet.</p> : null}</div>
        <section className="shared-thread-comments" aria-label="Comments"><h2><MessageCircle aria-hidden="true" /> Comments <span>{comments.length}</span></h2>
          {olderCommentsCursor ? <button type="button" className="shared-thread-older" disabled={olderCommentsPending} onClick={() => { void loadOlderComments(); }}>{olderCommentsPending ? "Loading…" : "Load earlier comments"}</button> : null}
          {comments.length ? <ul>{comments.map((comment) => <li key={comment.id}><span>Guest comment</span><p>{comment.input}</p></li>)}</ul> : <p className="shared-thread-empty">No comments yet.</p>}
          {meta.permission === "write" ? <form onSubmit={submit}><label htmlFor="shared-thread-draft">Comment on this thread</label><textarea id="shared-thread-draft" aria-label="Comment on this thread" value={draft} maxLength={4000} disabled={pending} onChange={(event) => setDraft(event.target.value)} placeholder="Leave a comment for the thread owner…" /><div><small>Comments won’t be sent to the AI.</small><button type="submit" disabled={pending || !draft.trim()}><ArrowUp aria-hidden="true" />Post comment</button></div></form> : <p className="shared-thread-readonly"><LockKeyhole aria-hidden="true" /> This link is view only.</p>}
        </section>
      </> : null}
    </div>
  </main>;
}

function displayEvent(event: SharedEvent): { kind: "user" | "assistant"; text: string } | null {
  if (event.type === "turn_accepted" && typeof event.input === "string") return { kind: "user", text: event.input };
  if (event.type === "turn_completed" && typeof event.final_message === "string") return { kind: "assistant", text: event.final_message };
  return null;
}
