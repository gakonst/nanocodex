import { createAuthClient } from "better-auth/react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { ConnectAgent, HostConnection } from "nanocodex/connect";

import {
  clearLocalConnectState,
  connectionRequest,
  createConnectClient,
  loadProof,
  newProof,
  openAgent,
  storeProof,
  type DurableProof,
  type PublicConfiguration,
} from "./connect";
import { createConnectLifecycle, revokeHostPrincipal } from "./lifecycle";

const authClient = createAuthClient({ basePath: "/api/auth" });

type Phase = "loading" | "signed-out" | "ready" | "reconnecting" | "connecting" | "connected";

export function App() {
  const [configuration, setConfiguration] = useState<PublicConfiguration>();
  const [authenticated, setAuthenticated] = useState(false);
  const [phase, setPhase] = useState<Phase>("loading");
  const [connection, setConnection] = useState<HostConnection>();
  const [agent, setAgent] = useState<ConnectAgent>();
  const [proof, setProof] = useState<DurableProof>();
  const [turnPending, setTurnPending] = useState(false);
  const [error, setError] = useState<string>();
  const reconnectAttempt = useRef<string | undefined>(undefined);

  useEffect(() => {
    let active = true;
    void Promise.all([
      fetchJson<PublicConfiguration>("/api/config"),
      fetchJson<{ authenticated: boolean }>("/api/session"),
    ]).then(([nextConfiguration, session]) => {
      if (!active) return;
      setConfiguration(nextConfiguration);
      setAuthenticated(session.authenticated === true);
      setPhase(session.authenticated ? "ready" : "signed-out");
    }, (reason: unknown) => {
      if (!active) return;
      setError(errorMessage(reason));
      setPhase("signed-out");
    });
    return () => { active = false; };
  }, []);

  const clientOrError = useMemo(() => {
    try {
      return configuration ? createConnectClient(configuration) : undefined;
    } catch (reason) {
      return reason instanceof Error ? reason : new Error("Invalid public app configuration.");
    }
  }, [configuration]);
  const client = clientOrError instanceof Error ? undefined : clientOrError;
  const lifecycle = useMemo(() => createConnectLifecycle(), [client]);

  useEffect(() => {
    if (!authenticated || !client || !configuration?.appId) return;
    const key = `${configuration.appId}:${configuration.appOrigin}`;
    if (reconnectAttempt.current === key) return;
    reconnectAttempt.current = key;
    setPhase("reconnecting");
    void lifecycle.run(async (signal) => {
      const retained = await client.connection.reconnect(connectionRequest(signal));
      if (!retained) {
        setPhase("ready");
        return;
      }
      await activate(client, retained, signal);
    }).catch((reason: unknown) => {
      if (!isAbort(reason)) setError(errorMessage(reason));
      setPhase("ready");
    });

    async function activate(
      connectClient: NonNullable<typeof client>,
      retained: HostConnection,
      signal: AbortSignal,
    ) {
      const opened = await openAgent(connectClient, retained, signal);
      if (signal.aborted) {
        await opened.session.shutdown();
        signal.throwIfAborted();
      }
      setConnection(retained);
      setAgent(opened);
      setProof(loadProof(retained.agentId));
      setPhase("connected");
    }
  }, [authenticated, client, configuration?.appId, configuration?.appOrigin, lifecycle]);

  async function signIn() {
    setError(undefined);
    const result = await authClient.signIn.social({
      provider: "github",
      callbackURL: `${window.location.origin}/`,
    });
    if (result.error) setError(result.error.message ?? "GitHub sign-in failed.");
  }

  async function connect() {
    if (!client) return;
    setError(undefined);
    setPhase("connecting");
    try {
      await lifecycle.run(async (signal) => {
        const created = await client.connection.connect(connectionRequest(signal));
        const opened = await openAgent(client, created, signal);
        if (signal.aborted) {
          await opened.session.shutdown();
          signal.throwIfAborted();
        }
        setConnection(created);
        setAgent(opened);
        setProof(loadProof(created.agentId));
        setPhase("connected");
      });
    } catch (reason) {
      if (!isAbort(reason)) setError(errorMessage(reason));
      setPhase("ready");
    }
  }

  async function signOut() {
    if (!client) return;
    setTurnPending(true);
    setError(undefined);
    try {
      await lifecycle.beforeProviderLogout({
        revoke: () => revokeHostPrincipal(),
        async logoutConnect() {
          await client.account.logout();
          await agent?.session.shutdown();
        },
        clearUi() {
          clearLocalConnectState();
          setConnection(undefined);
          setAgent(undefined);
          setProof(undefined);
        },
      });
      const result = await authClient.signOut();
      if (result.error) throw new Error(result.error.message ?? "Better Auth sign-out failed.");
      reconnectAttempt.current = undefined;
      setAuthenticated(false);
      setPhase("signed-out");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setTurnPending(false);
    }
  }

  async function runFirstTurn() {
    if (!agent) return;
    setTurnPending(true);
    setError(undefined);
    try {
      const current = proof?.agentId === agent.id ? proof : newProof(agent.id);
      const result = await agent.turn.prompt({
        input: `Remember this exact marker for my next turn: ${current.marker}. Reply briefly when it is stored.`,
      }).result();
      const completed = { ...current, first: result.finalMessage };
      storeProof(completed);
      setProof(completed);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setTurnPending(false);
    }
  }

  async function runSecondTurn() {
    if (!agent || !proof?.first || proof.agentId !== agent.id) return;
    setTurnPending(true);
    setError(undefined);
    try {
      const result = await agent.turn.prompt({
        input: "What exact marker did I ask you to remember in my previous turn? Return only the marker.",
      }).result();
      const completed = { ...proof, second: result.finalMessage };
      storeProof(completed);
      setProof(completed);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setTurnPending(false);
    }
  }

  const configurationError = clientOrError instanceof Error
    ? clientOrError.message
    : configuration && !configuration.configured
      ? "Configure the Worker, D1 database, GitHub OAuth app, and Nanocodex host project first."
      : undefined;
  const busy = phase === "loading" || phase === "reconnecting" || phase === "connecting" || turnPending;

  return (
    <main className="shell">
      <header className="topbar">
        <a className="wordmark" href="/" aria-label="Nanocodex Better Auth example">
          <span className="mark" aria-hidden="true" />
          <span>Nanocodex</span>
        </a>
        <span className={`status ${connection ? "online" : ""}`}>
          <span aria-hidden="true" />
          {phaseLabel(phase)}
        </span>
      </header>

      <section className="hero">
        <p className="eyebrow">External host authentication</p>
        <h1>Your login. One scoped agent.</h1>
        <p className="lede">
          Better Auth keeps the GitHub session and OAuth credentials in this Worker. The browser
          receives a one-time Nanocodex exchange, never the provider token or host project secret.
        </p>
      </section>

      <div className="workspace">
        <section className="panel primary" aria-labelledby="journey-title">
          <div className="panel-heading">
            <div>
              <p className="eyebrow">Live journey</p>
              <h2 id="journey-title">Two turns across a reconnect</h2>
            </div>
            <span className="step-count">01—03</span>
          </div>

          {configurationError ? (
            <div className="notice" role="status">{configurationError}</div>
          ) : !authenticated ? (
            <JourneyStep number="01" title="Authenticate the host">
              <p>Use GitHub through the app’s D1-backed Better Auth deployment.</p>
              <button disabled={busy} onClick={() => void signIn()} type="button">
                Continue with GitHub
              </button>
            </JourneyStep>
          ) : !connection ? (
            <JourneyStep number="02" title="Approve the hosted agent">
              <p>
                Nanocodex requests hosted execution and ChatGPT only—no access key, wallet, or
                spending authority.
              </p>
              <button disabled={busy || !client} onClick={() => void connect()} type="button">
                {phase === "reconnecting" ? "Checking saved grant…"
                  : phase === "connecting" ? "Opening Connect…" : "Connect Nanocodex"}
              </button>
            </JourneyStep>
          ) : (
            <JourneyStep number="03" title="Prove durable state">
              <p>
                Run turn one, reload this page, then run turn two. Reconnect validates a fresh
                host-principal exchange against the retained grant and opens the same agent.
              </p>
              <div className="turn-grid">
                <TurnCard
                  action="Run turn one"
                  disabled={turnPending || Boolean(proof?.first)}
                  label="Store a random marker"
                  output={proof?.first}
                  onRun={runFirstTurn}
                />
                <TurnCard
                  action="Run turn two"
                  disabled={turnPending || !proof?.first || Boolean(proof?.second)}
                  label="Recall it after reload"
                  output={proof?.second}
                  onRun={runSecondTurn}
                />
              </div>
            </JourneyStep>
          )}

          {error ? <div className="error" role="alert">{error}</div> : null}
        </section>

        <aside className="panel details" aria-labelledby="security-title">
          <div className="panel-heading">
            <div>
              <p className="eyebrow">Session boundary</p>
              <h2 id="security-title">What the app can see</h2>
            </div>
          </div>
          <dl>
            <Detail label="Better Auth" value={authenticated ? "Authenticated" : "No session"} />
            <Detail label="Authorization" value={connection?.authorization ?? "Hosted only"} />
            <Detail label="Agent" value={shortId(connection?.agentId)} />
            <Detail label="Grant" value={shortId(connection?.grant.id)} />
            <Detail label="Principal" value={shortId(connection?.principal.id)} />
          </dl>
          <div className="boundary">
            <strong>Worker only</strong>
            <p>Better Auth secret · GitHub client secret · OAuth tokens · Nanocodex project secret</p>
          </div>
          {authenticated ? (
            <button className="secondary" disabled={busy} onClick={() => void signOut()} type="button">
              Revoke session and sign out
            </button>
          ) : null}
        </aside>
      </div>
    </main>
  );
}

