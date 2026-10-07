import { useCallback, useEffect, useRef, useState } from "react";
import { MessageCircle } from "lucide-react";
import "./WhatsAppConnection.css";

type Status = {
  connected: boolean;
  state: string;
  attempt: { operation_id: string; state: string; expires_at?: number } | null;
  connection_id?: string;
  label?: string;
};
type Pairing = { code: string; expiresAt: number };
const endpoint = "/v1/connectors/whatsapp";
const terminal = new Set(["disconnected", "signed_out", "expired", "failed", "revoked", "unavailable"]);

// Pairing responses deliberately bypass the shared query cache and error-body parser.
// A provider diagnostic can contain private pairing material; only fixed copy is rendered.
async function request(path: string, signal: AbortSignal, init: RequestInit = {}): Promise<Record<string, unknown>> {
  const response = await fetch(path, {
    ...init, signal, cache: "no-store", credentials: "same-origin",
    headers: { accept: "application/json", ...init.headers, "x-nanocodex-request": "1" },
  });
  if (!response.ok) throw new Error(response.status === 401 ? "Sign in again to manage WhatsApp." : "Couldn’t check WhatsApp. Check the status before trying again.");
  if (response.status === 204) return {};
  const value: unknown = await response.json();
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid WhatsApp response.");
  return value as Record<string, unknown>;
}

type WhatsAppConnectionProps = { requiresLogin?: boolean; accountId?: string };

export function WhatsAppConnection(props: WhatsAppConnectionProps) {
  // Account changes discard all private component state before rendering the next account.
  return <WhatsAppConnectionContent key={props.accountId ?? "signed-out"} {...props} />;
}

