import { Sparkles } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { decodeClaudeLogin, type CredentialStatus } from "./modelCredentials";
import "./ClaudeConnection.css";

/** Account-only private OAuth intake. Codes never enter agent, query-cache or telemetry state. */
export function ClaudeConnection({ status, disabled, onChanged }: Readonly<{
  status: CredentialStatus["claude"];
  disabled: boolean;
  onChanged(): Promise<void>;
}>) {
  const [login, setLogin] = useState<ReturnType<typeof decodeClaudeLogin>>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  const code = useRef<HTMLInputElement>(null);
  const card = useRef<HTMLDivElement>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; if (code.current) code.current.value = ""; };
  }, []);
  useEffect(() => {
    if (window.location.hash === "#claude-connection") card.current?.scrollIntoView({ block: "center" });
  }, []);
  useEffect(() => {
    if (status.connected) {
      setLogin(undefined);
      if (code.current) code.current.value = "";
    }
  }, [status.connected]);
  async function start() {
    if (disabled || pending) return;
    // Reserve a window during the user gesture so async PKCE creation is not popup-blocked.
    const popup = window.open("about:blank", "_blank");
    if (popup) popup.opener = null;
    setPending(true); setError(undefined);
    setLogin(undefined);
    if (code.current) code.current.value = "";
    try {
      const response = await privateRequest("/v1/credentials/claude/login", "POST");
      const next = decodeClaudeLogin(await response.json());
      if (!mounted.current) { popup?.close(); return; }
      setLogin(next);
      if (popup && !popup.closed) popup.location.replace(next.authorizationUrl);
    } catch {
      popup?.close();
      // Never display response text: an upstream diagnostic might contain a private code.
      setError("Couldn’t start Claude sign-in. Refresh your account and try again.");
    } finally { setPending(false); }
  }
  async function complete(event: FormEvent) {
    event.preventDefault();
    if (disabled || pending || !code.current?.value.trim()) return;
    const privateCode = code.current.value.trim();
    code.current.value = "";
    setPending(true); setError(undefined);
    try {
      const response = await privateRequest("/v1/credentials/claude/login/complete", "POST", { code: privateCode });
      await response.body?.cancel();
      setLogin(undefined);
      if (mounted.current) await onChanged();
    } catch {
      setError("Claude sign-in wasn’t confirmed. Refresh status before starting a new sign-in. Codes expire and can only be used once.");
    } finally { setPending(false); }
  }
  async function disconnect() {
    if (disabled || pending) return;
    setPending(true); setError(undefined);
    if (code.current) code.current.value = "";
    try {
      const response = await privateRequest("/v1/credentials/claude", "DELETE");
      await response.body?.cancel();
      setLogin(undefined);
      if (mounted.current) await onChanged();
    } catch { setError("Couldn’t confirm Claude disconnection. Refresh status before trying again."); }
    finally { setPending(false); }
  }
  const signingIn = !status.connected && Boolean(login || status.pending || status.login);
  return <div className="wizard-connector-card chatgpt-accounts" id="claude-connection" role="listitem" ref={card}>
    <button className={`connection-card${status.connected ? " is-connected" : ""}`}
      type="button" disabled={disabled || pending} onClick={() => void (status.connected ? disconnect() : start())}>
      <span className="connector-logo" aria-hidden="true"><Sparkles fill="none" /></span>
      <span className="connection-card-copy">
        <strong>Claude</strong>
        <span>{status.connected ? "Subscription connected · Check the model picker for available models"
          : "Use your Claude Pro or Max subscription for managed model access"}</span>
      </span>
      <span className="connection-card-action">{pending ? "Working…" : status.connected ? "Disconnect" : signingIn ? "Restart sign-in" : "Connect"}</span>
    </button>
    {signingIn ? <form className="chatgpt-account-details connection-setup claude-connection-details" onSubmit={(event) => void complete(event)}>
      <strong>Finish Claude sign-in privately</strong>
      <p>Open the Claude sign-in page, approve access, then paste the returned code here. Never send the code in a chat.</p>
      {login ? <a href={login.authorizationUrl} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer">Open Claude sign-in page</a>
        : <p>To reopen the sign-in page, restart sign-in above.</p>}
      <div className="claude-private-field">
      <label htmlFor="claude-private-code">Claude authorization code (code#state)</label>
      <input id="claude-private-code" ref={code} type="password" autoComplete="off" spellCheck={false}
        autoCapitalize="none" disabled={disabled || pending} required maxLength={8192} />
      </div>
      <button type="submit" disabled={disabled || pending}>Complete Claude sign-in</button>
    </form> : null}
    {error ? <div className="account-failure claude-connection-failure" role="alert"><p>{error}</p>
      <button type="button" disabled={disabled || pending} onClick={() => void onChanged()}>Refresh Claude status</button>
    </div> : null}
  </div>;
}

async function privateRequest(path: string, method: string, body?: { code: string }): Promise<Response> {
  const response = await fetch(path, {
    method, credentials: "same-origin", cache: "no-store", redirect: "error",
    headers: { accept: "application/json", ...(body ? { "content-type": "application/json" } : {}) },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  if (!response.ok) { await response.body?.cancel(); throw new Error("Claude connection request failed."); }
  return response;
}