function JourneyStep({ children, number, title }: Readonly<{
  children: React.ReactNode;
  number: string;
  title: string;
}>) {
  return (
    <div className="journey-step">
      <span className="number">{number}</span>
      <div><h3>{title}</h3>{children}</div>
    </div>
  );
}

function TurnCard({ action, disabled, label, onRun, output }: Readonly<{
  action: string;
  disabled: boolean;
  label: string;
  onRun(): Promise<void>;
  output?: string;
}>) {
  return (
    <article className="turn-card">
      <span>{label}</span>
      {output ? <p>{output}</p> : <p className="muted">Waiting for this durable turn.</p>}
      <button disabled={disabled} onClick={() => void onRun()} type="button">{action}</button>
    </article>
  );
}

function Detail({ label, value }: Readonly<{ label: string; value?: string }>) {
  return <div><dt>{label}</dt><dd>{value ?? "—"}</dd></div>;
}

async function fetchJson<T>(path: string): Promise<T> {
  const response = await fetch(path, {
    credentials: "same-origin",
    headers: { accept: "application/json" },
  });
  if (!response.ok) throw new Error(`${path} failed (${response.status}).`);
  return response.json() as Promise<T>;
}

function shortId(value?: string): string | undefined {
  if (!value) return undefined;
  return value.length > 22 ? `${value.slice(0, 11)}…${value.slice(-7)}` : value;
}

function phaseLabel(phase: Phase): string {
  switch (phase) {
    case "loading": return "Loading";
    case "signed-out": return "Signed out";
    case "ready": return "Host ready";
    case "reconnecting": return "Reconnecting";
    case "connecting": return "Connecting";
    case "connected": return "Agent connected";
  }
}

function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : "The request failed.";
}

function isAbort(reason: unknown): boolean {
  return reason instanceof DOMException && reason.name === "AbortError";
}
