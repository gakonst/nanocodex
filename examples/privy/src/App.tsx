import { usePrivy } from "@privy-io/react-auth";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Client,
  Dialog,
  Principal,
  Transport,
  type ConnectAgent,
  type HostConnection,
} from "nanocodex/connect";
import { createSessionFence, revokeHostPrincipal } from "./session-fence";

export type PublicConfiguration = Readonly<{
  appId: string;
  appOrigin: string;
  privyAppId: string;
  nanocodexApiUrl: string;
  connectDialogUrl: string;
}>;

type TranscriptEntry = Readonly<{
  role: "you" | "agent";
  text: string;
}>;

const VISIBILITY = Object.freeze({
  finalMessages: true,
  actionSummaries: true,
  conversationHistory: true,
});
const CHATGPT = Object.freeze({ chatgpt: true as const });
const STORAGE_PREFIX = "nanocodex:privy-example";
const FIRST_TURN = "Remember the code PRIVY_HOST_OK for my next turn. Reply with SAVED.";
const SECOND_TURN = "What code did I ask you to remember? Reply with only that code.";

export function App({ configuration }: Readonly<{ configuration: PublicConfiguration }>) {
  const { ready, authenticated, login, logout, user } = usePrivy();
  const [connection, setConnection] = useState<HostConnection>();
  const [agent, setAgent] = useState<ConnectAgent>();
  const [status, setStatus] = useState("Waiting for Privy");
  const [error, setError] = useState<string>();
  const [transcript, setTranscript] = useState<readonly TranscriptEntry[]>([]);
  const generation = useRef(0);
  const activeUserId = useRef<string | undefined>(undefined);
  const agentRef = useRef<ConnectAgent | undefined>(undefined);
  const fence = useMemo(() => createSessionFence(), []);
  const originMatches = configuration.appOrigin === window.location.origin;
  const client = useMemo(() => Client.create({
    appId: configuration.appId,
    appOrigin: configuration.appOrigin,
    principal: Principal.host({ url: "/api/nanocodex/host-principal" }),
    dialog: Dialog.popup({
      host: configuration.connectDialogUrl,
      key: "privy-host-auth-example",
      name: "Privy × Nanocodex",
    }),
    transport: Transport.http(configuration.nanocodexApiUrl, {
      key: "privy-host-auth-example",
      name: "Nanocodex API",
    }),
  }), [configuration]);

  function clearLocalSession(userId?: string) {
    agentRef.current = undefined;
    setAgent(undefined);
    setConnection(undefined);
    setTranscript([]);
    if (userId) clearStoredSession(userId);
  }

  useEffect(() => {
    if (!ready || !originMatches) return;
    const current = ++generation.current;
    const nextUserId = authenticated ? user?.id : undefined;
    void (async () => {
      const previousUserId = activeUserId.current;
      if (previousUserId && previousUserId !== nextUserId) {
        const previousAgent = agentRef.current;
        try {
          await fence.beforeProviderChange({
            revoke: () => revokeHostPrincipal(),
            shutdownAgent: () => previousAgent?.session.shutdown() ?? Promise.resolve(),
            logoutConnect: () => client.account.logout(),
            clearUi: () => clearLocalSession(previousUserId),
          });
        } catch (reason) {
          await previousAgent?.session.shutdown().catch(() => undefined);
          await client.account.logout().catch(() => undefined);
          clearLocalSession(previousUserId);
          activeUserId.current = undefined;
          setStatus(nextUserId ? "Reload to retry session cleanup" : "Sign in with Privy");
          setError(`The prior Nanocodex session could not be revoked: ${errorMessage(reason)}`);
          return;
        }
      } else {
        await fence.cancel();
      }

      if (!nextUserId) {
        activeUserId.current = undefined;
        await client.account.logout().catch(() => undefined);
        clearLocalSession();
        setStatus("Sign in with Privy");
        return;
      }

      activeUserId.current = nextUserId;
      setTranscript(readTranscript(nextUserId));
      setStatus("Checking for a saved Nanocodex session…");
      setError(undefined);
      await fence.run(async (signal) => {
        const conversationId = readConversationId(nextUserId);
        const restored = await client.connection.reconnect({
          authorization: "hosted",
          capabilities: { agent: VISIBILITY, cloudAccounts: CHATGPT },
          conversationId,
          permission: "agent.run",
          signal,
        });
        signal.throwIfAborted();
        if (!restored) {
          if (generation.current === current) setStatus("Ready to connect");
          return;
        }
        const restoredAgent = await client.agent.create({ connection: restored, signal });
        signal.throwIfAborted();
        if (generation.current === current && activeUserId.current === nextUserId) {
          agentRef.current = restoredAgent;
          setConnection(restored);
          setAgent(restoredAgent);
          setStatus("Reconnected to the durable agent");
        }
      });
    })().catch((reason) => {
      if (generation.current !== current || isAbort(reason)) return;
      setStatus("Ready to connect");
      setError(errorMessage(reason));
    });
  }, [authenticated, client, fence, originMatches, ready, user?.id]);

  async function connect() {
    const userId = activeUserId.current;
    if (!userId) return;
    const current = ++generation.current;
    setError(undefined);
    setStatus("Opening Nanocodex approval…");
    try {
      await fence.run(async (signal) => {
        const conversationId = getOrCreateConversationId(userId);
        const next = await client.connection.connect({
          authorization: "hosted",
          capabilities: { agent: VISIBILITY, cloudAccounts: CHATGPT },
          conversationId,
          permission: "agent.run",
          signal,
        });
        signal.throwIfAborted();
        const nextAgent = await client.agent.create({ connection: next, signal });
        signal.throwIfAborted();
        if (generation.current === current && activeUserId.current === userId) {
          agentRef.current = nextAgent;
          setConnection(next);
          setAgent(nextAgent);
          setStatus("Connected to a durable agent");
        }
      });
    } catch (reason) {
      if (!isAbort(reason)) setError(errorMessage(reason));
      setStatus("Ready to connect");
    }
  }

  async function runTurn(prompt: string) {
    const currentAgent = agentRef.current;
    const userId = activeUserId.current;
    if (!currentAgent || !userId) return;
    const current = generation.current;
    setError(undefined);
    setStatus("Running durable turn…");
    try {
      await fence.run(async (signal) => {
        const next = [...transcript, { role: "you" as const, text: prompt }];
        setTranscript(next);
        writeTranscript(userId, next);
        const shutdown = () => { void currentAgent.session.shutdown(); };
        signal.addEventListener("abort", shutdown, { once: true });
        try {
          const result = await currentAgent.turn.prompt({ input: prompt }).result();
          signal.throwIfAborted();
          if (generation.current !== current || activeUserId.current !== userId) return;
          const completed = [...next, { role: "agent" as const, text: result.finalMessage }];
          setTranscript(completed);
          writeTranscript(userId, completed);
          setStatus("Turn completed");
        } finally {
          signal.removeEventListener("abort", shutdown);
        }
      });
    } catch (reason) {
      if (!isAbort(reason)) setError(errorMessage(reason));
      if (generation.current === current) setStatus("Agent connected");
    }
  }

  async function secureLogout() {
    const userId = activeUserId.current;
    const currentAgent = agentRef.current;
    if (!userId) return;
    setError(undefined);
    setStatus("Revoking the host session…");
    try {
      await fence.beforeProviderChange({
        revoke: () => revokeHostPrincipal(),
        shutdownAgent: () => currentAgent?.session.shutdown() ?? Promise.resolve(),
        logoutConnect: () => client.account.logout(),
        clearUi() {
          generation.current += 1;
          activeUserId.current = undefined;
          agentRef.current = undefined;
          setAgent(undefined);
          setConnection(undefined);
          setTranscript([]);
          clearStoredSession(userId);
        },
      });
      // Privy logout happens last, while DELETE above can still verify the old cookie.
      await logout();
      setStatus("Signed out securely");
    } catch (reason) {
      setStatus(connection ? "Agent connected" : "Ready");
      setError(`${errorMessage(reason)} Privy was not logged out; retry the secure logout.`);
    }
  }

  if (!originMatches) {
    return (
      <main className="boot-error" role="alert">
        This Worker is configured for <code>{configuration.appOrigin}</code>, not the current
        origin. Host-principal exchanges fail closed until these match exactly.
      </main>
    );
  }

  const completedTurns = transcript.filter(({ role }) => role === "agent").length;
  const busy = status.includes("…");

  return (
    <main className="shell">
      <header className="topbar">
        <div><span className="mark" /> Nanocodex</div>
        <span className="provider">Privy host authentication</span>
      </header>

      <section className="hero">
        <p className="eyebrow">Existing login → durable agent</p>
        <h1>Your Privy session,<br />your hosted Nanocodex.</h1>
        <p className="lede">
          The server verifies Privy, exchanges an opaque one-time principal, and keeps both
          provider credentials and the Nanocodex project secret out of this browser.
        </p>
      </section>

      <div className="grid">
        <section className="card auth-card">
          <p className="label">01 · Authenticate</p>
          <h2>{authenticated ? "Privy session active" : "Sign in to begin"}</h2>
          <p>{authenticated ? `Signed in as ${user?.email?.address ?? "a Privy user"}.`
            : "Email authentication is owned by Privy; Nanocodex never receives its token."}</p>
          {!authenticated ? (
            <button className="primary" disabled={!ready} onClick={login} type="button">
              Continue with Privy
            </button>
          ) : (
            <button className="secondary danger" onClick={() => void secureLogout()} type="button">
              Securely log out
            </button>
          )}
        </section>

        <section className="card connect-card">
          <p className="label">02 · Connect</p>
          <h2>{connection ? "Host principal connected" : "Approve the hosted agent"}</h2>
          <p>
            {connection
              ? `Grant ${short(connection.grant.id)} · agent ${short(connection.agentId)}`
              : "The popup grants hosted agent access only—no wallet, access key, or MPP authority."}
          </p>
          <button
            className="primary"
            disabled={!authenticated || busy || Boolean(connection)}
            onClick={() => void connect()}
            type="button"
          >
            {connection ? "Connected" : "Connect Nanocodex"}
          </button>
          <p className="status"><span />{status}</p>
        </section>

        <section className="card turns-card">
          <div className="turns-heading">
            <div>
              <p className="label">03 · Prove durability</p>
              <h2>Two turns across reload</h2>
            </div>
            <span className="counter">{Math.min(completedTurns, 2)} / 2</span>
          </div>
          <p>Run turn one, reload this page, wait for “Reconnected,” then run turn two.</p>
          <div className="turn-actions">
            <button disabled={!agent || busy} onClick={() => void runTurn(FIRST_TURN)} type="button">
              Turn 1 · remember
            </button>
            <button disabled={!agent || busy || completedTurns < 1} onClick={() => void runTurn(SECOND_TURN)} type="button">
              Turn 2 · recall
            </button>
          </div>
          <div className="transcript" aria-live="polite">
            {transcript.length === 0 ? <p className="empty">Agent replies appear here.</p> : transcript.map((entry, index) => (
              <div className={`message ${entry.role}`} key={`${index}-${entry.text.slice(0, 12)}`}>
                <span>{entry.role}</span><p>{entry.text}</p>
              </div>
            ))}
          </div>
        </section>
      </div>

      {error ? <div className="error" role="alert">{error}</div> : null}
      <footer>Same-origin exchange · session-bound reconnect · revoke-before-logout fencing</footer>
    </main>
  );
}

