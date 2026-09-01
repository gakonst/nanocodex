import { useCallback, useEffect, useState } from "react";
import type {
  CloudAccount,
  ConnectAgent,
  Connection,
  Grant,
  MachineUsdFunding,
} from "nanocodex/connect";
import {
  useConnectAgent,
  useFund,
  useLogoutAccount,
  useRevokeGrant,
} from "nanocodex-react/connect";

import { config } from "./config";
import { ConnectAgentExperience, type AppObservation } from "./ConnectAgentExperience";
import { presentGrantCapabilities } from "./grantPresentation.mjs";

type AuditEvent = Readonly<{
  id: number;
  tone: "neutral" | "success" | "error";
  title: string;
  detail: string;
  time: string;
}>;

const INITIAL_AUDIT: readonly AuditEvent[] = [];

const MACHINE_USD_ATOMICS = 1_000_000n;

type VisibilityRequest = Readonly<{
  finalMessages: boolean;
  actionSummaries: boolean;
  conversationHistory: boolean;
  rawTraces: boolean;
}>;

type ConnectRequest = Readonly<{
  connectors: Readonly<Partial<Record<CloudAccount, true>>>;
  visibility: VisibilityRequest;
}>;

const DEFAULT_REQUEST: ConnectRequest = {
  connectors: { github: true, gmail: true, gdrive: true, x: true, slack: true, chatgpt: true },
  visibility: {
    finalMessages: true,
    actionSummaries: true,
    conversationHistory: false,
    rawTraces: false,
  },
};

const EMPTY_OBSERVATION: AppObservation = { actions: [], historyTurns: 0, traceEvents: 0 };

