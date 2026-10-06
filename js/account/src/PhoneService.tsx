import "./DeviceConnect.css";
import "./PhoneService.css";
import { useCallback, useEffect, useRef, useState } from "react";
import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import { useAccountSession } from "./AccountSession";
import { completePhoneApproval, enrollmentTarget } from "./serviceEnrollment";

type NumberInfo = { id: string; phone_number: string; status: "active" | "release_pending" | "released"; country: string };
type Available = { phone_number: string; country: string; type: string };
type Message = { id: string; from: string; body: string; received_at: string; expires_at: string };
type Quote = { id: string; currency: string; monthly_price: string; inbound_sms_price: string; recurring: true; expires_at: string };
type PhoneRequest = { operation_id: string; kind: "purchase" | "release"; status: "pending_approval" | "complete" | "denied" | "expired" | "failed" | "outcome_unknown"; phone_number: string; number_id?: string; quote?: Quote };
const base = "/v1/services/phone";
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const record = (value: unknown): value is Record<string, unknown> => Boolean(value) && typeof value === "object" && !Array.isArray(value);
const phone = (value: unknown): value is string => typeof value === "string" && /^\+[1-9][0-9]{7,14}$/.test(value);
const date = (value: unknown): value is string => typeof value === "string" && Number.isFinite(Date.parse(value));

class ServiceError extends Error { constructor(readonly status: number) { super(status === 401 ? "Sign in again to continue." : status === 403 ? "Your account cannot perform this action." : status === 503 ? "Phone service is unavailable. Check again later." : "The request could not be completed."); } }
async function request(path: string, method = "GET", body?: unknown): Promise<unknown> {
  const response = await fetch(base + path, {method, credentials: "same-origin", cache: "no-store", redirect: "error", referrerPolicy: "no-referrer", headers: {accept: "application/json", ...(body === undefined ? {} : {"content-type": "application/json"})}, ...(body === undefined ? {} : {body: JSON.stringify(body)})});
  if (!response.ok) { await response.body?.cancel(); throw new ServiceError(response.status); }
  return response.json();
}
function operation(value: unknown, expected: string): PhoneRequest {
  if (!record(value) || !record(value.request)) throw new Error("Invalid phone request response.");
  const item = value.request;
  if (item.operation_id !== expected || !uuid.test(expected) || !["purchase", "release"].includes(String(item.kind))
    || !["pending_approval", "complete", "denied", "expired", "failed", "outcome_unknown"].includes(String(item.status)) || !phone(item.phone_number)
    || (item.number_id !== undefined && (typeof item.number_id !== "string" || !uuid.test(item.number_id)))) throw new Error("Invalid phone request response.");
  if (item.quote !== undefined) {
    const quote = item.quote;
    if (!record(quote) || typeof quote.id !== "string" || !uuid.test(quote.id) || typeof quote.currency !== "string" || !/^[a-zA-Z]{3}$/.test(quote.currency)
      || typeof quote.monthly_price !== "string" || !/^\d+(\.\d{1,8})?$/.test(quote.monthly_price)
      || typeof quote.inbound_sms_price !== "string" || !/^\d+(\.\d{1,8})?$/.test(quote.inbound_sms_price)
      || quote.recurring !== true || !date(quote.expires_at)) throw new Error("Invalid phone quote response.");
  }
  if (item.kind === "purchase" && item.status === "pending_approval" && !item.quote) throw new Error("The request is missing its price quote.");
  return item as PhoneRequest;
}
function numbers(value: unknown): NumberInfo[] {
  if (!record(value) || !Array.isArray(value.numbers) || !value.numbers.every(item => record(item) && typeof item.id === "string" && uuid.test(item.id) && phone(item.phone_number) && ["active", "release_pending", "released"].includes(String(item.status)))) throw new Error("Invalid phone number response.");
  return value.numbers as NumberInfo[];
}
const terminal = (status: PhoneRequest["status"]) => ["complete", "denied", "expired", "failed"].includes(status);
const statusLabel = (item: PhoneRequest) => item.status === "complete" ? item.kind === "purchase" ? "Number activated" : "Number released" : item.status === "pending_approval" ? "Awaiting your approval" : item.status === "outcome_unknown" ? "Outcome unknown" : item.status === "denied" ? "Request denied" : item.status === "expired" ? "Quote expired" : "Request failed";