function getOrCreateConversationId(userId: string): string {
  const existing = readConversationId(userId);
  if (existing) return existing;
  const created = crypto.randomUUID();
  localStorage.setItem(storageKeys(userId).conversation, created);
  return created;
}

function readConversationId(userId: string): string | undefined {
  const value = localStorage.getItem(storageKeys(userId).conversation);
  return value && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)
    ? value : undefined;
}

function readTranscript(userId: string): readonly TranscriptEntry[] {
  try {
    const value = JSON.parse(localStorage.getItem(storageKeys(userId).transcript) ?? "[]") as unknown;
    return Array.isArray(value) && value.every((entry) => entry && typeof entry === "object"
      && ((entry as TranscriptEntry).role === "you" || (entry as TranscriptEntry).role === "agent")
      && typeof (entry as TranscriptEntry).text === "string") ? value as TranscriptEntry[] : [];
  } catch {
    return [];
  }
}

function writeTranscript(userId: string, value: readonly TranscriptEntry[]) {
  localStorage.setItem(storageKeys(userId).transcript, JSON.stringify(value.slice(-8)));
}

function storageKeys(userId: string) {
  const scope = encodeURIComponent(userId);
  return {
    conversation: `${STORAGE_PREFIX}:${scope}:conversation`,
    transcript: `${STORAGE_PREFIX}:${scope}:transcript`,
  };
}

function clearStoredSession(userId: string) {
  const keys = storageKeys(userId);
  localStorage.removeItem(keys.conversation);
  localStorage.removeItem(keys.transcript);
}

function short(value: string): string {
  return value.length > 20 ? `${value.slice(0, 10)}…${value.slice(-6)}` : value;
}

function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : "The request failed.";
}

function isAbort(reason: unknown): boolean {
  return reason instanceof DOMException && reason.name === "AbortError";
}
