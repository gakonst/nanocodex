import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  NanocodexProvider,
} from "nanocodex-react";
import { NanocodexTui } from "nanocodex-tui-react";
import "nanocodex-tui-react/structure.css";

import { nanocodexConfig } from "./nanocodex";

/** Website policy around the reusable TUI: credential UX and the site theme. */
export function AgentTerminal() {
  return (
    <NanocodexProvider config={nanocodexConfig}>
      <AgentTerminalDemo />
    </NanocodexProvider>
  );
}

function AgentTerminalDemo() {
  const [credentialSource, setCredentialSource] = useState<CredentialSource | undefined>();
  useEffect(() => {
    let active = true;
    void fetch("/api/health")
      .then(async (response) => {
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return response.json() as Promise<{
          agent_configured?: boolean;
          credential_source?: CredentialSource;
        }>;
      })
      .then((health) => {
        if (!active) return;
        setCredentialSource(health.agent_configured === true
          && (health.credential_source === "user" || health.credential_source === "deployment")
          ? health.credential_source
          : null);
      }, () => {
        if (active) setCredentialSource(null);
      });
    return () => { active = false; };
  }, []);
  return (
    <div className="nanocodex-demo">
      <CredentialBar source={credentialSource} />
      <NanocodexTui
        enabled={credentialSource === "user" || credentialSource === "deployment"}
        unavailableMessage={credentialSource === undefined
          ? "Checking OpenAI credentials..."
          : "OpenAI API key is not configured"}
      />
    </div>
  );
}

type CredentialSource = "user" | "deployment" | null;

function CredentialBar({ source }: { source: CredentialSource | undefined }) {
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const keyRef = useRef<HTMLInputElement>(null);

  const save = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const apiKey = keyRef.current?.value.trim() ?? "";
    if (!apiKey) {
      setError("Enter an OpenAI API key.");
      keyRef.current?.focus();
      return;
    }
    setBusy(true);
    setError("");
    try {
      const response = await fetch("/api/auth/openai", {
        method: "PUT",
        credentials: "same-origin",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ api_key: apiKey }),
      });
      if (keyRef.current) keyRef.current.value = "";
      if (!response.ok) throw new Error(await credentialError(response));
      window.location.reload();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not start the key session.");
      setBusy(false);
    }
  };

  const clear = async () => {
    setBusy(true);
    setError("");
    try {
      const response = await fetch("/api/auth/openai", {
        method: "DELETE",
        credentials: "same-origin",
      });
      if (!response.ok) throw new Error(await credentialError(response));
      window.location.reload();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not forget the key session.");
      setBusy(false);
    }
  };

  const label = source === undefined
    ? "Checking OpenAI credentials"
    : source === "user"
    ? "Using your OpenAI API key"
    : source === "deployment"
      ? "Using the site demo key"
      : "Add an OpenAI API key to run the agent";

  return (
    <aside className="agent-byok" aria-label="OpenAI API key">
      <div className="agent-byok-summary">
        <span><i className={source ? "is-ready" : ""} aria-hidden="true" />{label}</span>
        <div>
          <button type="button" onClick={() => { setEditing((value) => !value); setError(""); }} disabled={busy}>
            {source === "user" ? "Replace key" : "Use your key"}
          </button>
          {source === "user" ? <button type="button" onClick={clear} disabled={busy}>Forget key</button> : null}
        </div>
      </div>
      {editing ? (
        <form className="agent-byok-form" onSubmit={save}>
          <label htmlFor="nanocodex-openai-key">OpenAI API key</label>
          <input
            id="nanocodex-openai-key"
            ref={keyRef}
            type="password"
            autoComplete="new-password"
            placeholder="sk-…"
            disabled={busy}
            spellCheck={false}
          />
          <button type="submit" disabled={busy}>{busy ? "Starting…" : "Start one-hour session"}</button>
          <p>Your key is held server-side for one hour. This page receives only an HttpOnly session cookie.</p>
        </form>
      ) : null}
      {error ? <p className="agent-byok-error" role="alert">{error}</p> : null}
    </aside>
  );
}

async function credentialError(response: Response): Promise<string> {
  const payload = await response.json().catch(() => undefined) as { error?: unknown } | undefined;
  return typeof payload?.error === "string" ? payload.error : `Request failed with HTTP ${response.status}`;
}