export function App() {
  const [audit, setAudit] = useState<readonly AuditEvent[]>(INITIAL_AUDIT);
  const [error, setError] = useState<string>();
  const [request, setRequest] = useState<ConnectRequest>(DEFAULT_REQUEST);
  const [observation, setObservation] = useState<AppObservation>(EMPTY_OBSERVATION);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const connect = useConnectAgent({ config });
  const connection = connect.connection;
  const fund = useFund({ config });
  const logoutAccount = useLogoutAccount({ config });
  const revoke = useRevokeGrant({ config });
  const isMutating = connect.connectionStatus === "connecting"
    || connect.isPending
    || fund.isPending
    || logoutAccount.isPending
    || revoke.isPending;
  const mercatorReady = Boolean(
    connect.agent
    && connection
    && connection.authorization === "access_key"
    && connection.mpp.balanceStatus === "ready"
    && connection.mpp.balance > 0n,
  );
  const observe = useCallback((value: AppObservation) => setObservation(value), []);

  useEffect(() => {
    if (!connection && detailsOpen) setDetailsOpen(false);
  }, [connection, detailsOpen]);

  useEffect(() => {
    if (!detailsOpen) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") setDetailsOpen(false);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [detailsOpen]);

  function record(title: string, detail: string, tone: AuditEvent["tone"] = "success") {
    setAudit((current) => [{
      id: Date.now(),
      tone,
      title,
      detail,
      time: new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
    }, ...current].slice(0, 8));
  }

  function fail(title: string, reason: unknown) {
    const message = errorMessage(reason);
    setError(message);
    record(title, message, "error");
  }

  function startConnect() {
    setError(undefined);
    connect.mutate(
      {
        capabilities: {
          agent: request.visibility,
          cloudAccounts: request.connectors,
        },
        permission: "agent.run",
      },
      {
        onSuccess({ connection: nextConnection }) {
          setObservation(EMPTY_OBSERVATION);
          record(
            "Agent instantiated",
            `${presentGrantCapabilities(nextConnection.grant.capabilities).join(" + ")} approved; ChatGPT is the model and MPP is reserved for BOOST.`,
          );
        },
        onError(reason: Error) {
          fail("Connection rejected", reason);
        },
      },
    );
  }

  function addMachineUsd() {
    if (!connection) return;
    setError(undefined);
    fund.mutate(
      {
        accountAddress: connection.accountAddress,
        grantId: connection.grant.id,
        usdAmountCents: 500,
      },
      {
        onSuccess(result: MachineUsdFunding) {
          record("machineUSD added", `Order ${result.order.id} issued $5.00 machineUSD.`);
        },
        onError(reason: Error) {
          if (isUserRejection(reason)) {
            record("Onramp cancelled", "No payment was submitted and the signed MPP limits are unchanged.", "neutral");
            return;
          }
          fail("Onramp failed", reason);
        },
      },
    );
  }

  function revokeAccess() {
    if (!connection) return;
    setError(undefined);
    revoke.mutate(
      { grantId: connection.grant.id },
      {
        onSuccess(grant: Grant) {
          record("Access revoked", `${shortHex(grant.id)} and its Tempo access key can no longer authorize requests.`);
        },
        onError(reason: Error) {
          fail("Revocation failed", reason);
        },
      },
    );
  }

  function logout() {
    setError(undefined);
    logoutAccount.mutate(undefined, {
      onSuccess() {
        setObservation(EMPTY_OBSERVATION);
        record("Signed out", "This browser forgot the Nanocodex account session. The app grant was not revoked.", "neutral");
      },
      onError(reason: Error) {
        fail("Sign out failed", reason);
      },
    });
  }

  if (connect.connectionStatus === "connecting" && !connect.isPending) return null;

  return (
    <main className={`app-shell${connection ? " is-connected" : ""}`} data-testid="connect-playground">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true" />
          <span className="brand-product">Nanocodex Connect</span>
        </div>
        <div className="environment">
          <span className="environment-dot" aria-hidden="true" />
          <span>{connection ? "Atlas connected" : "Playground · mainnet"}</span>
        </div>
      </header>

      <div className={`workspace-grid ${connection ? "is-connected" : ""}`}>
          {!connection ? (
            <>
              <section className="panel panel-dark connect-method">
                <header className="panel-heading">
                  <div>
                    <h1>Connect Atlas</h1>
                    <p className="panel-kicker">wallet_connect</p>
                  </div>
                  <span className="status-pill" data-testid="connection-status">Not connected</span>
                </header>
                <div className="panel-body empty-state">
                  <div className="empty-orbit" aria-hidden="true" />
                  <div className="connect-copy">
                    <h2>One approval.</h2>
                    <p>Sign in and grant this app the capabilities configured at right.</p>
                  </div>
                  {error ? (
                    <div className="error-banner connect-error" data-testid="error-message" role="alert">
                      <span>{error}</span>
                      <button aria-label="Dismiss error" onClick={() => setError(undefined)} type="button">×</button>
                    </div>
                  ) : null}
                  <button
                    className="primary-button connect-button"
                    data-testid="connect-button"
                    disabled={isMutating}
                    onClick={startConnect}
                    type="button"
                  >
                    Connect
                  </button>
                </div>
              </section>
              <aside className="playground-config">
                <section className="panel configuration-panel">
                  <header className="panel-heading">
                    <div>
                      <h2>Configuration</h2>
                      <p className="panel-kicker">Requested capabilities</p>
                    </div>
                  </header>
                  <div className="panel-body">
                    <PermissionBuilder
                      disabled={isMutating}
                      request={request}
                      onChange={setRequest}
                    />
                  </div>
                </section>
              </aside>
            </>
          ) : (
            <>
              <section className="panel chat-primary">
                <AgentPanel
                  agent={connect.agent}
                  connection={connection}
                  isMutating={isMutating}
                  onLogout={logout}
                  onOpenDetails={() => setDetailsOpen(true)}
                  onObservation={observe}
                />
              </section>
              <button
                aria-label="Close connection details"
                className={`connection-rail-backdrop${detailsOpen ? " is-open" : ""}`}
                onClick={() => setDetailsOpen(false)}
                type="button"
              />
              <aside
                aria-label="Connection details"
                aria-modal={detailsOpen ? true : undefined}
                className={`connected-rail${detailsOpen ? " is-open" : ""}`}
                role={detailsOpen ? "dialog" : undefined}
              >
                <button
                  className="connection-rail-close"
                  onClick={() => setDetailsOpen(false)}
                  type="button"
                >
                  Close
                </button>
                <section className="panel connection-panel">
                  <header className="panel-heading">
                    <div>
                      <h2>Atlas Workspace</h2>
                      <p className="panel-kicker">Connect grant</p>
                    </div>
                    <span className="status-pill active" data-testid="connection-status">Active</span>
                  </header>
                  <ConnectionWorkspace
                    connection={connection}
                    error={error}
                    isMutating={isMutating}
                    mppReady={connection.authorization === "access_key"
                      && connection.mpp.balanceStatus === "ready"}
                    mercatorReady={mercatorReady}
                    onDismissError={() => setError(undefined)}
                    onFund={addMachineUsd}
                    onLogout={logout}
                    onRevoke={revokeAccess}
                  />
                </section>
                <AppProjectionPanel
                  audit={audit}
                  observation={observation}
                  visibility={connection.grant.visibility}
                />
              </aside>
            </>
          )}
      </div>
    </main>
  );
}

function AgentPanel({ agent, connection, isMutating, onLogout, onOpenDetails, onObservation }: Readonly<{
  agent: ConnectAgent | undefined;
  connection: Connection;
  isMutating: boolean;
  onLogout(): void;
  onOpenDetails(): void;
  onObservation(value: AppObservation): void;
}>) {
  return agent ? (
    <ConnectAgentExperience
      agent={agent}
      connection={connection}
      isMutating={isMutating}
      onLogout={onLogout}
      onOpenDetails={onOpenDetails}
      onObservation={onObservation}
    />
  ) : null;
}

function AppProjectionPanel({ audit, observation, visibility }: Readonly<{
  audit: readonly AuditEvent[];
  observation: AppObservation;
  visibility: VisibilityRequest;
}>) {
  return (
    <section className="panel app-view-panel">
      <header className="panel-heading">
        <div>
          <h2>Atlas can see</h2>
          <p className="panel-kicker">Grant-enforced projection</p>
        </div>
        <span className="status-pill">Scoped</span>
      </header>
      <VisibilityInspector observation={observation} visibility={visibility} />
      <details className="audit-details">
        <summary>Audit · {audit.length}</summary>
        <ol className="audit-list" data-testid="audit-events">
          {audit.map((event) => (
            <li className="audit-event" key={event.id}>
              <span className={`audit-dot ${event.tone}`} aria-hidden="true" />
              <div>
                <strong>{event.title}</strong>
                <p>{event.detail}</p>
              </div>
              <time>{event.time}</time>
            </li>
          ))}
        </ol>
      </details>
    </section>
  );
}

function PermissionBuilder({ disabled, onChange, request }: Readonly<{
  disabled: boolean;
  onChange(value: ConnectRequest): void;
  request: ConnectRequest;
}>) {
  const connectors: readonly Readonly<{ id: CloudAccount; label: string; required?: boolean }>[] = [
    { id: "github", label: "GitHub" },
    { id: "gmail", label: "Gmail" },
    { id: "gdrive", label: "Drive" },
    { id: "x", label: "X" },
    { id: "chatgpt", label: "ChatGPT", required: true },
  ];
  const visibility: readonly Readonly<{ id: keyof VisibilityRequest; label: string; detail: string }>[] = [
    { id: "finalMessages", label: "Replies", detail: "Final assistant messages" },
    { id: "actionSummaries", label: "Actions", detail: "Tools used, without arguments" },
    { id: "conversationHistory", label: "History", detail: "Conversation titles and prior messages" },
    { id: "rawTraces", label: "Traces", detail: "Full reasoning and tool traffic" },
  ];

  function setVisibility(id: keyof VisibilityRequest, checked: boolean) {
    const next = { ...request.visibility, [id]: checked };
    if (id === "rawTraces" && checked) {
      next.finalMessages = true;
      next.actionSummaries = true;
      next.conversationHistory = true;
    } else if (!checked) {
      next.rawTraces = false;
    }
    onChange({ ...request, visibility: next });
  }

  return (
    <div className="permission-builder" data-testid="permission-builder">
      <fieldset>
        <legend>Connectors</legend>
        <div className="permission-options">
          {connectors.map((item) => (
            <label key={item.id} title={item.required ? "Required to run this embedded agent" : item.label}>
              <input
                checked={request.connectors[item.id] === true}
                disabled={disabled || item.required}
                onChange={(event) => {
                  const connectors = { ...request.connectors };
                  if (event.target.checked) connectors[item.id] = true;
                  else delete connectors[item.id];
                  onChange({ ...request, connectors });
                }}
                type="checkbox"
              />
              <span>{item.label}</span>
            </label>
          ))}
        </div>
      </fieldset>
      <fieldset>
        <legend>Atlas sees</legend>
        <div className="permission-options">
          {visibility.map((item) => (
            <label key={item.id} title={item.detail}>
              <input
                checked={request.visibility[item.id]}
                disabled={disabled}
                onChange={(event) => setVisibility(item.id, event.target.checked)}
                type="checkbox"
              />
              <span>{item.label}</span>
            </label>
          ))}
        </div>
      </fieldset>
    </div>
  );
}

function VisibilityInspector({ observation, visibility }: Readonly<{
  observation: AppObservation;
  visibility: VisibilityRequest;
}>) {
  const actions = observation.actions.filter((item) => item.startsWith("tool."));
  return (
    <dl className="visibility-inspector" data-testid="visibility-inspector">
      <Projection
        allowed={visibility.finalMessages}
        label="Final reply"
        value={observation.finalMessage || "Waiting for a turn"}
      />
      <Projection
        allowed={visibility.actionSummaries}
        label="Actions"
        value={actions.length ? actions.map((item) => item.slice(5)).join(" · ") : "No actions yet"}
      />
      <Projection
        allowed={visibility.conversationHistory}
        label="History"
        value={`${observation.historyTurns} visible turn${observation.historyTurns === 1 ? "" : "s"}`}
      />
      <Projection
        allowed={visibility.rawTraces}
        label="Raw traces"
        value={`${observation.traceEvents} streamed events`}
      />
    </dl>
  );
}

function Projection({ allowed, label, value }: Readonly<{
  allowed: boolean;
  label: string;
  value: string;
}>) {
  return (
    <div className={allowed ? "projection-row is-allowed" : "projection-row is-private"}>
      <dt>{label}</dt>
      <dd>{allowed ? value : "Private"}</dd>
    </div>
  );
}

function ConnectionWorkspace({
  connection,
  error,
  isMutating,
  mppReady,
  mercatorReady,
  onDismissError,
  onFund,
  onLogout,
  onRevoke,
}: Readonly<{
  connection: Connection;
  error: string | undefined;
  isMutating: boolean;
  mppReady: boolean;
  mercatorReady: boolean;
  onDismissError(): void;
  onFund(): void;
  onLogout(): void;
  onRevoke(): void;
}>) {
  const paidConnection = connection.authorization === "access_key" ? connection : undefined;
  return (
    <>
      <div className="panel-body">
        {error ? (
          <div className="error-banner" data-testid="error-message" role="alert">
            <span>{error}</span>
            <button aria-label="Dismiss error" onClick={onDismissError} type="button">×</button>
          </div>
        ) : null}

        {paidConnection && mppReady ? <div className="balance-card">
          <div className="mercator-balance-heading">
            <span className="balance-label">MPP available balance</span>
            <span className={mercatorReady ? "mercator-connected" : "mercator-locked"}>
              Mercator {mercatorReady ? "connected" : "locked"}
            </span>
          </div>
          <div className="balance-value" data-testid="mpp-balance">
            {formatMachineUsd(paidConnection.mpp.balance)} <span>MACHUSD</span>
          </div>
          <div className="balance-meta">
            {formatMachineUsd(paidConnection.mpp.spent)} spent of {formatMachineUsd(paidConnection.mpp.limit)} daily limit
          </div>
        </div> : null}

        <div className="action-grid" aria-label="Connection actions">
          {mppReady ? <article className="action-card">
            <div>
              <strong>{mercatorReady ? "Add machineUSD" : "Unlock Mercator"}</strong>
              <p>Buy MACHUSD in this dialog with the embedded headless onramp.</p>
            </div>
            <button
              className="secondary-button"
              data-testid="fund-button"
              disabled={isMutating}
              onClick={onFund}
              type="button"
            >
              {mercatorReady ? "Add $5.00" : "Buy MACHUSD"}
            </button>
          </article> : null}
          <article className="action-card">
            <div>
              <strong>Explicit MPP boundary</strong>
              <p>Mercator can spend at most 0.25 MACHUSD per request and 10.00 per day.</p>
            </div>
          </article>
        </div>
      </div>

      <details className="grant-details">
        <summary>Grant details</summary>
        <dl className="details">
          <Detail label="Account" testId="account-address" value={connection.accountAddress} />
          <Detail label="Grant" testId="grant-id" value={connection.grant.id} />
          <Detail label="Capabilities" value={presentGrantCapabilities(connection.grant.capabilities).join(" · ")} />
          {paidConnection && mppReady ? <Detail
            label="Model settlement"
            value={`${formatMachineUsd(paidConnection.mpp.settlementBalance)} ${paidConnection.mpp.settlementSymbol}`}
          /> : null}
          {paidConnection ? (
            <>
              <Detail label="Access key" testId="access-key" value={paidConnection.accessKey.keyId} />
              <Detail label="Witness" testId="witness" value={paidConnection.accessKey.witness} />
              <Detail
                label="Key expiry"
                value={new Date(paidConnection.accessKey.expiry * 1_000).toLocaleDateString()}
              />
              <Detail
                label="MPP permission"
                value={`${formatMachineUsd(paidConnection.mpp.maxPerRequest)} / request · ${formatMachineUsd(paidConnection.mpp.limit)} / day`}
              />
            </>
          ) : <Detail label="Authorization" value="Hosted — no spending or contract authority" />}
        </dl>
      </details>

      <div className="panel-body account-actions">
        <button
          className="secondary-button"
          data-testid="logout-button"
          disabled={isMutating}
          onClick={onLogout}
          type="button"
        >
          Sign out
        </button>
        <button
          className="danger-button"
          data-testid="revoke-button"
          disabled={isMutating}
          onClick={onRevoke}
          type="button"
        >
          Revoke agent grant
        </button>
      </div>
    </>
  );
}

function Detail({ label, testId, value }: Readonly<{ label: string; testId?: string; value: string }>) {
  return (
    <div className="detail-row">
      <dt>{label}</dt>
      <dd className={label === "Capabilities" ? "good" : undefined} data-testid={testId} title={value}>
        {value}
      </dd>
    </div>
  );
}

function formatMachineUsd(value: bigint) {
  const whole = value / MACHINE_USD_ATOMICS;
  const rawFraction = (value % MACHINE_USD_ATOMICS).toString().padStart(6, "0");
  const fraction = rawFraction.replace(/0+$/, "").padEnd(2, "0");
  return `${whole}.${fraction}`;
}

function shortHex(value: string) {
  return value.length > 14 ? `${value.slice(0, 8)}…${value.slice(-5)}` : value;
}

function errorMessage(reason: unknown) {
  if (reason instanceof Error && reason.message) return reason.message;
  return "The account operation failed. Review the event and try again.";
}

function isUserRejection(reason: unknown) {
  return reason instanceof Error && reason.message === "The request was not approved.";
}