export function PhoneService() {
  const session = useAccountSession();
  if (window.top !== window) return <div className="vault-page"><h1>Open phone services in a secure window</h1><p>Phone requests and approvals are available only in a top-level window. Use the app’s approval button to open a secure popup.</p></div>;
  if (session.status === "checking") return null;
  if (!session.account?.persistent) return <div className="wizard-page wizard-account-page"><h1>Phone numbers</h1><AccountChooser description="Sign in to manage your dedicated numbers and incoming messages." disabled={session.operation !== null} failure={session.error} onChooseAccount={selection => void session.chooseAccount(selection)} /></div>;
  return <PhoneAccount key={session.account.id} accountId={session.account.id} />;
}

function PhoneAccount({accountId}: {accountId: string}) {
  const storageKey = `nanocodex:phone-request:${accountId}`;
  const [owned, setOwned] = useState<NumberInfo[]>([]);
  const [available, setAvailable] = useState<Available[] | null>(null);
  const [area, setArea] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [pendingId, setPendingId] = useState(() => {
    const queryId = new URLSearchParams(location.search).get("operation_id");
    const savedId = window.sessionStorage.getItem(storageKey);
    const id = queryId ?? savedId;
    return id && uuid.test(id) ? id : "";
  });
  const [pending, setPending] = useState<PhoneRequest>();
  const [uncertain, setUncertain] = useState(false);
  const [accepted, setAccepted] = useState(false);
  const [shareCompletion, setShareCompletion] = useState(false);
  const callbackTarget = enrollmentTarget();
  const [now, setNow] = useState(Date.now());
  const [inbox, setInbox] = useState<string>();
  const [messages, setMessages] = useState<Message[]>([]);
  const [cursor, setCursor] = useState<string>();
  const alive = useRef(true), action = useRef(false);
  const inboxRef = useRef<string | undefined>(undefined);
  const callbackId = useRef("");
  useEffect(() => { alive.current = true; const timer = window.setInterval(() => setNow(Date.now()), 1000); return () => { alive.current = false; clearInterval(timer); }; }, []);
  const load = useCallback(async () => { const result = numbers(await request("/numbers")); if (alive.current) setOwned(result); }, []);
  const observe = useCallback((item: PhoneRequest) => {
    if (!alive.current) return;
    setPending(item); setAccepted(false); setUncertain(item.status === "outcome_unknown");
    if (terminal(item.status)) {
      window.sessionStorage.removeItem(storageKey);
      void load().catch(() => { if (alive.current) setError("Couldn’t refresh your numbers. Refresh the list to confirm its current state."); });
    }
  }, [load, storageKey]);
  useEffect(() => { void load().catch(cause => { if (alive.current) setError(cause instanceof ServiceError ? cause.message : "Couldn’t load your numbers."); }); }, [load]);
  const initialId = useRef(pendingId).current;
  useEffect(() => {
    if (!initialId) return;
    let cancelled = false;
    void request(`/requests/${initialId}`).then(value => { if (!cancelled) observe(operation(value, initialId)); }).catch(() => { if (!cancelled && alive.current) {setUncertain(true); setError("Couldn’t retrieve this request. Check its status before starting another request.");} });
    return () => { cancelled = true; };
  }, [initialId, observe]);
  async function run(work: () => Promise<void>) {
    if (action.current) return;
    action.current = true; setBusy(true); setError("");
    try { await work(); } catch (cause) { if (alive.current) setError(cause instanceof ServiceError ? cause.message : "Couldn’t complete the request. Check its status before trying again."); }
    finally { action.current = false; if (alive.current) setBusy(false); }
  }
  const locked = Boolean(pendingId && (!pending || !terminal(pending.status)));
  async function stage(selected: Available | NumberInfo) {
    const id = crypto.randomUUID();
    // Persist the operation fence before sending any mutating request.
    window.sessionStorage.setItem(storageKey, id); setPending(undefined); setPendingId(id); setAccepted(false); setUncertain(false);
    try {
      const result = "id" in selected ? await request(`/numbers/${selected.id}`, "DELETE", {operation_id: id}) : await request("/numbers", "POST", {operation_id: id, phone_number: selected.phone_number, country: "US"});
      observe(operation(result, id));
    } catch (cause) {
      if (alive.current) {
        if (cause instanceof ServiceError && [400, 401, 403, 404, 409, 422, 429].includes(cause.status)) { window.sessionStorage.removeItem(storageKey); setPendingId(""); }
        else setUncertain(true);
      }
      throw cause;
    }
  }
  function share(item: PhoneRequest) {
    if (terminal(item.status) && callbackId.current !== item.operation_id + item.status) { completePhoneApproval(item.operation_id, item.status); callbackId.current = item.operation_id + item.status; }
  }
  async function decide(approve: boolean) {
    if (!pending || pending.status !== "pending_approval" || uncertain) return;
    try {
      const item = operation(await request(`/requests/${pending.operation_id}/${approve ? "approve" : "deny"}`, "POST", !approve ? {} : pending.kind === "purchase" ? {quote_id: pending.quote!.id, accept_recurring: true} : {confirm_release: true}), pending.operation_id);
      observe(item); if (shareCompletion) share(item);
    }
    catch (cause) { if (alive.current) setUncertain(true); throw cause; }
  }
  async function readInbox(id: string, next?: string) {
    inboxRef.current = id; setInbox(id);
    if (!next) { setMessages([]); setCursor(undefined); }
    const result = await request(`/numbers/${id}/messages${next ? `?cursor=${encodeURIComponent(next)}` : ""}`);
    if (!record(result) || !Array.isArray(result.messages) || !result.messages.every(item => record(item) && typeof item.id === "string" && typeof item.from === "string" && typeof item.body === "string" && date(item.received_at) && date(item.expires_at)) || (result.next_cursor !== undefined && result.next_cursor !== null && typeof result.next_cursor !== "string")) throw new Error("Invalid message response.");
    if (alive.current && inboxRef.current === id) {setMessages(previous => next ? [...previous, ...result.messages as Message[]] : result.messages as Message[]); setCursor(typeof result.next_cursor === "string" ? result.next_cursor : undefined);}
  }
  return <div className="vault-page"><div className="vault-content phone-service">
    <header className="vault-heading"><div><h1>Phone numbers</h1></div><p>Dedicated numbers for incoming SMS. Some websites may not accept these numbers for verification.</p></header>
    {error ? <p role="alert">{error}</p> : null}
    {pendingId ? <section className="vault-section" aria-label="Phone request">
      <h2>{pending?.kind === "release" ? "Release number" : "Number request"}</h2>
      {pending ? <><p><strong>{pending.phone_number}</strong></p><p role="status">{statusLabel(pending)}</p></> : <p role="status">Checking request status…</p>}
      {pending?.status === "pending_approval" && !uncertain ? <>
        {pending.kind === "purchase" && pending.quote ? <>
          <dl><dt>Recurring monthly cost</dt><dd>{pending.quote.currency.toUpperCase()} {pending.quote.monthly_price} / month</dd><dt>Incoming SMS</dt><dd>{pending.quote.currency.toUpperCase()} {pending.quote.inbound_sms_price} / message</dd><dt>Quote expires</dt><dd>{new Date(pending.quote.expires_at).toLocaleString()}</dd></dl>
          <label className="phone-consent"><input type="checkbox" checked={accepted} onChange={event => setAccepted(event.target.checked)} />I approve the recurring monthly cost and incoming SMS charges until I release this number.</label>
          {Date.parse(pending.quote.expires_at) <= now ? <p role="alert">This quote has expired. Deny this request and select a number for a new quote.</p> : null}
        </> : <><p>Releasing this number stops incoming messages. You may lose access to accounts that use it for two-factor authentication. Update those accounts before releasing it.</p><label className="phone-consent"><input type="checkbox" checked={accepted} onChange={event => setAccepted(event.target.checked)} />I have updated my linked accounts and understand that I may lose verification access.</label></>}
        {callbackTarget ? <label className="phone-consent"><input type="checkbox" checked={shareCompletion} onChange={event => setShareCompletion(event.target.checked)} />Share completion with {callbackTarget.origin}. This includes the request ID and status.</label> : null}
        <div className="phone-actions"><button type="button" disabled={busy} onClick={() => void run(() => decide(false))}>Deny request</button><button type="button" disabled={busy || !accepted || Boolean(callbackTarget && !shareCompletion) || (pending.kind === "purchase" && Date.parse(pending.quote!.expires_at) <= now)} onClick={() => void run(() => decide(true))}>{pending.kind === "purchase" ? "Approve purchase" : "Confirm release"}</button></div>
      </> : null}
      {uncertain ? <p>The outcome is unknown. Check this same request before taking another action. A new purchase or release will not be submitted.</p> : null}
      <button type="button" disabled={busy} onClick={() => void run(async () => { const item = operation(await request(`/requests/${pendingId}`), pendingId); observe(item); if (shareCompletion) share(item); })}>Check status</button>
      {pending && terminal(pending.status) ? <button type="button" disabled={busy} onClick={() => {setPendingId(""); setPending(undefined);}}>Close request</button> : null}
      {pending && terminal(pending.status) && callbackTarget ? <button type="button" disabled={busy} onClick={() => share(pending)}>Share completion with {callbackTarget.origin}</button> : null}
    </section> : null}
    <section className="vault-section" aria-label="Find a number"><h2>Find a number</h2><form onSubmit={event => {event.preventDefault(); void run(async () => {
      const result = await request(`/numbers/available?country=US&limit=10${area ? `&area_code=${encodeURIComponent(area)}` : ""}`);
      if (!record(result) || !Array.isArray(result.numbers) || !result.numbers.every(item => record(item) && phone(item.phone_number) && item.country === "US" && item.type === "local")) throw new Error("Invalid available numbers.");
      if (alive.current) setAvailable(result.numbers as Available[]);
    });}}><p>United States · local numbers</p><label>Area code (optional)<input value={area} onChange={event => setArea(event.target.value)} inputMode="numeric" pattern="[2-9][0-9]{2}" maxLength={3} /></label><button type="submit" disabled={busy || locked}>Find available numbers</button></form>
      {available ? available.length ? <ul className="phone-list">{available.map(item => <li key={item.phone_number}><span>{item.phone_number}</span><button type="button" disabled={busy || locked} onClick={() => void run(() => stage(item))}>Review quote for {item.phone_number}</button></li>)}</ul> : <p>No numbers are available for this search.</p> : null}
    </section>
    <section className="vault-section" aria-label="Your numbers"><h2>Your numbers</h2><button type="button" disabled={busy} onClick={() => void run(load)}>Refresh numbers</button>
      {owned.length ? <ul className="phone-list">{owned.map(item => <li key={item.id}><div><strong>{item.phone_number}</strong><p>{item.status.replaceAll("_", " ")}</p></div><div className="phone-actions"><button type="button" disabled={busy || item.status !== "active"} onClick={() => void run(() => readInbox(item.id))}>Read messages for {item.phone_number}</button><button type="button" disabled={busy || locked || item.status !== "active"} onClick={() => void run(() => stage(item))}>Release {item.phone_number}</button></div></li>)}</ul> : <p>No dedicated numbers yet.</p>}
    </section>
    {inbox ? <section className="vault-section" aria-label="SMS inbox"><h2>Incoming messages</h2><p>{owned.find(item => item.id === inbox)?.phone_number}</p><p>Messages expire automatically. Refresh to see new messages.</p><button type="button" disabled={busy} onClick={() => void run(() => readInbox(inbox))}>Refresh messages</button>
      {messages.filter(item => Date.parse(item.expires_at) > now).length ? <ul className="phone-list">{messages.filter(item => Date.parse(item.expires_at) > now).map(item => <li key={item.id}><article><strong>{item.from}</strong><p className="phone-message">{item.body}</p><small>{new Date(item.received_at).toLocaleString()} · Expires {new Date(item.expires_at).toLocaleString()}</small></article></li>)}</ul> : <p>No unexpired messages.</p>}
      {cursor ? <button type="button" disabled={busy} onClick={() => void run(() => readInbox(inbox, cursor))}>Load older messages</button> : null}
    </section> : null}
  </div></div>;
}
