import { ArrowLeft, LockKeyhole, MessageCircle, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Agent, AgentEvent } from "nanocodex-react/agent";
import { TerminalComposer } from "nanocodex-connect-embed";
import { AgentConversation } from "nanocodex-connect-embed";
import "nanocodex-connect-embed/conversation.css";
import "./AgentTerminal.css";
import "./Home.css";
import "./ThreadSharing.css";

type SharedMetadata = { agent_id?: string; title?: string; permission: "read" | "write"; latest_event_cursor?: string };
type SharedEvent = { cursor: string; type: "turn_accepted" | "turn_completed" | "event" | "assistant_delta" | "turn_failed" | "turn_cancelled"; turn_id?: string | null; id?: string; input?: string; final_message?: string; delta?: string; author?: string; event?: AgentEvent; agent_id?: number };
type PendingTurn = { id: string; input: string };
const revokedMessage = "This link is invalid or has been revoked.";
const after = (a: string, b: string) => BigInt(a) > BigInt(b);
const byCursor = (a: { cursor: string }, b: { cursor: string }) => BigInt(a.cursor) < BigInt(b.cursor) ? -1 : BigInt(a.cursor) > BigInt(b.cursor) ? 1 : 0;

// Guest access is isolated from account cookies and owner sessions.
// The fragment bearer never enters a URL request, browser storage, telemetry or a tool call.
export function SharedThreadView({ agentId }: { agentId: string }) {
  const [token] = useState(() => {
    const value = new URLSearchParams(window.location.hash.slice(1)).get("token") ?? "";
    return /^nsl_[A-Za-z0-9_-]{43}$/.test(value) ? value : "";
  });
  const base = `/v1/shared/${encodeURIComponent(agentId)}`;
  const [meta, setMeta] = useState<SharedMetadata | null>(null);
  const [events, setEvents] = useState<SharedEvent[]>([]);
  const [optimistic, setOptimistic] = useState<PendingTurn[]>([]);
  const eventListeners = useRef(new Set<(event: AgentEvent) => void>());
  const historyListeners = useRef(new Set<(events: readonly AgentEvent[]) => void>());
  const historySnapshot = useRef<readonly AgentEvent[]>([]);
  const rawAssistantTurns = useRef(new Set<string>());
  const optimisticIds = useRef(new Set<string>());
  const loadOlderRef = useRef<() => Promise<boolean>>(async () => false);
  const [olderCursor, setOlderCursor] = useState<string | null>(null);
  const [olderPending, setOlderPending] = useState(false);
  const historyExhausted = useRef(false);
  const streamCursor = useRef<string | null>(null);
  const accessEpoch = useRef(0);
  const [draft, setDraft] = useState("");
  const [pending, setPending] = useState(false);
  const pendingTurn = useRef<PendingTurn | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const read = useCallback(async (path: string, signal?: AbortSignal) => {
    const response = await fetch(base + path, { headers: { Authorization: `Bearer ${token}` }, credentials: "omit", cache: "no-store", signal });
    if (!response.ok) throw new Error(response.status === 403 || response.status === 404 ? revokedMessage : "The shared thread is unavailable. Try again.");
    return response.json() as Promise<unknown>;
  }, [base, token]);
  const invalidate = useCallback((message = revokedMessage) => {
    // Ignore any earlier history requests that complete after a revoke.
    accessEpoch.current++;
    setLoading(false); setPending(false); setOlderPending(false);
    setMeta(null); setEvents([]); setOptimistic([]); optimisticIds.current.clear(); setOlderCursor(null); pendingTurn.current = null; setError(message);
  }, []);
  const refresh = useCallback(async (signal?: AbortSignal) => {
    const epoch = accessEpoch.current;
    if (!token) { invalidate("This link is missing its access token."); setLoading(false); return; }
    setLoading(true); setError("");
    try {
      const metadata = await read("", signal) as SharedMetadata;
      const history = await read("/events/history", signal) as { data: SharedEvent[]; has_more: boolean; next_cursor?: string };
      if (signal?.aborted || epoch !== accessEpoch.current) return;
      streamCursor.current ??= metadata.latest_event_cursor ?? "0";
      setMeta(metadata);
      setEvents((previous) => mergeEvents(previous, Array.isArray(history.data) ? history.data : []));
      setOlderCursor((current) => current ?? (!historyExhausted.current && history.has_more ? history.next_cursor ?? null : null));
    } catch (cause) {
      if (!signal?.aborted && epoch === accessEpoch.current) {
        if (cause instanceof Error && cause.message === revokedMessage) invalidate();
        else setError(cause instanceof Error ? cause.message : "Couldn’t open this thread.");
      }
    } finally { if (!signal?.aborted && epoch === accessEpoch.current) setLoading(false); }
  }, [read, token, invalidate]);
  useEffect(() => {
    const controller = new AbortController();
    void refresh(controller.signal);
    const timer = window.setInterval(() => { if (document.visibilityState === "visible") void refresh(controller.signal); }, 15_000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, [refresh]);

  // Authorization headers cannot be supplied to EventSource. Consume a fetch SSE
  // response instead, preserving the server cursor across reconnects and tab sleep.
  useEffect(() => {
    if (!meta || !token) return;
    const controller = new AbortController();
    const epoch = accessEpoch.current;
    let active = true;
    const connect = async () => {
      let delay = 500;
      while (active && !controller.signal.aborted && epoch === accessEpoch.current) {
        try {
          const cursor = streamCursor.current ?? meta.latest_event_cursor ?? "0";
          const response = await fetch(`${base}/events?after=${encodeURIComponent(cursor)}`, {
            headers: { Authorization: `Bearer ${token}`, accept: "text/event-stream" },
            credentials: "omit", cache: "no-store", signal: controller.signal,
          });
          if (response.status === 403 || response.status === 404) { invalidate(); return; }
          if (!response.ok || !response.body) throw new Error("The live feed is temporarily unavailable.");
          delay = 500;
          const reader = response.body.getReader();
          const decoder = new TextDecoder();
          let buffer = "";
          while (active && epoch === accessEpoch.current) {
            const chunk = await reader.read();
            if (chunk.done || epoch !== accessEpoch.current) break;
            buffer += decoder.decode(chunk.value, { stream: true });
            // Keep a malformed or unbounded SSE frame from accumulating forever.
            if (buffer.length > 1_000_000) throw new Error("The live feed frame was too large.");
            let boundary: number;
            while (epoch === accessEpoch.current && (boundary = buffer.indexOf("\n\n")) !== -1) {
              const frame = buffer.slice(0, boundary).replaceAll("\r", "");
              buffer = buffer.slice(boundary + 2);
              const data = frame.split("\n").filter((line) => line.startsWith("data: ")).map((line) => line.slice(6)).join("\n");
              if (!data) continue;
              const event = JSON.parse(data) as SharedEvent;
              if (typeof event.cursor !== "string" || !/^\d+$/.test(event.cursor) || !after(event.cursor, streamCursor.current ?? "0")) continue;
              streamCursor.current = event.cursor;
              if (event.type === "event" && event.event?.type === "assistant.message" && event.turn_id)
                rawAssistantTurns.current.add(event.turn_id);
              // History reconciliation replaces an optimistic prompt with the
              // durable event; streaming it too would briefly display two rows.
              if (!(event.type === "turn_accepted" && event.id && optimisticIds.current.has(event.id)))
                for (const sdk of projectEvents(event, agentId, rawAssistantTurns.current))
                  for (const listener of eventListeners.current) listener(sdk);
              if (event.type === "turn_accepted" || event.type === "turn_completed" || event.type === "event" || event.type === "turn_failed" || event.type === "turn_cancelled")
                setEvents((current) => mergeEvents(current, [event]));
            }
          }
          if (active && !controller.signal.aborted && epoch === accessEpoch.current) {
            // A revoked link actively closes its stream. Recheck before retrying,
            // rather than leaving the last private transcript visible indefinitely.
            await read("", controller.signal);
          }
        } catch (cause) {
          if (!active || controller.signal.aborted || epoch !== accessEpoch.current) return;
          if (cause instanceof Error && cause.message === revokedMessage) { invalidate(); return; }
          await new Promise((resolve) => window.setTimeout(resolve, delay));
          delay = Math.min(delay * 2, 10_000);
        }
      }
    };
    void connect();
    return () => { active = false; controller.abort(); };
  }, [base, token, Boolean(meta), read, invalidate, agentId]); // eslint-disable-line react-hooks/exhaustive-deps

  function showGuestError(cause: unknown, fallback: string) {
    const message = cause instanceof Error ? cause.message : fallback;
    if (message === revokedMessage) invalidate(message);
    else setError(message);
  }
  async function loadOlder(): Promise<boolean> {
    if (!olderCursor || olderPending) return false;
    const epoch = accessEpoch.current;
    setOlderPending(true); setError("");
    try {
      const page = await read(`/events/history?before=${encodeURIComponent(olderCursor)}`) as { data: SharedEvent[]; has_more: boolean; next_cursor?: string };
      if (epoch !== accessEpoch.current) return false;
      setEvents((current) => mergeEvents(current, page.data));
      if (!page.has_more) historyExhausted.current = true;
      setOlderCursor(page.has_more ? page.next_cursor ?? null : null);
      return true;
    } catch (cause) { if (epoch === accessEpoch.current) showGuestError(cause, "Couldn’t load earlier messages."); return false; }
    finally { setOlderPending(false); }
  }
  loadOlderRef.current = loadOlder;
  const history = useMemo(() => projectHistory(events, optimistic, agentId), [events, optimistic, agentId]);
  useEffect(() => {
    historySnapshot.current = history;
    for (const listener of historyListeners.current) listener(history);
  }, [history]);
  const agent = useMemo<Agent>(() => ({
    sessionId: agentId,
    events: { watch: () => ({
      onEvent(listener) { eventListeners.current.add(listener); return () => eventListeners.current.delete(listener); },
      onHistory(listener) { historyListeners.current.add(listener); listener(historySnapshot.current); return () => historyListeners.current.delete(listener); },
      loadOlder: () => loadOlderRef.current(),
      off() {},
    }) },
    turn: { prompt: ({ input, id: providedId }: { input: string; id?: string }) => {
      const id = providedId ?? crypto.randomUUID();
      const response = fetch(`${base}/turns`, { method: "POST", credentials: "omit", cache: "no-store",
        headers: { Authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify({ id, input }) });
      return { historyEntryId: `managed-user-${id}`, steer: async () => { throw Error("Steering is unavailable on shared links."); },
        cancel: async () => { throw Error("Cancellation is unavailable on shared links."); },
        result: async () => { const result = await response; if (!result.ok) throw Error(result.status === 403 || result.status === 404 ? revokedMessage : "Couldn’t confirm your message. Send again to retry."); return { finalMessage: "", dispose() {} }; },
        dispose() {}, };
    } },
  }), [agentId, base, token]);
  async function submit(value: string) {
    const input = value.trim();
    if (!input || pending || meta?.permission !== "write") return;
    const candidate = pendingTurn.current?.input === input ? pendingTurn.current : { id: crypto.randomUUID(), input };
    pendingTurn.current = candidate;
    optimisticIds.current.add(candidate.id);
    setOptimistic((current) => current.some((turn) => turn.id === candidate.id) ? current : [...current, candidate]);
    const epoch = accessEpoch.current;
    setPending(true); setError("");
    try {
      // The guest adapter uses the same turn.prompt contract as the owned terminal.
      const turn = agent.turn.prompt({ input, id: candidate.id } as { input: string; id: string });
      await turn.result();
      if (epoch !== accessEpoch.current) return;
      pendingTurn.current = null;
      setDraft("");
      void refresh();
    } catch (cause) {
      if (epoch === accessEpoch.current) showGuestError(cause, "Couldn’t confirm your message. Send again to retry.");
    } finally { if (epoch === accessEpoch.current) setPending(false); }
  }
  const composer = meta?.permission === "write"
    ? <TerminalComposer draft={draft} onChange={setDraft} onSubmit={(value) => { void submit(value); }}
        onCancel={() => {}} pending={pending} running={false} status="ready" placeholder="Message Nanocodex…" />
    : <p className="shared-thread-readonly"><LockKeyhole aria-hidden="true" /> This link is view only.</p>;

  return <main className="nanocodex-demo chat-workspace is-full shared-chat-workspace">
    <div className="conversation-workspace">
      <aside className="shared-chat-sidebar" aria-label="Shared conversation"><a href="/" className="shared-thread-brand"><span className="paradigm-mark" aria-hidden="true" /> Nanocodex</a>
        <div className="shared-chat-sidebar-thread"><MessageCircle aria-hidden="true" /><span>{meta?.title || "Shared thread"}</span></div>
        <p><LockKeyhole aria-hidden="true" /> {meta?.permission === "write" ? "Can send messages" : "View only"}</p>
      </aside>
      <div className="conversation-main">
        <header className="agent-chat-header"><a href="/" aria-label="Nanocodex home" className="shared-chat-back"><ArrowLeft aria-hidden="true" /></a>
          <div className="agent-chat-heading"><strong>{meta?.title || "Shared thread"}</strong><span>Shared conversation · {meta?.permission === "write" ? "can send messages" : "view only"}</span></div>
          <div className="agent-chat-header-actions">
            {olderCursor ? <button type="button" className="shared-thread-older" disabled={olderPending} aria-label="Load earlier messages" onClick={() => { void loadOlder(); }}><span className="shared-chat-older-wide">Load earlier messages</span><span className="shared-chat-older-short">Earlier</span></button> : null}
            <button type="button" className="chat-icon-button" onClick={() => { void refresh(); }} disabled={loading} aria-label="Refresh shared thread" title="Refresh shared thread"><RefreshCw aria-hidden="true" /></button>
          </div>
        </header>
        {loading && !meta ? <p role="status" className="shared-thread-state">Opening shared thread…</p> : null}
        {error && !meta ? <div role="alert" className="shared-thread-state"><h1>Can’t open this thread</h1><p>{error}</p><button type="button" onClick={() => { void refresh(); }}>Try again</button></div> : null}
        {meta ? <><div className="shared-chat-boundary"><LockKeyhole aria-hidden="true" /> {meta.permission === "write" ? "You’re in a shared conversation. Messages you send start a real AI turn." : "You’re viewing this shared conversation."}</div>
          {error ? <p className="shared-thread-error" role="alert">{error}</p> : null}
          <AgentConversation agent={agent} agentError={undefined} mode="full" voice={false}
            onConversationActivity={() => {}} onStateChange={() => {}} retryAgent={() => { void refresh(); }}
            composer={composer} welcome={loading ? "Opening shared thread…" : "No messages have been shared yet."} />
        </> : null}
      </div>
    </div>
  </main>;
}

function projectEvents(row: SharedEvent, sessionId: string, rawAssistantTurns: Set<string>): AgentEvent[] {
  const turnId = row.turn_id ?? row.id;
  const cursor = Number(row.cursor);
  if (!Number.isSafeInteger(cursor) || cursor > Math.floor(Number.MAX_SAFE_INTEGER / 4)) return [];
  const seq = cursor * 4;
  const payload = turnId ? { turn_id: turnId } : {};
  if (row.type === "event" && row.event && typeof row.event.type === "string")
    return [{ ...row.event, request_id: sessionId, seq, payload: { ...row.event.payload, ...payload,
      ...(row.agent_id === undefined ? {} : { managed_agent_id: row.agent_id }) } }];
  if (row.type === "turn_accepted" && typeof row.input === "string")
    return [{ request_id: sessionId, seq, type: "managed.prompt", payload: { ...payload, text: row.input,
      ...(row.author === "guest" ? { author: "guest" } : {}) } }];
  if (row.type === "assistant_delta" && typeof row.delta === "string")
    return [{ request_id: sessionId, seq, type: "assistant.delta", payload: { ...payload, text: row.delta } }];
  if (row.type === "turn_completed" && typeof row.final_message === "string") {
    const completion: AgentEvent = { request_id: sessionId, seq: seq + 1, type: "run.completed", payload: { ...payload, status: "completed", disposition: "completed" } };
    return rawAssistantTurns.has(turnId ?? "") ? [completion] : [
      { request_id: sessionId, seq, type: "assistant.message", payload: { ...payload, text: row.final_message } }, completion,
    ];
  }
  if (row.type === "turn_failed" || row.type === "turn_cancelled")
    return [{ request_id: sessionId, seq, type: "run.failed", payload: { ...payload, message: "Turn unavailable" } }];
  return [];
}
function projectHistory(rows: SharedEvent[], optimistic: PendingTurn[], sessionId: string): readonly AgentEvent[] {
  const accepted = new Set(rows.filter((row) => row.type === "turn_accepted").map((row) => row.turn_id ?? row.id));
  const rawAssistantTurns = new Set(rows.filter((row) => row.type === "event" && row.event?.type === "assistant.message")
    .map((row) => row.turn_id).filter((id): id is string => typeof id === "string"));
  const projected = rows.flatMap((row) => projectEvents(row, sessionId, rawAssistantTurns));
  for (const turn of optimistic) if (!accepted.has(turn.id)) projected.push({
    request_id: sessionId, seq: Number.MAX_SAFE_INTEGER - optimistic.length + optimistic.indexOf(turn),
    type: "managed.prompt", payload: { turn_id: turn.id, text: turn.input, author: "guest" },
  });
  return projected;
}
function mergeEvents(previous: SharedEvent[], incoming: SharedEvent[]) {
  const unique = new Map(previous.map((item) => [item.cursor, item]));
  for (const event of incoming) if (event && typeof event.cursor === "string" && /^\d+$/.test(event.cursor)
    && (event.type === "turn_accepted" || event.type === "turn_completed" || event.type === "event" || event.type === "turn_failed" || event.type === "turn_cancelled")) unique.set(event.cursor, event);
  return [...unique.values()].sort(byCursor);
}
