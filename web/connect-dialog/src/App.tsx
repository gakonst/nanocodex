import { Provider, Storage, webAuthn } from "accounts";
import { loadStripe } from "@stripe/stripe-js/pure";
import type { Stripe, StripeElements } from "@stripe/stripe-js";
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Dialog } from "nanocodex/connect";

import { classifyMachineUsdOrder } from "./machineUsdOrder.mjs";
import {
  accountLoginCapabilities,
  appVisibilityPermissions,
  connectApiOrigin,
  registeredApp,
  sanitizeWalletResult,
  signedAppResources,
  usesBrowserLocalWebAuthn,
} from "./connectPolicy.mjs";
import { parentDialog, type WalletRequest } from "./protocol";

const browserLocalWebAuthn = usesBrowserLocalWebAuthn(window.location.origin);
const provider = createProvider(browserLocalWebAuthn);
let browserSession: Promise<void> | undefined;

export async function logoutAccount() {
  await provider.request({ method: "wallet_disconnect" });
}

const connectorIds = ["github", "gmail", "gdrive", "x", "chatgpt"] as const;
const connectorResourcePrefix = "urn:nanocodex:connector:";
const connectorsResourcePrefix = "urn:nanocodex:connectors:";
const nanocodexOrigin = "https://nanocodex.gakonst.workers.dev";
type ConnectorId = typeof connectorIds[number];
type ConnectorStatus = Readonly<{
  connected: boolean;
  account_id?: string | undefined;
  label?: string | undefined;
}>;
type ConnectorStatuses = Partial<Record<ConnectorId, ConnectorStatus>>;
type PendingApproval = Readonly<{
  accountAddress: `0x${string}`;
  apiUrl: string;
  result: unknown;
  requestId: string;
  token: string;
}>;
type ConnectorAttempt = {
  abort: AbortController;
  connector: ConnectorId;
  expiryTimer?: number | undefined;
  popup?: Window | undefined;
  popupCheck?: number | undefined;
  popupClosed?: number | undefined;
  requestId: string;
  token: string;
};
type CeremonyAttempt = Readonly<{ requestId: string }>;