function WhatsAppConnectionContent({ requiresLogin = false, accountId = "" }: WhatsAppConnectionProps) {
  const storageKey = `nanocodex.whatsapp.attempt:${accountId}`;
  const [expanded, setExpanded] = useState(() => new URL(window.location.href).searchParams.get("connect") === "whatsapp");
  const operation = useRef<string | null>(null);
  const [phone, setPhone] = useState("");
  const [status, setStatus] = useState<Status | null>(null);
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [uncertain, setUncertain] = useState(false);
  const [copied, setCopied] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const lifetime = useRef<AbortController | null>(null);
  const pending = useRef(false);
  const expiredOperation = useRef<string | undefined>(undefined);

  const check = useCallback(async (manual = false) => {
    const abort = lifetime.current;
    if (!abort || abort.signal.aborted || pending.current) return;
    pending.current = true;
    // Background polling must never disable inputs or flash "Checking…".
    if (manual) setBusy(true);
    try {
      const body = await request(endpoint, abort.signal);
      if (typeof body.connected !== "boolean" || typeof body.state !== "string") throw new Error();
      if (body.attempt !== null && (typeof body.attempt !== "object" || !body.attempt
        || !("operation_id" in body.attempt) || typeof body.attempt.operation_id !== "string"
        || !("state" in body.attempt) || !["requested", "ready", "expired", "unknown", "paired"].includes(String(body.attempt.state)))) throw new Error();
      const current: Status = {
        connected: body.connected, state: body.state,
        attempt: body.attempt && typeof body.attempt === "object" && "operation_id" in body.attempt && typeof body.attempt.operation_id === "string" && "state" in body.attempt && typeof body.attempt.state === "string"
          ? body.attempt as NonNullable<Status["attempt"]> : null,
        ...(typeof body.connection_id === "string" ? { connection_id: body.connection_id } : {}),
        ...(typeof body.label === "string" ? { label: body.label } : {}),
      };
      if (current.attempt) {
        operation.current = current.attempt.operation_id;
        try { sessionStorage.setItem(storageKey, operation.current); } catch {}
      }
      if (current.connected || current.attempt?.state === "expired" || current.attempt?.state === "paired") {
        operation.current = null;
        try { sessionStorage.removeItem(storageKey); } catch {}
      }
      let nextPairing: Pairing | null = null;
      if (!current.connected && current.attempt?.operation_id && current.attempt.state === "ready"
        && expiredOperation.current !== current.attempt?.operation_id) {
        const privateResult = await request(`${endpoint}/pairing?operation_id=${encodeURIComponent(current.attempt?.operation_id)}`, abort.signal);

        // Connection success is established by the status endpoint, never by a code response.
        if (terminal.has(String(privateResult.state))) current.state = String(privateResult.state);
        else if (typeof (privateResult.code ?? privateResult.pairing_code) === "string") {
          const code = String(privateResult.code ?? privateResult.pairing_code).replaceAll("-", "");
          if (!/^[A-Z0-9]{8}$/.test(code) || typeof privateResult.expires_at !== "number" || !Number.isFinite(privateResult.expires_at)) throw new Error();
          if (privateResult.expires_at > Date.now()) nextPairing = { code, expiresAt: privateResult.expires_at };
          else { current.state = "expired"; expiredOperation.current = current.attempt?.operation_id; }
        }
      }
      if (abort.signal.aborted) return;
      if (current.attempt?.operation_id && expiredOperation.current === current.attempt?.operation_id && !current.connected) current.state = "expired";
      setStatus(current);
      setPairing(nextPairing);
      setUncertain(!!operation.current && !current.attempt && !current.connected);
      setError(null);
    } catch {
      if (!abort.signal.aborted) {
        setPairing(null);
        setError("Couldn’t check WhatsApp. Check the status before trying again.");
      }
    } finally {
      pending.current = false;
      if (manual && !abort.signal.aborted) setBusy(false);
    }
  }, [storageKey]);

  useEffect(() => {
    if (requiresLogin) return;
    const abort = new AbortController();
    lifetime.current = abort;
    try { operation.current = sessionStorage.getItem(storageKey); } catch {}
    setUncertain(!!operation.current);
    void check();
    return () => { abort.abort(); lifetime.current = null; };
  }, [check, requiresLogin, storageKey]);

  useEffect(() => {
    if (!pairing) return;
    const expire = window.setTimeout(() => {
      expiredOperation.current = status?.attempt?.operation_id;
      setPairing(null);
      setCopied(false);
      setStatus(value => value?.connected ? value : value ? { ...value, state: "expired" } : value);
    }, Math.max(0, pairing.expiresAt - Date.now()));
    return () => window.clearTimeout(expire);
  }, [pairing, status?.attempt?.operation_id]);

  const active = !!status && !status.connected && !!status.attempt && !terminal.has(status.attempt.state) && status.state !== "expired";
  useEffect(() => {
    if (!active || error || uncertain) return;
    const timer = window.setInterval(() => void check(), 4_000);
    const visible = () => { if (document.visibilityState === "visible") void check(); };
    document.addEventListener("visibilitychange", visible);
    return () => { window.clearInterval(timer); document.removeEventListener("visibilitychange", visible); };
  }, [active, check, error, uncertain]);

  const start = async () => {
    const abort = lifetime.current;
    const normalized = phone.replace(/[\s()-]/g, "");
    if (!abort || pending.current || !status) return;
    if (!/^\+[1-9][0-9]{7,14}$/.test(normalized)) {
      setError("Enter your WhatsApp phone number with its country code, for example +14155550123.");
      return;
    }
    pending.current = true;
    setBusy(true);
    setError(null);
    setPairing(null);
    setCopied(false);
    // An active attempt is replaced by a fresh one; an uncertain start reuses its id.
    const operationId = (!active && operation.current) || crypto.randomUUID();
    expiredOperation.current = undefined;
    try { sessionStorage.setItem(storageKey, operationId); } catch {
      pending.current = false; setBusy(false);
      setError("Allow session storage to safely recover a linking request."); return;
    }
    operation.current = operationId;
    setUncertain(true);
    try {
      await request(`${endpoint}/start`, abort.signal, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ operation_id: operationId, phone: normalized }),
      });
      if (abort.signal.aborted) return;
      setPhone("");
      setReplacing(false);
      setStatus({ connected: false, state: "linking", attempt: { operation_id: operationId, state: "requested" } });
      pending.current = false;
      await check();
    } catch {
      if (!abort.signal.aborted) setError("The link request may have started. Check the status to continue the same attempt.");
    } finally {
      pending.current = false;
      if (!abort.signal.aborted) setBusy(false);
    }
  };

  const copyCode = async () => {
    const abort = lifetime.current;
    if (!pairing || Date.now() >= pairing.expiresAt) { setPairing(null); return; }
    try {
      await navigator.clipboard.writeText(pairing.code);
      if (!abort?.signal.aborted) { setCopied(true); navigator.vibrate?.(10); }
    } catch {
      if (!abort?.signal.aborted) setError("Select the code and copy it manually.");
    }
  };

  const unlink = async () => {
    const abort = lifetime.current;
    if (!abort || pending.current || !status?.connection_id) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    setPairing(null);
    try {
      await request(`${endpoint}/connections/${encodeURIComponent(status.connection_id)}`, abort.signal, { method: "DELETE" });
      pending.current = false;
      await check();
    } catch {
      if (!abort.signal.aborted) setError("Couldn’t confirm unlinking. Check the status before trying again.");
    } finally {
      pending.current = false;
      if (!abort.signal.aborted) setBusy(false);
    }
  };

  return <section className="whatsapp-connection" id="whatsapp-connection" aria-label="WhatsApp connection">
    <button className={`connection-card connector-row${status?.connected ? " is-connected" : ""}`} type="button"
      disabled={requiresLogin} aria-expanded={expanded} aria-controls="whatsapp-pairing" onClick={() => setExpanded(value => !value)}>
      <MessageCircle aria-hidden="true" />
      <span className="connection-card-copy"><strong>WhatsApp</strong><span>{status?.connected ? status.label ?? "Connected" : "Link your WhatsApp on this phone"}</span></span>
      <span className="connection-card-action">{status?.connected ? "Manage" : "Connect"}</span>
    </button>
    {expanded && <div id="whatsapp-pairing" className="whatsapp-pairing">
      {status?.connected ? <>
        <p role="status">WhatsApp is connected.</p>
        <p>Available messages depend on what WhatsApp syncs to this linked device; complete history may not be available.</p>
        <button type="button" disabled={busy || !status.connection_id} onClick={() => void unlink()}>Unlink WhatsApp</button>
      </> : <>
        {(status?.state === "expired" || status?.attempt?.state === "expired") && <p role="status">This code expired. Start a new link when you’re ready.</p>}
        {status?.state === "failed" && <p role="status">WhatsApp couldn’t complete this link. You can start a new attempt.</p>}
        {uncertain && <p role="status">The link request may still be processing. Check the connection, or reenter the same phone number to retry this attempt.</p>}
        {(!active || replacing) && <form onSubmit={event => { event.preventDefault(); void start(); }}>
          <label htmlFor="whatsapp-phone">WhatsApp phone number</label>
          <input id="whatsapp-phone" type="tel" inputMode="tel" autoComplete="tel" placeholder="+14155550123" value={phone} onChange={event => setPhone(event.target.value)} disabled={busy} required />
          <button type="submit" className="whatsapp-primary" disabled={busy || !status || !!error && !phone}>{active ? "Get a new code" : uncertain ? "Retry same linking attempt" : "Start linking"}</button>
        </form>}
        {active && !pairing && !error && <p role="status">{status?.attempt?.state === "unknown" ? "WhatsApp has not confirmed this attempt. Keep checking until it connects or expires." : "Waiting for your linking code…"}</p>}
        {pairing && <div className="whatsapp-code-card">
          <label htmlFor="whatsapp-private-code">Your private linking code</label>
          <button type="button" className="whatsapp-code" id="whatsapp-private-code" aria-label={`Linking code ${pairing.code.split("").join(" ")}. Tap to copy.`} onClick={() => void copyCode()}>
            <output>{pairing.code.slice(0, 4)}<span aria-hidden="true">-</span>{pairing.code.slice(4)}</output>
            <small>{copied ? "Copied" : "Tap to copy"}</small>
          </button>
          <a className="whatsapp-primary" href="whatsapp://" onClick={() => { void copyCode(); }}>Copy &amp; open WhatsApp</a>
          <ol><li>In WhatsApp, open Settings → Linked devices → Link a device.</li><li>Choose “Link with phone number instead”.</li><li>Paste the code. This page updates automatically when linking finishes.</li></ol>
          <p>Expires in <Countdown until={pairing.expiresAt} />. Keep this code private.</p>
          {!replacing && <button type="button" onClick={() => setReplacing(true)}>Wrong number or need a new code?</button>}
        </div>}
      </>}
      {error && <p role="alert">{error}</p>}
      <button type="button" disabled={busy} onClick={() => void check(true)}>{busy ? "Checking…" : "Check connection"}</button>
    </div>}
  </section>;
}

function Countdown({ until }: { until: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, []);
  const seconds = Math.max(0, Math.round((until - now) / 1_000));
  return <time dateTime={new Date(until).toISOString()}>{Math.floor(seconds / 60)}:{String(seconds % 60).padStart(2, "0")}</time>;
}
