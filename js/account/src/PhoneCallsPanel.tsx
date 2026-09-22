import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { activePhoneCall, steerablePhoneCall, pollPhoneCalls, steeringOperation, type SteeringOperation } from "./phoneCalls";
import { pathForAgent } from "./navigation";
import "./PhoneCallsPanel.css";

type PhoneCall = { call_id: string; to?: string; status: string; call_agent_id?: string; transcript_truncated?: boolean; transcript?: { speaker: string; text: string }[] };
class PhoneRequestError extends Error {
  constructor(readonly status: number) { super(`Phone request failed (${status}).`); }
}
async function request<T>(url: string, init: RequestInit): Promise<T> {
  const response = await fetch(url, { credentials: "same-origin", cache: "no-store", redirect: "error", ...init });
  if (!response.ok) throw new PhoneRequestError(response.status);
  return response.json() as Promise<T>;
}
export function PhoneCallsPanel({ parentAgentId, enabled }: { parentAgentId: string; enabled: boolean }) {
  const [calls, setCalls] = useState<PhoneCall[]>([]);
  const hadCalls = useRef(false);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const base = `/v1/agents/${encodeURIComponent(parentAgentId)}/phone/calls`;
  useEffect(() => {
    let controller: AbortController | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    let unavailableForSession = false;
    const stop = () => { controller?.abort(); clearTimeout(timer); };
    const refresh = async () => {
      if (disposed || unavailableForSession || !pollPhoneCalls(enabled, document.visibilityState)) return;
      const current = new AbortController();
      controller = current;
      try {
        const data = await request<{ calls: PhoneCall[] }>(base, { signal: current.signal });
        if (!current.signal.aborted) {
          if (data.calls.length) hadCalls.current = true;
          setCalls(data.calls); setError("");
        }
      } catch (cause) {
        if (!current.signal.aborted) {
          const unavailable = cause instanceof PhoneRequestError && [403, 404].includes(cause.status);
          unavailableForSession = unavailable && !hadCalls.current;
          setError(unavailableForSession ? "" : cause instanceof Error ? cause.message : "Unable to load calls.");
        }
      } finally {
        if (!disposed && !unavailableForSession && !current.signal.aborted) timer = setTimeout(refresh, 4000);
      }
    };
    const visibility = () => { stop(); void refresh(); };
    document.addEventListener("visibilitychange", visibility);
    void refresh();
    return () => { disposed = true; stop(); document.removeEventListener("visibilitychange", visibility); };
  }, [base, enabled, revision]);
  if (!enabled || (!calls.length && !error)) return null;
  return <section className="phone-calls" aria-label="Phone calls">
    <header><strong>Phone calls</strong><span>{calls.length}</span></header>
    {error && <p role="alert">{error}</p>}
    {calls.map(call => <CallRow key={call.call_id} call={call} base={base} refresh={() => setRevision(value => value + 1)} />)}
  </section>;
}
function CallRow({ call, base, refresh }: { call: PhoneCall; base: string; refresh(): void }) {
  const [instructions, setInstructions] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const operation = useRef<SteeringOperation | undefined>(undefined);
  const pending = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const active = activePhoneCall(call.status);
  const steerable = steerablePhoneCall(call.status);
  async function act(action: "steer" | "hangup") {
    if (pending.current || !active) return;
    const text = instructions.trim();
    if (action === "steer" && (!steerable || !text)) return;
    if (action === "steer" && new TextEncoder().encode(text).length > 8000) { setError("Instructions must be at most 8,000 UTF-8 bytes."); return; }
    const body = action === "steer" ? (operation.current = steeringOperation(operation.current, text)) : {};
    pending.current = true; setBusy(true); setError(""); setNotice("");
    try {
      const receipt = await request<{ steering?: { status: string } }>(`${base}/${encodeURIComponent(call.call_id)}/${action}`, {
        method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
      });
      if (mounted.current) {
        const status = receipt.steering?.status;
        setNotice(action === "steer" ? `Instruction request: ${status ?? "received"}. Check the transcript for acknowledgement.` : "Hangup requested.");
        if (action === "steer" && status === "submitted") { operation.current = undefined; setInstructions(""); }
        refresh();
      }
    } catch (cause) {
      if (mounted.current) setError(`${cause instanceof Error ? cause.message : "Request failed."} The result may be uncertain. Check the call before retrying; unchanged instructions reuse the same request ID.`);
    } finally { pending.current = false; if (mounted.current) setBusy(false); }
  }
  return <details className="phone-call">
    <summary><strong>{call.to || "Phone call"}</strong><span>{call.status}</span></summary>
    {call.call_agent_id && <Link to={pathForAgent(call.call_agent_id)}>Open call agent</Link>}
    <div className="phone-call-transcript" aria-label="Call transcript">
      {call.transcript?.length ? call.transcript.map((entry, index) => <p key={index}><strong>{entry.speaker}: </strong>{entry.text}</p>) : <p>No transcript yet.</p>}
    </div>
    {call.transcript_truncated && <p>Transcript shortened. Open the call agent for more context.</p>}
    {active && <form onSubmit={event => { event.preventDefault(); void act("steer"); }}>
      {steerable && <label>Instructions for this call<textarea value={instructions} disabled={busy} onChange={event => setInstructions(event.target.value)} rows={2} maxLength={8000} /></label>}
      <div className="phone-call-actions">{steerable && <button disabled={busy || !instructions.trim()} type="submit">Send instructions</button>}<button disabled={busy} type="button" onClick={() => void act("hangup")}>Hang up</button></div>
    </form>}
    {notice && <p role="status">{notice}</p>}{error && <p role="alert">{error}</p>}
  </details>;
}