export function App() {
  const subscribe = useCallback(
    (listener: () => void) => parentDialog.subscribe?.(listener) ?? (() => {}),
    [],
  );
  const getSnapshot = useCallback(() => parentDialog.getRequest?.(), []);
  const request = useSyncExternalStore(subscribe, getSnapshot, () => undefined);
  const requestPolicyError = walletRequestPolicyError(request);
  const [ceremonyRequestId, setCeremonyRequestId] = useState<string>();
  const [failure, setFailure] = useState<Readonly<{ id: string; message: string }>>();
  const [accountMode, setAccountMode] = useState<"login" | "register">("login");
  const [pendingApproval, setPendingApproval] = useState<PendingApproval>();
  const [connectorStatuses, setConnectorStatuses] = useState<ConnectorStatuses>();
  const [connectorAction, setConnectorAction] = useState<ConnectorId>();
  const [deviceCode, setDeviceCode] = useState<Readonly<{
    code: string;
    expiresAt?: number | undefined;
    url: string;
  }>>();
  const activeConnector = useRef<ConnectorAttempt | undefined>(undefined);
  const activeCeremony = useRef<CeremonyAttempt | undefined>(undefined);
  const currentRequestId = useRef<string | undefined>(undefined);
  currentRequestId.current = request?.id;

  const finishConnectorAttempt = useCallback((attempt: ConnectorAttempt, closePopup = true) => {
    if (activeConnector.current !== attempt) return false;
    activeConnector.current = undefined;
    attempt.abort.abort();
    if (attempt.expiryTimer !== undefined) window.clearTimeout(attempt.expiryTimer);
    if (attempt.popupCheck !== undefined) window.clearInterval(attempt.popupCheck);
    if (attempt.popupClosed !== undefined) window.clearTimeout(attempt.popupClosed);
    if (closePopup && attempt.popup && !attempt.popup.closed) attempt.popup.close();
    setConnectorAction(undefined);
    return true;
  }, []);

  useEffect(() => {
    const previous = activeConnector.current;
    if (previous) finishConnectorAttempt(previous);
    setAccountMode("login");
    setPendingApproval(undefined);
    setConnectorStatuses(undefined);
    setConnectorAction(undefined);
    setDeviceCode(undefined);
  }, [request?.id, finishConnectorAttempt]);

  useEffect(() => () => {
    const attempt = activeConnector.current;
    if (attempt) {
      activeConnector.current = undefined;
      attempt.abort.abort();
      if (attempt.expiryTimer !== undefined) window.clearTimeout(attempt.expiryTimer);
      if (attempt.popupCheck !== undefined) window.clearInterval(attempt.popupCheck);
      if (attempt.popupClosed !== undefined) window.clearTimeout(attempt.popupClosed);
      if (attempt.popup && !attempt.popup.closed) attempt.popup.close();
    }
  }, []);

  useEffect(() => {
    if (!pendingApproval) return;
    const onMessage = (event: MessageEvent<unknown>) => {
      const attempt = activeConnector.current;
      if (
        !attempt
        || attempt.connector === "chatgpt"
        || event.origin !== pendingApproval.apiUrl
        || event.source !== attempt.popup
        || !isConnectorCompletion(event.data)
        || event.data.connector !== attempt.connector
      ) return;
      if (event.data.result !== "success") {
        if (finishConnectorAttempt(attempt)) {
          setFailure({
            id: attempt.requestId,
            message: event.data.error ?? event.data.message ?? "The account provider did not complete the connection.",
          });
        }
        return;
      }
      stopPopupMonitor(attempt);
      void (async () => {
        try {
          const state = await refreshConnectors(pendingApproval);
          if (!state.connectors[attempt.connector]?.connected) {
            throw new Error("The account provider completed without connecting the requested account.");
          }
        } catch (error) {
          if (activeConnector.current === attempt) {
            setFailure({ id: attempt.requestId, message: errorMessage(error) });
          }
        } finally {
          finishConnectorAttempt(attempt);
        }
      })();
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [pendingApproval, finishConnectorAttempt]);

  useEffect(() => {
    if (!request) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || ceremonyRequestId === request.id) return;
      event.preventDefault();
      reject();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [request?.id, ceremonyRequestId]);

  useEffect(() => {
    if (!request || !requestPolicyError) return;
    void parentDialog.reject(new Error(requestPolicyError));
  }, [request?.id, requestPolicyError]);

  useEffect(() => {
    if (
      !request
      || request.type !== "walletConnect"
      || accountMode !== "register"
      || browserLocalWebAuthn
    ) return;
    void ensureBrowserSession().catch((error) => {
      if (currentRequestId.current === request.id) {
        setFailure({ id: request.id, message: errorMessage(error) });
      }
    });
  }, [request?.id, accountMode]);

  useEffect(() => {
    if (
      !pendingApproval
      || !connectorStatuses?.chatgpt?.connected
      || ceremonyRequestId === pendingApproval.requestId
      || connectorAction
    ) return;
    const completed = pendingApproval;
    setPendingApproval(undefined);
    void parentDialog.respond(completed.result);
  }, [connectorAction, connectorStatuses?.chatgpt?.connected, pendingApproval, ceremonyRequestId]);

  if (!request || requestPolicyError) return null;

  const ceremonyActive = ceremonyRequestId === request.id;

  async function approve() {
    const activeRequest = request;
    if (!activeRequest || activeCeremony.current) return;
    setFailure(undefined);
    if (activeRequest.type === "machineUsdFund") return;

    const attempt: CeremonyAttempt = { requestId: activeRequest.id };
    activeCeremony.current = attempt;
    setCeremonyRequestId(activeRequest.id);
    try {
      if (
        activeRequest.type === "walletConnect"
        && accountMode === "register"
        && !browserLocalWebAuthn
      ) {
        await ensureBrowserSession();
      }
      const result = await provider.request(
        (activeRequest.type === "walletConnect"
          ? walletRequest(activeRequest, accountMode)
          : activeRequest.rpc) as never,
      ) as undefined | { accounts: readonly Readonly<{ address: `0x${string}` }>[] };
      if (currentRequestId.current !== attempt.requestId) {
        throw new DOMException("The Connect request changed.", "AbortError");
      }
      if (activeRequest.type === "walletConnect" && !result?.accounts[0]) {
        throw new Error("Accounts did not return a connected account.");
      }
      if (activeRequest.type === "walletConnect") {
        const account = result!.accounts[0] as Readonly<{
          address: `0x${string}`;
          capabilities?: Readonly<{ auth?: Readonly<{
            connectors?: ConnectorStatuses;
            profile?: Readonly<{ linked?: boolean }>;
            token?: string;
          }> }>;
        }>;
        const auth = account.capabilities?.auth;
        const token = auth?.token;
        if (!token) throw new Error("Accounts did not return an authenticated Connect session.");
        const next: PendingApproval = {
          accountAddress: account.address,
          apiUrl: connectApiUrl(activeRequest),
          result: sanitizeWalletResult(result),
          requestId: activeRequest.id,
          token,
        };
        if (auth?.connectors && auth.profile?.linked === true) {
          setConnectorStatuses(auth.connectors);
          if (auth.connectors.chatgpt?.connected) {
            await parentDialog.respond(next.result);
            return;
          }
          setPendingApproval(next);
          return;
        }
        const connectors = await authorizeNanocodexAccount(next);
        if (connectors.chatgpt?.connected) {
          await parentDialog.respond(next.result);
          return;
        }
        setPendingApproval(next);
        return;
      }
      await parentDialog.respond(result);
    } catch (error) {
      if (currentRequestId.current === attempt.requestId) {
        setFailure({
          id: activeRequest.id,
          message: activeRequest.type === "walletConnect"
            ? connectCeremonyError(error, accountMode)
            : errorMessage(error),
        });
      }
    } finally {
      if (activeCeremony.current === attempt) {
        activeCeremony.current = undefined;
        setCeremonyRequestId(undefined);
      }
    }
  }

  async function refreshConnectors(approval: PendingApproval) {
    const response = await fetch(`${approval.apiUrl}/v1/connectors`, {
      headers: { authorization: `Bearer ${approval.token}` },
    });
    const body = await response.json() as Readonly<{
      connectors?: ConnectorStatuses;
      profile?: Readonly<{ linked?: boolean }>;
    }> & Record<string, any>;
    if (!response.ok || !body.connectors) {
      throw new Error(apiError(body, "Unable to read connected accounts."));
    }
    if (currentRequestId.current !== approval.requestId) {
      throw new DOMException("The Connect request changed.", "AbortError");
    }
    setConnectorStatuses(body.connectors);
    if (body.connectors.chatgpt?.connected) setDeviceCode(undefined);
    return { connectors: body.connectors };
  }

  async function authorizeNanocodexAccount(approval: PendingApproval): Promise<ConnectorStatuses> {
    const start = await fetch(`${approval.apiUrl}/v1/account-link`, {
      method: "POST",
      headers: { authorization: `Bearer ${approval.token}` },
    });
    const started = await start.json() as Record<string, unknown>;
    if (!start.ok) throw new Error(apiError(started, "Unable to authorize your Nanocodex account."));
    const authorizationUrl = new URL(requiredUrl(started.authorization_url));
    const state = opaqueToken(started.state, "account-link state");
    if (authorizationUrl.origin !== nanocodexOrigin
      || authorizationUrl.pathname !== "/v1/connect/account-link"
      || authorizationUrl.searchParams.get("state") !== state) {
      throw new Error("The Nanocodex account authorization is invalid.");
    }

    authorizationUrl.pathname = "/v1/connect/account-link/authorize";
    const authorize = await fetch(authorizationUrl, {
      method: "POST",
      credentials: "same-origin",
      headers: { accept: "application/json" },
    });
    const authorized = await authorize.json() as Record<string, unknown>;
    if (!authorize.ok) throw new Error(apiError(authorized, "Unable to authorize your Nanocodex account."));
    const code = opaqueToken(authorized.code, "account-link code");
    if (opaqueToken(authorized.state, "account-link state") !== state) {
      throw new Error("The Nanocodex account authorization state changed.");
    }

    const complete = await fetch(`${approval.apiUrl}/v1/account-link`, {
      method: "PUT",
      headers: {
        authorization: `Bearer ${approval.token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ code, state }),
    });
    const completed = await complete.json() as Readonly<{
      connectors?: ConnectorStatuses;
      linked?: boolean;
    }> & Record<string, any>;
    if (!complete.ok || completed.linked !== true || !completed.connectors) {
      throw new Error(apiError(completed, "Unable to authorize your Nanocodex account."));
    }
    if (currentRequestId.current !== approval.requestId) {
      throw new DOMException("The Connect request changed.", "AbortError");
    }
    setConnectorStatuses(completed.connectors);
    if (completed.connectors.chatgpt?.connected) setDeviceCode(undefined);
    return completed.connectors;
  }

  async function connectConnector(id: ConnectorId) {
    if (
      !pendingApproval
      || ceremonyActive
      || connectorAction
      || !connectorStatuses
      || connectorStatuses[id]?.connected
    ) return;
    setFailure(undefined);
    const popup = id === "chatgpt" ? undefined : window.open("about:blank", "nanocodex-connect-oauth", "popup,width=520,height=720");
    if (id !== "chatgpt" && !popup) {
      setFailure({ id: pendingApproval.requestId, message: "The account authorization popup was blocked. Allow popups and try again." });
      return;
    }
    const attempt: ConnectorAttempt = {
      abort: new AbortController(),
      connector: id,
      popup: popup ?? undefined,
      requestId: pendingApproval.requestId,
      token: crypto.randomUUID(),
    };
    activeConnector.current = attempt;
    setConnectorAction(id);
    if (id !== "chatgpt") monitorPopup(attempt);
    try {
      const response = await fetch(`${pendingApproval.apiUrl}/v1/connectors/${id}`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${pendingApproval.token}`,
          "content-type": "application/json",
        },
        body: "{}",
        signal: attempt.abort.signal,
      });
      const body = await response.json() as Record<string, unknown>;
      if (!isActiveConnector(activeConnector.current, attempt, currentRequestId.current)) return;
      if (!response.ok) throw new Error(apiError(body, `Unable to connect ${id}.`));
      if (id === "chatgpt") {
        const url = requiredUrl(body.verification_url);
        const code = requiredText(body.user_code, "ChatGPT device code");
        const expiresAt = requiredExpiry(body.expires_at);
        setDeviceCode({
          code,
          url,
          expiresAt,
        });
        attempt.expiryTimer = window.setTimeout(() => {
          if (finishConnectorAttempt(attempt)) {
            setDeviceCode(undefined);
            setFailure({ id: attempt.requestId, message: "The ChatGPT device code expired. Try again." });
          }
        }, expiresAt - Date.now());
        const chatGptPopup = window.open(url, "nanocodex-connect-chatgpt", "popup,width=520,height=720");
        if (!chatGptPopup) {
          setFailure({ id: attempt.requestId, message: "The ChatGPT popup was blocked. Open the verification link below to continue." });
        } else attempt.popup = chatGptPopup;
        void pollChatGpt(attempt, pendingApproval, expiresAt, pollDelay(body.poll_after_ms));
        return;
      }
      const authorizationUrl = requiredUrl(body.authorization_url);
      popup!.location.href = authorizationUrl;
    } catch (error) {
      if (finishConnectorAttempt(attempt) && !isAbortError(error)) {
        setFailure({ id: attempt.requestId, message: errorMessage(error) });
      }
    }
  }

  async function pollChatGpt(
    attempt: ConnectorAttempt,
    approval: PendingApproval,
    expiresAt: number,
    initialDelay: number,
  ) {
    let delay = initialDelay;
    try {
      for (;;) {
        const remaining = expiresAt - Date.now();
        if (remaining <= 0) throw new Error("The ChatGPT device code expired. Try again.");
        await abortableDelay(Math.min(delay, remaining), attempt.abort.signal);
        if (!isActiveConnector(activeConnector.current, attempt, currentRequestId.current)) return;
        const response = await fetch(`${approval.apiUrl}/v1/connectors/chatgpt`, {
          headers: { authorization: `Bearer ${approval.token}` },
          signal: attempt.abort.signal,
        });
        const body = await response.json() as Record<string, unknown>;
        if (!isActiveConnector(activeConnector.current, attempt, currentRequestId.current)) return;
        if (Date.now() >= expiresAt) throw new Error("The ChatGPT device code expired. Try again.");
        if (response.ok && body.connected === true) {
          await refreshConnectors(approval);
          return;
        }
        if (response.status !== 202) {
          throw new Error(apiError(body, "ChatGPT connection failed."));
        }
        delay = pollDelay(body.poll_after_ms);
      }
    } catch (error) {
      if (!isAbortError(error) && activeConnector.current === attempt) {
        setDeviceCode(undefined);
        setFailure({ id: attempt.requestId, message: errorMessage(error) });
      }
    } finally {
      finishConnectorAttempt(attempt);
    }
  }

  function monitorPopup(attempt: ConnectorAttempt) {
    attempt.popupCheck = window.setInterval(() => {
      if (activeConnector.current !== attempt || !attempt.popup?.closed) return;
      window.clearInterval(attempt.popupCheck);
      attempt.popupCheck = undefined;
      attempt.popupClosed = window.setTimeout(() => {
        if (finishConnectorAttempt(attempt, false)) {
          setFailure({ id: attempt.requestId, message: "The account authorization popup was closed before it completed." });
        }
      }, 750);
    }, 300);
  }

  function stopPopupMonitor(attempt: ConnectorAttempt) {
    if (attempt.popupCheck !== undefined) window.clearInterval(attempt.popupCheck);
    if (attempt.popupClosed !== undefined) window.clearTimeout(attempt.popupClosed);
    attempt.popupCheck = undefined;
    attempt.popupClosed = undefined;
  }

  function reject() {
    const attempt = activeConnector.current;
    if (attempt) finishConnectorAttempt(attempt);
    setFailure(undefined);
    void parentDialog.reject(new Error("The request was not approved."));
  }

  function selectAccountMode(mode: "login" | "register") {
    if (activeCeremony.current) return;
    setFailure(undefined);
    setAccountMode(mode);
  }

  const approvalDisabled = ceremonyActive;

  return (
    <main className="dialog-shell" data-request={request.type} data-testid="remote-connect-dialog">
      <header className="dialog-header">
        <span className="wordmark">nanocodex/connect</span>
        <span className="secure-label">
          <span aria-hidden="true" /> passkey
        </span>
      </header>

      {request.type === "walletConnect" ? (
        <>
          <div className="dialog-content">
            <ConnectionApproval
              accountMode={accountMode}
              connectorAction={connectorAction}
              connectorStatuses={connectorStatuses}
              disabled={ceremonyActive || connectorAction !== undefined}
              deviceCode={deviceCode}
              onAccountModeChange={selectAccountMode}
              onConnectConnector={connectConnector}
              request={walletView(request)}
            />
            {failure?.id === request.id ? (
              <p className="dialog-error" role="alert">{failure.message}</p>
            ) : null}
          </div>
          <div className="dialog-actions">
            <button type="button" disabled={ceremonyActive} onClick={reject}>Cancel</button>
            {!pendingApproval ? (
              <button
                type="button"
                disabled={approvalDisabled}
                onClick={() => void approve()}
              >
                {accountMode === "login" ? "Continue" : "Create account"}
              </button>
            ) : null}
          </div>
        </>
      ) : request.type === "walletRevokeAccessKey" ? (
        <>
          <div className="dialog-content">
            <RevocationApproval request={request} />
            {failure?.id === request.id ? (
              <p className="dialog-error" role="alert">{failure.message}</p>
            ) : null}
          </div>
          <div className="dialog-actions">
            <button type="button" disabled={ceremonyActive} onClick={reject}>Cancel</button>
            <button type="button" disabled={ceremonyActive} onClick={() => void approve()}>
              Revoke with passkey
            </button>
          </div>
        </>
      ) : (
        <FundingApproval request={request} onReject={reject} />
      )}
    </main>
  );
}

function RevocationApproval({ request }: Readonly<{ request: WalletRequest }>) {
  const params = record(firstParam(request.rpc.params));
  return (
    <>
      <section className="request-title" aria-labelledby="revocation-heading">
        <h1 id="revocation-heading">Revoke agent access</h1>
      </section>
      <section className="detail-section" aria-labelledby="revocation-details">
        <SectionHeading id="revocation-details" label="Revocation" value="One passkey" />
        <div className="permission-rows">
          <PermissionRow label="Account" value={shortAddress(params.address)} />
          <PermissionRow label="Access key" value={shortAddress(params.accessKeyAddress)} />
          <PermissionRow label="Effect" value="Immediate" />
        </div>
      </section>
    </>
  );
}

type ConnectionView = Omit<Dialog.ConnectionRequest, "auth" | "accessKey"> & Readonly<{
  auth: Readonly<{ message?: string; resources: readonly string[] }>;
  accessKey?: Omit<Dialog.ConnectionRequest["accessKey"], "witness"> & Readonly<{ witness?: `0x${string}` }>;
}>;

function ConnectionApproval({
  accountMode,
  connectorAction,
  connectorStatuses,
  disabled,
  deviceCode,
  onAccountModeChange,
  onConnectConnector,
  request,
}: Readonly<{
  accountMode: "login" | "register";
  connectorAction?: ConnectorId | undefined;
  connectorStatuses?: ConnectorStatuses | undefined;
  disabled: boolean;
  deviceCode?: Readonly<{ code: string; expiresAt?: number | undefined; url: string }> | undefined;
  onAccountModeChange(mode: "login" | "register"): void;
  onConnectConnector(id: ConnectorId): void;
  request: ConnectionView;
}>) {
  const appVisibility = appVisibilityPermissions(request.auth.resources);
  return (
    <>
      <section className="consent-hero" aria-labelledby="approval-heading">
        <AppMark name={request.app.name} />
        <div>
          <h1 id="approval-heading">Connect to {request.app.name}</h1>
          <span>{request.accessKey ? "New key" : "Active key"}</span>
        </div>
      </section>

      {!connectorStatuses ? <div className="account-mode" role="group" aria-label="Nanocodex account">
        <button
          type="button"
          aria-pressed={accountMode === "login"}
          disabled={disabled}
          onClick={() => onAccountModeChange("login")}
        >
          Sign in
        </button>
        <button
          type="button"
          aria-pressed={accountMode === "register"}
          disabled={disabled}
          onClick={() => onAccountModeChange("register")}
        >
          Create account
        </button>
      </div> : null}

      {connectorStatuses && !connectorStatuses.chatgpt?.connected ? (
        <div className="connector-prompt" role="status">
          <strong>Connect ChatGPT</strong>
          <span>Required to continue</span>
        </div>
      ) : null}

      <section className="oauth-permissions" aria-label="Requested capabilities">
        <div className="capability-logos" role="list">
          {request.permission.connectors.map((connector) => {
            const id = connector.id as ConnectorId;
            const status = connectorStatuses?.[id];
            const resolved = connectorStatuses !== undefined;
            const label = `${permissionTitle(connector.id, connector.name)}. ${connectorStateLabel(
              status,
              resolved,
            )}. ${connector.detail}`;
            const className = `capability-token ${id === "chatgpt" && resolved && !status?.connected ? "required " : ""}${status?.connected ? "connected" : resolved ? "disconnected" : "unresolved"}`;
            const contents = <>
              <ConnectorLogo id={connector.id} name={connector.name} />
              {resolved ? <span className="connector-state" aria-hidden="true">
                {status?.connected ? "✓" : "+"}
              </span> : null}
            </>;
            return (
              <div className="capability-entry" key={connector.id} role="listitem">
                {resolved && !status?.connected ? (
                  <button
                    aria-label={label}
                    className={`${className} capability-action`}
                    data-tooltip={connectorTooltip(status, connector.detail, resolved)}
                    disabled={disabled || connectorAction !== undefined}
                    onClick={() => onConnectConnector(id)}
                    type="button"
                  >
                    {contents}
                  </button>
                ) : (
                  <div
                    aria-label={label}
                    className={className}
                    data-tooltip={connectorTooltip(status, connector.detail, resolved)}
                    tabIndex={0}
                  >
                    {contents}
                  </div>
                )}
              </div>
            );
          })}
          {request.mpp ? (
            <div
              className="capability-token"
              data-tooltip={`${formatToken(request.mpp.maxPerRequest, request.mpp.symbol)} per request · ${formatToken(request.mpp.limit, request.mpp.symbol)} per day · ${request.accessKey ? expiryLabel(request.accessKey.expiry) : "active grant"}`}
              role="listitem"
              tabIndex={0}
              aria-label={`machineUSD spend permission. ${formatToken(request.mpp.maxPerRequest, request.mpp.symbol)} per request, ${formatToken(request.mpp.limit, request.mpp.symbol)} per day.`}
            >
              <SpendLogo />
            </div>
          ) : null}
        </div>
        {appVisibility.length > 0 ? (
          <div className="app-sees" aria-label="App sees" role="list">
            <span className="app-sees-label" aria-hidden="true">App sees</span>
            {appVisibility.map((permission) => (
              <span
                aria-label={`${permission.label}: ${permission.detail}`}
                className="app-sees-permission"
                data-tooltip={permission.detail}
                key={permission.resource}
                role="listitem"
                tabIndex={0}
              >
                {permission.label}
              </span>
            ))}
          </div>
        ) : null}
      </section>

      {deviceCode ? (
        <a className="device-code" href={deviceCode.url} rel="noreferrer" target="_blank">
          <span>ChatGPT</span>
          <strong>{deviceCode.code}</strong>
        </a>
      ) : null}

      <details className="advanced-details">
        <summary>Details</summary>
        <dl className="key-details">
          <Detail label="App" value={request.app.origin} />
          {request.mpp ? <Detail label="Spend" value={`${formatToken(request.mpp.maxPerRequest, request.mpp.symbol)} / request · ${formatToken(request.mpp.limit, request.mpp.symbol)} / day`} /> : null}
          {request.accessKey ? (
            <>
              <Detail label="Key" value={request.accessKey.keyId} />
              <Detail label="Witness" value={request.accessKey.witness ?? "Bound to the SIWE challenge at approval"} />
              <Detail label="Expires" value={formatExpiry(request.accessKey.expiry)} />
            </>
          ) : <Detail label="Key" value="Reuse the app's active delegated signer" />}
        </dl>
        <ul className="resource-list" aria-label="Connect capability resources">
          {request.auth.resources.map((resource) => <li key={resource}>{resource}</li>)}
        </ul>
        {request.auth.message ? <pre>{request.auth.message}</pre> : null}
      </details>
    </>
  );
}

type FundingAttempt = Readonly<{
  clientSecret: string;
  id: string;
  orderToken: string;
  stripe: Stripe;
}>;

function FundingApproval({ request, onReject }: Readonly<{
  request: Dialog.FundingRequest;
  onReject(): void;
}>) {
  const dollars = (request.usdAmountCents / 100).toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
  const paymentTarget = useRef<HTMLDivElement>(null);
  const [attempt, setAttempt] = useState<FundingAttempt>();
  const [elements, setElements] = useState<StripeElements>();
  const [failure, setFailure] = useState<string>();
  const [busy, setBusy] = useState(false);
  const started = useRef(false);

  useEffect(() => {
    if (started.current || !request.accountAddress) return;
    started.current = true;
    void preparePayment();
  }, [request.id]);

  useEffect(() => {
    if (!attempt || !paymentTarget.current) return;
    const next = attempt.stripe.elements({
      clientSecret: attempt.clientSecret,
      appearance: {
        theme: "night",
        variables: {
          borderRadius: "0px",
          colorBackground: "#161616",
          colorDanger: "#ff8585",
          colorPrimary: "#ffffff",
          colorText: "#ffffff",
          colorTextSecondary: "rgba(255,255,255,.62)",
          fontFamily: "Berkeley Mono, ui-monospace, monospace",
          spacingUnit: "3px",
        },
      },
    });
    const payment = next.create("payment", { layout: "tabs" });
    payment.mount(paymentTarget.current);
    setElements(next);
    return () => {
      payment.destroy();
      setElements(undefined);
    };
  }, [attempt]);

  async function preparePayment() {
    if (!request.accountAddress || busy) return;
    setFailure(undefined);
    setBusy(true);
    try {
      const orderToken = randomToken();
      const response = await fetch(onrampUrl(request.apiUrl, "/v1/machine-usd/orders"), {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "idempotency-key": request.id,
        },
        body: JSON.stringify({
          wallet_address: request.accountAddress,
          usd_amount_cents: request.usdAmountCents,
          order_token: orderToken,
        }),
      });
      const body = await response.json() as Record<string, any>;
      if (!response.ok) throw new Error(apiError(body, "Unable to create the machineUSD order."));
      const stripe = await loadStripe(request.stripePublishableKey);
      if (!stripe) throw new Error("Stripe could not initialize the embedded payment form.");
      if (typeof body.order?.id !== "string" || typeof body.payment?.client_secret !== "string") {
        throw new Error("The machineUSD order response is invalid.");
      }
      setAttempt({
        clientSecret: body.payment.client_secret,
        id: body.order.id,
        orderToken,
        stripe,
      });
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  }

  async function confirmPayment() {
    if (!attempt || !elements || busy) return;
    setFailure(undefined);
    setBusy(true);
    try {
      const submitted = await elements.submit();
      if (submitted.error) throw new Error(submitted.error.message);
      const confirmed = await attempt.stripe.confirmPayment({
        elements,
        clientSecret: attempt.clientSecret,
        confirmParams: { return_url: window.location.href },
        redirect: "if_required",
      });
      if (confirmed.error) throw new Error(confirmed.error.message);
      const order = await waitForOrder(request.apiUrl, attempt);
      await parentDialog.respond({
        order: {
          id: order.id,
          status: order.status,
          usd_amount_cents: order.usd_amount_cents,
          machine_usd_amount_atomics: String(order.usd_amount_cents * 10_000),
          issuance_transaction_hash: order.issuance_transaction_hash,
        },
      });
    } catch (error) {
      setFailure(errorMessage(error));
      setBusy(false);
    }
  }

  return (
    <>
      <div className="dialog-content">
        <section className="request-title" aria-labelledby="approval-heading">
          <h1 id="approval-heading">Add MACHUSD</h1>
        </section>

        <section className="onramp-card" aria-label="machineUSD card onramp">
          <div className="card-topline">
            <span>machineUSD</span>
            <span className="card-method">CARD</span>
          </div>
          <div className="funding-amount"><span>$</span>{dollars}</div>
          {attempt ? <div className="stripe-payment-element" ref={paymentTarget} /> : (
            <dl className="funding-details">
              <Detail label="Grant" value={request.grantId} />
              <Detail label="Token" value={request.tokenAddress} />
              <Detail label="Network" value={`Tempo · ${request.chainId}`} />
              {request.accountAddress ? <Detail label="Account" value={request.accountAddress} /> : null}
            </dl>
          )}
        </section>

        {failure ? <p className="dialog-error" role="alert">{failure}</p> : null}
      </div>
      <div className="dialog-actions">
        <button type="button" disabled={busy} onClick={onReject}>Cancel</button>
        <button
          type="button"
          disabled={busy || (attempt ? !elements : !request.accountAddress)}
          onClick={attempt ? confirmPayment : preparePayment}
        >
          {attempt ? `Pay $${dollars}` : failure ? "Try again" : "Secure card form"}
        </button>
      </div>
    </>
  );
}

async function waitForOrder(apiUrl: string, attempt: FundingAttempt) {
  for (;;) {
    const response = await fetch(onrampUrl(apiUrl, `/v1/machine-usd/orders/${encodeURIComponent(attempt.id)}`), {
      headers: { authorization: `Bearer ${attempt.orderToken}` },
    });
    const body = await response.json() as Record<string, any>;
    if (!response.ok) throw new Error(apiError(body, "Unable to read the machineUSD order."));
    const order = body.order;
    const status = classifyMachineUsdOrder(order);
    if (status === "complete") return order;
    if (status === "failed") {
      throw new Error("The machineUSD purchase did not complete.");
    }
    await new Promise((resolve) => window.setTimeout(resolve, 1_500));
  }
}

function onrampUrl(apiUrl: string, path: string) {
  return `${apiUrl.replace(/\/+$/, "")}${path}`;
}

function randomToken() {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function apiError(body: Record<string, any>, fallback: string) {
  return typeof body.error?.message === "string" ? body.error.message : fallback;
}

function SectionHeading({ id, label, value }: Readonly<{ id: string; label: string; value: string }>) {
  return (
    <div className="section-heading">
      <h2 id={id}>{label}</h2>
      <span>{value}</span>
    </div>
  );
}

function AppMark({ name }: Readonly<{ name: string }>) {
  return <span className="app-mark" aria-hidden="true">{name.slice(0, 1).toUpperCase()}</span>;
}

function ConnectorLogo({ id, name }: Readonly<{ id: string; name: string }>) {
  if (id === "chatgpt" || id === "model") {
    return (
      <span className="connector-logo connector-logo-openai" aria-hidden="true">
        <svg viewBox="146 227 268 265" role="presentation">
          <path d="M249.176 323.434V298.276C249.176 296.158 249.971 294.569 251.825 293.509L302.406 264.381C309.29 260.409 317.5 258.555 325.973 258.555C357.75 258.555 377.877 283.185 377.877 309.399C377.877 311.253 377.877 313.371 377.611 315.49L325.178 284.771C322.001 282.919 318.822 282.919 315.645 284.771L249.176 323.434ZM367.283 421.415V361.301C367.283 357.592 365.694 354.945 362.516 353.092L296.048 314.43L317.763 301.982C319.617 300.925 321.206 300.925 323.058 301.982L373.639 331.112C388.205 339.586 398.003 357.592 398.003 375.069C398.003 395.195 386.087 413.733 367.283 421.412V421.415ZM233.553 368.452L211.838 355.742C209.986 354.684 209.19 353.095 209.19 350.975V292.718C209.19 264.383 230.905 242.932 260.301 242.932C271.423 242.932 281.748 246.641 290.49 253.26L238.321 283.449C235.146 285.303 233.555 287.951 233.555 291.659V368.455L233.553 368.452ZM280.292 395.462L249.176 377.985V340.913L280.292 323.436L311.407 340.913V377.985L280.292 395.462ZM300.286 475.968C289.163 475.968 278.837 472.259 270.097 465.64L322.264 435.449C325.441 433.597 327.03 430.949 327.03 427.239V350.445L349.011 363.155C350.865 364.213 351.66 365.802 351.66 367.922V426.179C351.66 454.514 329.679 475.965 300.286 475.965V475.968ZM237.525 416.915L186.944 387.785C172.378 379.31 162.582 361.305 162.582 343.827C162.582 323.436 174.763 305.164 193.563 297.485V357.861C193.563 361.571 195.154 364.217 198.33 366.071L264.535 404.467L242.82 416.915C240.967 417.972 239.377 417.972 237.525 416.915ZM234.614 460.343C204.689 460.343 182.71 437.833 182.71 410.028C182.71 407.91 182.976 405.792 183.238 403.672L235.405 433.863C238.582 435.715 241.763 435.715 244.938 433.863L311.407 395.466V420.622C311.407 422.742 310.612 424.331 308.758 425.389L258.179 454.519C251.293 458.491 243.083 460.343 234.611 460.343H234.614ZM300.286 491.854C332.329 491.854 359.073 469.082 365.167 438.892C394.825 431.211 413.892 403.406 413.892 375.073C413.892 356.535 405.948 338.529 391.648 325.552C392.972 319.991 393.766 314.43 393.766 308.87C393.766 271.003 363.048 242.666 327.562 242.666C320.413 242.666 313.528 243.723 306.644 246.109C294.725 234.457 278.307 227.042 260.301 227.042C228.258 227.042 201.513 249.815 195.42 280.004C165.761 287.685 146.694 315.49 146.694 343.824C146.694 362.362 154.638 380.368 168.938 393.344C167.613 398.906 166.819 404.467 166.819 410.027C166.819 447.894 197.538 476.231 233.024 476.231C240.172 476.231 247.058 475.173 253.943 472.788C265.859 484.441 282.278 491.854 300.286 491.854Z" />
        </svg>
      </span>
    );
  }
  if (id === "github") {
    return (
      <span className="connector-logo connector-logo-github" aria-hidden="true">
        <svg viewBox="0 0 24 24" role="presentation">
          <path d="M12 .7a11.5 11.5 0 0 0-3.64 22.41c.58.1.79-.25.79-.56v-2.23c-3.22.7-3.9-1.37-3.9-1.37-.53-1.34-1.29-1.7-1.29-1.7-1.05-.72.08-.71.08-.71 1.17.08 1.78 1.2 1.78 1.2 1.04 1.78 2.72 1.27 3.38.97.1-.75.41-1.27.74-1.56-2.57-.29-5.27-1.29-5.27-5.69 0-1.26.45-2.29 1.19-3.09-.12-.29-.52-1.47.11-3.05 0 0 .97-.31 3.16 1.18A10.9 10.9 0 0 1 12 6.11c.98 0 1.95.13 2.87.39 2.19-1.49 3.15-1.18 3.15-1.18.63 1.58.23 2.76.11 3.05.74.8 1.19 1.83 1.19 3.09 0 4.42-2.71 5.39-5.29 5.68.42.36.79 1.06.79 2.14v3.27c0 .31.21.67.8.56A11.5 11.5 0 0 0 12 .7Z" />
        </svg>
      </span>
    );
  }
  if (id === "gmail") {
    return (
      <span className="connector-logo connector-logo-gmail" aria-hidden="true">
        <svg viewBox="0 0 24 18" role="presentation">
          <path fill="#4285f4" d="M1.7 18H5V6.4L0 2.65v13.7C0 17.26.74 18 1.7 18Z" />
          <path fill="#34a853" d="M19 18h3.3c.96 0 1.7-.74 1.7-1.65V2.65L19 6.4V18Z" />
          <path fill="#fbbc04" d="M19 6.4 24 2.65V1.8C24-.23 21.68-.9 20.23.18L19 1.1v5.3Z" />
          <path fill="#ea4335" d="M5 6.4V1.1L12 6.35l7-5.25v5.3l-7 5.25L5 6.4Z" />
          <path fill="#c5221f" d="M0 1.8v.85L5 6.4V1.1L3.77.18C2.32-.9 0-.23 0 1.8Z" />
        </svg>
      </span>
    );
  }
  if (id === "gdrive") {
    return (
      <span className="connector-logo connector-logo-drive" aria-hidden="true">
        <svg viewBox="0 0 24 22" role="presentation">
          <path fill="#0f9d58" d="M8.2 14.7 4.1 22h11.7l4.1-7.3H8.2Z" />
          <path fill="#ffcd40" d="m16 0 8 14.7h-8L8 0h8Z" />
          <path fill="#4285f4" d="M8 0 0 14.7 4.1 22 12 7.3 8 0Z" />
        </svg>
      </span>
    );
  }
  if (id === "x") {
    return <span className="connector-logo connector-logo-x" aria-hidden="true">X</span>;
  }
  return (
    <span className="connector-logo connector-logo-nanocodex" aria-hidden="true" title={name}>
      <svg viewBox="0 0 24 24" role="presentation">
        <path d="M10.4 3h3.2v7.4H21v3.2h-7.4V21h-3.2v-7.4H3v-3.2h7.4V3Z" />
      </svg>
    </span>
  );
}

function SpendLogo() {
  return (
    <span className="connector-logo connector-logo-spend" aria-hidden="true">
      <span>M</span>
      <i>≤</i>
    </span>
  );
}

function permissionTitle(id: string, fallback: string) {
  if (id === "github") return "GitHub";
  if (id === "gmail") return "Gmail";
  if (id === "gdrive") return "Google Drive";
  if (id === "x") return "X";
  if (id === "chatgpt" || id === "model") return "ChatGPT";
  return fallback;
}

function connectorStateLabel(status: ConnectorStatus | undefined, resolved: boolean) {
  if (!resolved) return "Requested";
  if (!status?.connected) return "Not connected";
  return status.label ? `Connected as ${status.label}` : "Connected";
}

function connectorTooltip(status: ConnectorStatus | undefined, detail: string, resolved: boolean) {
  return `${connectorStateLabel(status, resolved)} · ${detail}`;
}

function Detail({ label, value }: Readonly<{ label: string; value: string }>) {
  return (
    <div>
      <dt>{label}</dt>
      <dd title={value}>{value}</dd>
    </div>
  );
}

function PermissionRow({ label, value }: Readonly<{ label: string; value: string }>) {
  return (
    <div className="permission-row">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function formatExpiry(expiry: number) {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
    timeZone: "UTC",
  }).format(new Date(expiry * 1_000)) + " UTC";
}

function expiryLabel(expiry: number) {
  const days = Math.max(1, Math.round((expiry * 1_000 - Date.now()) / 86_400_000));
  return `${days} day expiry`;
}

function formatToken(atomics: bigint, symbol: string) {
  const whole = atomics / 1_000_000n;
  const fractional = (atomics % 1_000_000n).toString().padStart(6, "0").replace(/0+$/, "");
  const amount = fractional ? `${whole}.${fractional}` : whole.toString();
  return `${amount} ${symbol}`;
}

function formatPeriod(seconds: number) {
  if (seconds === 86_400) return "24 hours";
  if (seconds % 3_600 === 0) return `${seconds / 3_600} hours`;
  return `${seconds} seconds`;
}

function shortAddress(value: unknown) {
  return typeof value === "string" && value.length > 15
    ? `${value.slice(0, 8)}…${value.slice(-6)}`
    : "Unavailable";
}

function walletRequest(request: WalletRequest, accountMode: "login" | "register") {
  const params = record(firstParam(request.rpc.params));
  const capabilities = record(params.capabilities);
  const { resources } = walletConnectContext(request);
  const {
    credentialId: _credentialId,
    method: _method,
    name: _name,
    selectAccount: _selectAccount,
    userId: _userId,
    ...sharedCapabilities
  } = capabilities;
  const apiUrl = connectApiUrl(request);
  const walletAuth = (() => {
    const auth = capabilities.auth;
    if (!auth) return auth;
    if (typeof auth === "string") {
      return { url: auth, verify: `${apiUrl}/v1/connect/auth` };
    }
    const forwarded = record(auth);
    return {
      ...forwarded,
      verify: `${apiUrl}/v1/connect/auth`,
      resources,
    };
  })();
  return {
    ...request.rpc,
    params: [{
      ...params,
      capabilities: {
        ...sharedCapabilities,
        ...(accountMode === "login"
          ? accountLoginCapabilities(storedProviderAccounts())
          : { method: "register", name: "Nanocodex Connect" }),
        ...(walletAuth ? { auth: walletAuth } : {}),
      },
    }],
  };
}

function storedProviderAccounts(): unknown {
  return (provider as unknown as {
    store: { getState(): { accounts: unknown } };
  }).store.getState().accounts;
}

function createProvider(browserLocal: boolean) {
  return Provider.create({
    adapter: webAuthn(browserLocal
      ? {
          name: "Nanocodex",
          rdns: "xyz.paradigm.nanocodex",
        }
      : {
          auth: "/webauthn",
          name: "Nanocodex",
          rdns: "xyz.paradigm.nanocodex",
        }),
    maxAccounts: 1,
    mpp: false,
    storage: Storage.idb({ key: "nanocodex" }),
  });
}

async function ensureBrowserSession() {
  if (browserSession) return browserSession;
  const attempt = (async () => {
    const response = await fetch("/v1/me", {
      cache: "no-store",
      credentials: "same-origin",
      headers: { accept: "application/json" },
    });
    const body: unknown = await response.json().catch(() => undefined);
    if (!response.ok) {
      throw new Error(apiError(record(body), "Unable to start a Nanocodex browser session."));
    }
    if (
      !isRecord(body)
      || !isRecord(body.user)
      || typeof body.user.id !== "string"
      || typeof body.user.persistent !== "boolean"
    ) {
      throw new Error("The Nanocodex account service returned an invalid browser session.");
    }
  })();
  browserSession = attempt;
  try {
    await attempt;
  } catch (error) {
    if (browserSession === attempt) browserSession = undefined;
    throw error;
  }
}

function walletView(request: WalletRequest): ConnectionView {
  const params = record(firstParam(request.rpc.params));
  const capabilities = record(params.capabilities);
  const { app, resources } = walletConnectContext(request);
  const requestedConnectors = requestedConnectorIdsFromResources(resources);
  const access = record(capabilities.authorizeAccessKey);
  const limits = array(access.limits).map((value) => {
    const limit = record(value);
    return {
      token: hex(limit.token),
      limit: BigInt(String(limit.limit)),
      ...(typeof limit.period === "number" ? { period: limit.period } : {}),
    };
  });
  const scopes = array(access.scopes).map((value) => {
    const scope = record(value);
    return {
      address: hex(scope.address),
      ...(typeof scope.selector === "string" ? { selector: scope.selector } : {}),
      ...(Array.isArray(scope.recipients) ? { recipients: scope.recipients.map(hex) } : {}),
    };
  });
  const primary = limits[0] ?? {
    token: "0x20c0000000000000000000006637932dE5413804" as const,
    limit: 10_000_000n,
    period: 86_400,
  };
  const preparedAccessKey = typeof access.address === "string" && typeof access.publicKey === "string"
    ? {
        address: hex(access.address),
        chainId: BigInt(String(access.chainId ?? params.chainId ?? "0x1079")),
        keyId: hex(access.address),
        publicKey: hex(access.publicKey),
        keyType: access.keyType === "webAuthn" || access.keyType === "secp256k1" ? access.keyType : "p256" as const,
        limits,
        scopes,
        expiry: Number(access.expiry),
      }
    : undefined;
  return {
    id: request.id,
    type: "connect",
    app,
    accountAddress: "0x0000000000000000000000000000000000000000",
    auth: {
      resources,
    },
    permission: {
      id: "agent.run",
      title: "Use your Nanocodex agent",
      description: "Run an app-owned Nanocodex agent with your approved capabilities.",
      connectors: requestedConnectors.map(connectorDefinition),
    },
    ...(preparedAccessKey ? { accessKey: preparedAccessKey } : {}),
    ...(resources.includes("urn:nanocodex:mpp:machusd:spend") ? {
      mpp: {
        token: primary.token,
        symbol: "MACHUSD",
        limit: primary.limit,
        period: primary.period ?? 86_400,
        maxPerRequest: 250_000n,
      },
    } : {}),
  };
}

function requestedConnectorIdsFromResources(resources: readonly string[]): ConnectorId[] {
  return [...new Set(resources.flatMap((resource) => {
    if (resource.startsWith(connectorResourcePrefix)) {
      return [resource.slice(connectorResourcePrefix.length)];
    }
    if (resource.startsWith(connectorsResourcePrefix)) {
      return resource.slice(connectorsResourcePrefix.length).split(",");
    }
    return [];
  }).filter(isConnectorId))];
}

function connectorDefinition(id: ConnectorId) {
  if (id === "github") return { id, name: "GitHub", detail: "Repositories and workflows" };
  if (id === "gmail") return { id, name: "Gmail", detail: "Read and send email" };
  if (id === "gdrive") return { id, name: "Google Drive", detail: "Read and create files" };
  if (id === "x") return { id, name: "X", detail: "Posts, follows, likes, lists, and messages" };
  return { id, name: "ChatGPT", detail: "Model access through your account" };
}

function isConnectorId(value: string): value is ConnectorId {
  return (connectorIds as readonly string[]).includes(value);
}

function connectApiUrl(request: WalletRequest) {
  const params = record(firstParam(request.rpc.params));
  return connectApiOrigin(record(params.capabilities).auth, window.location.origin);
}

function walletConnectContext(request: WalletRequest) {
  const params = record(firstParam(request.rpc.params));
  const auth = record(record(params.capabilities).auth);
  const resources = Array.isArray(auth.resources)
    ? auth.resources.filter((value): value is string => typeof value === "string")
    : [];
  const app = registeredApp(
    request.origin,
    request.appId,
    window.location.href,
    window.parent === window,
  );
  signedAppResources(resources, app);
  return { app, resources };
}

function isConnectorCompletion(value: unknown): value is Readonly<{
  type: "nanocodex:connector-complete";
  connector: ConnectorId;
  result: "success" | "error";
  error?: string | undefined;
  message?: string | undefined;
}> {
  return isRecord(value)
    && value.type === "nanocodex:connector-complete"
    && typeof value.connector === "string"
    && isConnectorId(value.connector)
    && (value.result === "success" || value.result === "error")
    && (value.error === undefined || typeof value.error === "string")
    && (value.message === undefined || typeof value.message === "string");
}

function walletRequestPolicyError(request: ReturnType<typeof parentDialog.getRequest>) {
  if (!request || request.type === "machineUsdFund") return undefined;
  try {
    if (request.type === "walletConnect") {
      walletConnectContext(request);
      connectApiUrl(request);
    } else {
      registeredApp(request.origin, request.appId, window.location.href, window.parent === window, false);
    }
    return undefined;
  } catch (error) {
    return errorMessage(error);
  }
}

function isActiveConnector(
  current: ConnectorAttempt | undefined,
  expected: ConnectorAttempt,
  requestId: string | undefined,
) {
  return current === expected
    && current.token === expected.token
    && requestId === expected.requestId
    && !expected.abort.signal.aborted;
}

function abortableDelay(milliseconds: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    if (signal.aborted) {
      reject(signal.reason ?? new DOMException("The connector request was canceled.", "AbortError"));
      return;
    }
    const timeout = window.setTimeout(done, milliseconds);
    signal.addEventListener("abort", canceled, { once: true });
    function done() {
      signal.removeEventListener("abort", canceled);
      resolve();
    }
    function canceled() {
      window.clearTimeout(timeout);
      reject(signal.reason ?? new DOMException("The connector request was canceled.", "AbortError"));
    }
  });
}

function requiredExpiry(value: unknown) {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new Error("The account broker returned no ChatGPT device-code expiry.");
  }
  const milliseconds = value < 1_000_000_000_000 ? value * 1_000 : value;
  if (milliseconds <= Date.now()) throw new Error("The ChatGPT device code has already expired.");
  return milliseconds;
}

function pollDelay(value: unknown) {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 500 && value <= 30_000
    ? value
    : 2_000;
}

function isAbortError(error: unknown) {
  return error instanceof DOMException && error.name === "AbortError";
}

function requiredUrl(value: unknown) {
  if (typeof value !== "string") throw new Error("The account broker returned no authorization URL.");
  const url = new URL(value);
  if (url.protocol !== "https:") throw new Error("The account broker returned an unsafe authorization URL.");
  return url.href;
}

function requiredText(value: unknown, label: string) {
  if (typeof value !== "string" || value.length === 0) throw new Error(`${label} is missing.`);
  return value;
}

function opaqueToken(value: unknown, label: string) {
  if (typeof value !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(value)) {
    throw new Error(`The account broker returned an invalid ${label}.`);
  }
  return value;
}

function record(value: unknown): Record<string, any> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, any>
    : {};
}

function isRecord(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function array(value: unknown): readonly unknown[] {
  return Array.isArray(value) ? value : [];
}

function firstParam(value: unknown) {
  return Array.isArray(value) ? value[0] : undefined;
}

function hex(value: unknown): `0x${string}` {
  if (typeof value !== "string" || !/^0x[0-9a-fA-F]+$/.test(value)) {
    throw new Error("Nanocodex Connect received invalid access-key material.");
  }
  return value as `0x${string}`;
}

function errorMessage(error: unknown) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "The passkey ceremony failed. Try again or reject the request.";
}

function connectCeremonyError(error: unknown, accountMode: "login" | "register") {
  const message = errorMessage(error);
  if (/request is already pending/i.test(message)) {
    return "A passkey prompt is already open. Complete or dismiss it, then try again.";
  }
  if (/failed to request credential|timed out or was not allowed|notallowederror/i.test(message)) {
    return accountMode === "login"
      ? "No passkey was selected. Try again or create an account."
      : "Passkey setup was canceled. Try again.";
  }
  return message;
}
