import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  AccountConnectionCard,
  AccountConnectionGrid,
  McpConnectionAddCard,
  McpConnectionCard,
} from "nanocodex-connect-ui/AccountConnectionSurface";
import { isRecord, responseFailure } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import { announceAccountMcpCatalogChanged } from "./browserMcp";
import { ConnectionLogo } from "nanocodex-connect-ui/ConnectionLogo";
import {
  connectorCompletion,
  connectorCompletionFor,
} from "nanocodex-connect-ui/connectorCompletion";
import {
  connectorCapabilityLabel,
  connectorConnectionsForCapabilities,
  connectorStatusesFromWire,
  googleConnectorCapabilities,
  type ConnectorCapability,
  type ConnectorConnection,
  type ConnectorProvider,
  type ConnectorStatus,
} from "nanocodex-connect-ui/connectorPolicy.mjs";

type AccountConnectorCapability = Exclude<ConnectorCapability, "chatgpt">;
type AccountConnectorProvider = Exclude<ConnectorProvider, "chatgpt">;
type AccountConnectorStatus = ConnectorStatus & Readonly<{ unavailable?: string }>;
type AccountConnectorStatuses = Record<AccountConnectorCapability, AccountConnectorStatus>;
type McpConnectionStatus =
  | "authorization_required"
  | "connected"
  | "reauthorization_required"
  | "disabled"
  | "revoked";
type McpConnection = Readonly<{
  id: string;
  name: string;
  status: McpConnectionStatus;
}>;
type ConnectorAttempt = {
  abort: AbortController;
  provider: AccountConnectorProvider;
  capabilities: readonly AccountConnectorCapability[];
  popup: Window;
  popupCheck: number;
  popupClosed?: number | undefined;
};
type McpAttempt = {
  abort: AbortController;
  connection: McpConnection;
  popup: Window;
  popupCheck: number;
  popupClosed?: number | undefined;
};

const mcpConnectionId = /^[A-Za-z0-9_-]{43}$/;
const mcpConnectionName = /^[^\u0000-\u001f\u007f]{1,256}$/u;
const mcpConnectionStatuses = new Set<McpConnectionStatus>([
  "authorization_required",
  "connected",
  "reauthorization_required",
  "disabled",
  "revoked",
]);

const accountConnectorCapabilities = [
  "github",
  ...googleConnectorCapabilities,
  "slack",
  "x",
] as const satisfies readonly AccountConnectorCapability[];

const connectorDefinitions = [
  { provider: "github", capabilities: ["github"], label: "GitHub", description: "Clone, push, and manage repositories and workflows" },
  { provider: "google", capabilities: googleConnectorCapabilities, label: "Google Workspace", description: "Mail, Drive, Calendar, Tasks, Docs, Sheets, Slides, and Contacts" },
  { provider: "slack", capabilities: ["slack"], label: "Slack", description: "Read and send messages as you in connected workspaces" },
  { provider: "x", capabilities: ["x"], label: "X", description: "Read and publish posts; manage follows, likes, bookmarks, lists, and messages" },
] as const satisfies ReadonlyArray<{
  provider: AccountConnectorProvider;
  capabilities: readonly AccountConnectorCapability[];
  label: string;
  description: string;
}>;

export function ProfileConnectors({
  accountId,
  after,
  children,
  presentation = "profile",
  requiresLogin = false,
  refreshSession,
}: {
  accountId: string;
  after?: ReactNode;
  children?: ReactNode;
  presentation?: "profile" | "wizard";
  requiresLogin?: boolean;
  refreshSession(): Promise<void>;
}) {
  const [connectors, setConnectors] = useState<AccountConnectorStatuses | null>(null);
  const [mcpConnections, setMcpConnections] = useState<readonly McpConnection[] | null>(null);
  const [mcpError, setMcpError] = useState<string | null>(null);
  const [mcpConnectionError, setMcpConnectionError] = useState<Readonly<{
    id: string;
    message: string;
  }> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [operation, setOperation] = useState<string | null>(null);
  const request = useRef<Promise<void> | undefined>(undefined);
  const mcpRequest = useRef<Promise<void> | undefined>(undefined);
  const activeConnector = useRef<ConnectorAttempt | undefined>(undefined);
  const activeMcp = useRef<McpAttempt | undefined>(undefined);
  const [result] = useState(readConnectorResult);
  const [mcpResult] = useState(readMcpResult);

  const finishConnectorAttempt = useCallback((attempt: ConnectorAttempt, closePopup = true) => {
    if (activeConnector.current !== attempt) return false;
    activeConnector.current = undefined;
    attempt.abort.abort();
    window.clearInterval(attempt.popupCheck);
    if (attempt.popupClosed !== undefined) window.clearTimeout(attempt.popupClosed);
    if (closePopup && !attempt.popup.closed) attempt.popup.close();
    setOperation(null);
    return true;
  }, []);

  const finishMcpAttempt = useCallback((attempt: McpAttempt, closePopup = true) => {
    if (activeMcp.current !== attempt) return false;
    activeMcp.current = undefined;
    attempt.abort.abort();
    window.clearInterval(attempt.popupCheck);
    if (attempt.popupClosed !== undefined) window.clearTimeout(attempt.popupClosed);
    if (closePopup && !attempt.popup.closed) attempt.popup.close();
    setOperation(null);
    return true;
  }, []);

  const refreshConnectors = useCallback(async (signal?: AbortSignal) => {
    const response = await connectorRequest("/v1/connectors", { signal });
    if (response.status === 401) {
      await response.body?.cancel();
      await refreshSession();
      return undefined;
    }
    if (!response.ok) throw await responseFailure(response, "Couldn’t load connectors.");
    const statuses = decodeConnectorStatus(await response.json());
    setConnectors(statuses);
    setError(null);
    return statuses;
  }, [refreshSession]);

  const load = useCallback((): Promise<void> => {
    if (request.current) return request.current;
    let current!: Promise<void>;
    current = (async () => {
      try {
        await refreshConnectors();
      } catch (cause) {
        const message = failureMessage(cause, "Couldn’t load connectors.");
        setConnectors(unavailableConnectorStatuses(message));
        setError(null);
      }
    })().finally(() => {
      if (request.current === current) request.current = undefined;
    });
    request.current = current;
    return current;
  }, [refreshConnectors]);

  const loadMcpConnections = useCallback((): Promise<void> => {
    if (mcpRequest.current) return mcpRequest.current;
    let current!: Promise<void>;
    current = (async () => {
      try {
        const response = await connectorRequest("/v1/connectors/mcp-connections");
        if (response.status === 401) {
          await response.body?.cancel();
          await refreshSession();
          return;
        }
        if (!response.ok) throw await responseFailure(response, "Couldn’t load MCP connections.");
        setMcpConnections(decodeMcpConnections(await response.json()));
        announceAccountMcpCatalogChanged();
        setMcpError(null);
      } catch (cause) {
        setMcpError(failureMessage(cause, "Couldn’t load MCP connections."));
      }
    })().finally(() => {
      if (mcpRequest.current === current) mcpRequest.current = undefined;
    });
    mcpRequest.current = current;
    return current;
  }, [refreshSession]);

  useEffect(() => {
    const previous = activeConnector.current;
    if (previous) finishConnectorAttempt(previous);
    const previousMcp = activeMcp.current;
    if (previousMcp) finishMcpAttempt(previousMcp);
    setConnectors(null);
    setMcpConnections(null);
    setMcpError(null);
    setMcpConnectionError(null);
    setError(null);
    if (requiresLogin) return;
    void load();
    void loadMcpConnections();
  }, [accountId, finishConnectorAttempt, finishMcpAttempt, load, loadMcpConnections, requiresLogin]);

  useEffect(() => () => {
    const attempt = activeConnector.current;
    if (attempt) {
      activeConnector.current = undefined;
      attempt.abort.abort();
      window.clearInterval(attempt.popupCheck);
      if (attempt.popupClosed !== undefined) window.clearTimeout(attempt.popupClosed);
      if (!attempt.popup.closed) attempt.popup.close();
    }
    const mcpAttempt = activeMcp.current;
    if (mcpAttempt) {
      activeMcp.current = undefined;
      mcpAttempt.abort.abort();
      window.clearInterval(mcpAttempt.popupCheck);
      if (mcpAttempt.popupClosed !== undefined) window.clearTimeout(mcpAttempt.popupClosed);
      if (!mcpAttempt.popup.closed) mcpAttempt.popup.close();
    }
  }, []);

  useEffect(() => {
    const onMessage = (event: MessageEvent<unknown>) => {
      const attempt = activeConnector.current;
      if (attempt) {
        const completion = connectorCompletionFor(event, {
          connector: attempt.provider,
          origin: window.location.origin,
          source: attempt.popup,
        });
        if (!completion) return;
        if (completion.result !== "success") {
          if (finishConnectorAttempt(attempt)) {
            setError(completion.message ?? "The account provider did not complete the connection. Try again.");
          }
          return;
        }
        window.clearInterval(attempt.popupCheck);
        if (attempt.popupClosed !== undefined) window.clearTimeout(attempt.popupClosed);
        void refreshConnectors(attempt.abort.signal).then((statuses) => {
          if (activeConnector.current !== attempt) return;
          if (!statuses) {
            throw new Error("Your account session expired. Sign in again and retry the connection.");
          }
          if (!attempt.capabilities.some((capability) => statuses[capability].connected)) {
            throw new Error("The account provider completed without connecting the requested account.");
          }
          finishConnectorAttempt(attempt);
        }).catch((cause) => {
          if (finishConnectorAttempt(attempt)) {
            setError(failureMessage(cause, `Couldn’t connect ${connectorLabel(attempt.provider)}.`));
          }
        });
        return;
      }
      const mcpAttempt = activeMcp.current;
      if (!mcpAttempt) return;
      const completion = connectorCompletionFor(event, {
        connector: mcpCompletionIdentifier(mcpAttempt.connection.id),
        origin: window.location.origin,
        source: mcpAttempt.popup,
      });
      if (!completion) return;
      if (completion.result !== "success") {
        if (finishMcpAttempt(mcpAttempt)) {
          setMcpConnectionError({
            id: mcpAttempt.connection.id,
            message: completion.message ?? "The MCP provider did not complete authorization. Connect again when you are ready.",
          });
        }
        return;
      }
      window.clearInterval(mcpAttempt.popupCheck);
      if (mcpAttempt.popupClosed !== undefined) window.clearTimeout(mcpAttempt.popupClosed);
      void loadMcpConnections().then(() => {
        if (activeMcp.current !== mcpAttempt) return;
        finishMcpAttempt(mcpAttempt);
      }).catch((cause) => {
        if (finishMcpAttempt(mcpAttempt)) {
          setMcpConnectionError({
            id: mcpAttempt.connection.id,
            message: failureMessage(cause, `Couldn’t connect ${mcpAttempt.connection.name}.`),
          });
        }
      });
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [finishConnectorAttempt, finishMcpAttempt, loadMcpConnections, refreshConnectors]);

  useEffect(() => {
    if (!mcpResult || mcpResult.result === "connected") return;
    setMcpConnectionError({
      id: mcpResult.id,
      message: mcpResult.result === "cancelled"
        ? "The MCP authorization was cancelled. Connect again when you are ready."
        : "The MCP provider could not complete authorization. Try connecting again.",
    });
  }, [mcpResult]);

  const connect = async (
    provider: AccountConnectorProvider,
    capabilities: readonly AccountConnectorCapability[],
  ) => {
    if (operation || activeConnector.current) return;
    const popup = window.open(
      "about:blank",
      "nanocodex-account-connector",
      "popup,width=520,height=720",
    );
    if (!popup) {
      setError("The account authorization popup was blocked. Allow popups and try again.");
      return;
    }
    const attempt: ConnectorAttempt = {
      abort: new AbortController(),
      provider,
      capabilities,
      popup,
      popupCheck: window.setInterval(() => {
        if (activeConnector.current !== attempt || !popup.closed) return;
        window.clearInterval(attempt.popupCheck);
        attempt.popupClosed = window.setTimeout(() => {
          if (finishConnectorAttempt(attempt, false)) {
            setError("The account authorization popup was closed before it completed. Connect again when you are ready.");
          }
        }, 750);
      }, 300),
    };
    activeConnector.current = attempt;
    setOperation(provider);
    setError(null);
    try {
      const response = await connectorRequest(`/v1/connectors/${provider}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ return_to: connectorReturnTo() }),
        signal: attempt.abort.signal,
      });
      if (activeConnector.current !== attempt) return;
      if (!response.ok) throw await responseFailure(response, `Couldn’t connect ${connectorLabel(provider)}.`);
      const body: unknown = await response.json();
      if (!isRecord(body) || typeof body.authorization_url !== "string") {
        throw new Error("Invalid connector authorization response.");
      }
      const authorizationUrl = new URL(body.authorization_url);
      if (authorizationUrl.protocol !== "https:") throw new Error("Invalid connector authorization URL.");
      if (popup.closed) throw new Error("The account authorization popup was closed before it started.");
      popup.location.href = authorizationUrl.href;
    } catch (cause) {
      if (finishConnectorAttempt(attempt) && !isAbortError(cause)) {
        setError(failureMessage(cause, `Couldn’t connect ${connectorLabel(provider)}.`));
      }
    }
  };

  const disconnect = async (
    provider: AccountConnectorProvider,
    connection?: ConnectorConnection,
  ) => {
    if (operation) return;
    setOperation(connection?.id ?? provider);
    setError(null);
    try {
      const path = connection
        ? `/v1/connectors/${provider}/connections/${encodeURIComponent(connection.id)}`
        : `/v1/connectors/${provider}`;
      const response = await connectorRequest(path, { method: "DELETE" });
      if (!response.ok) throw await responseFailure(response, `Couldn’t disconnect ${connection?.label ?? connectorLabel(provider)}.`);
      await response.body?.cancel();
      await load();
    } catch (cause) {
      setError(failureMessage(cause, `Couldn’t disconnect ${connection?.label ?? connectorLabel(provider)}.`));
    } finally {
      setOperation(null);
    }
  };

  const disconnectMcp = async (connection: McpConnection) => {
    if (operation || connection.status !== "connected") return;
    setOperation(connection.id);
    setMcpError(null);
    setMcpConnectionError(null);
    try {
      const response = await connectorRequest(
        `/v1/connectors/mcp-connections/${encodeURIComponent(connection.id)}`,
        { method: "DELETE" },
      );
      if (!response.ok) {
        throw await responseFailure(response, `Couldn’t disconnect ${connection.name}.`);
      }
      await response.body?.cancel();
      await loadMcpConnections();
    } catch (cause) {
      setMcpConnectionError({
        id: connection.id,
        message: failureMessage(cause, `Couldn’t disconnect ${connection.name}.`),
      });
    } finally {
      setOperation(null);
    }
  };

  const createMcp = async (target: string): Promise<boolean> => {
    if (operation) return false;
    setOperation("mcp:create");
    setMcpError(null);
    setMcpConnectionError(null);
    try {
      const response = await connectorRequest("/v1/connectors/mcp-connections", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ target }),
      });
      if (!response.ok) throw await responseFailure(response, "Couldn’t add the MCP connection.");
      const body: unknown = await response.json();
      const connection = mcpConnectionFromResponse(body);
      setMcpConnections((current) => [
        connection,
        ...(current ?? []).filter(({ id }) => id !== connection.id),
      ]);
      if (connection.status === "connected") announceAccountMcpCatalogChanged();
      return true;
    } catch (cause) {
      setMcpError(failureMessage(cause, "Couldn’t add the MCP connection."));
      return false;
    } finally {
      setOperation(null);
    }
  };

  const connectMcp = async (connection: McpConnection) => {
    if (operation || activeMcp.current || !mcpConnectionCanAuthorize(connection.status)) return;
    const popup = window.open(
      "about:blank",
      "nanocodex-account-mcp",
      "popup,width=520,height=720",
    );
    if (!popup) {
      setMcpConnectionError({
        id: connection.id,
        message: "The MCP authorization popup was blocked. Allow popups and try again.",
      });
      return;
    }
    const attempt: McpAttempt = {
      abort: new AbortController(),
      connection,
      popup,
      popupCheck: window.setInterval(() => {
        if (activeMcp.current !== attempt || !popup.closed) return;
        window.clearInterval(attempt.popupCheck);
        attempt.popupClosed = window.setTimeout(() => {
          if (finishMcpAttempt(attempt, false)) {
            setMcpConnectionError({
              id: connection.id,
              message: "The MCP authorization popup was closed before it completed. Connect again when you are ready.",
            });
          }
        }, 750);
      }, 300),
    };
    activeMcp.current = attempt;
    setOperation(connection.id);
    setMcpError(null);
    setMcpConnectionError(null);
    try {
      const response = await connectorRequest(
        `/v1/connectors/mcp-connections/${encodeURIComponent(connection.id)}/start`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ return_to: connectorReturnTo() }),
          signal: attempt.abort.signal,
        },
      );
      if (activeMcp.current !== attempt) return;
      if (!response.ok) throw await responseFailure(response, `Couldn’t connect ${connection.name}.`);
      const body: unknown = await response.json();
      const updated = mcpConnectionFromResponse(body, connection.id);
      setMcpConnections((current) => replaceMcpConnection(current ?? [], updated));
      if (updated.status === "connected") {
        announceAccountMcpCatalogChanged();
        finishMcpAttempt(attempt);
        return;
      }
      const authorizationUrl = authorizationUrlFromResponse(body);
      if (popup.closed) throw new Error("The MCP authorization popup was closed before it started.");
      popup.location.href = authorizationUrl.href;
    } catch (cause) {
      if (finishMcpAttempt(attempt) && !isAbortError(cause)) {
        setMcpConnectionError({
          id: connection.id,
          message: failureMessage(cause, `Couldn’t connect ${connection.name}.`),
        });
      }
    }
  };

  if (requiresLogin) {
    if (presentation === "wizard") {
      return (
        <>
          <AccountConnectionGrid>
            {children}
            {connectorDefinitions.map((definition) => (
              <AccountConnectionCard
                action="Connect"
                detail={definition.description}
                disabled
                key={definition.provider}
                logo={<ConnectionLogo id={definition.provider} />}
                onClick={() => undefined}
                title={definition.label}
              />
            ))}
          </AccountConnectionGrid>
          {after}
        </>
      );
    }
    return (
      <div className="profile-connectors connection-grid profile-connectors--locked">
        {children}
        {connectorDefinitions.map((definition) => <button
          className="connection-card connector-row"
          disabled
          key={definition.provider}
          type="button"
        >
          <ConnectionLogo id={definition.provider} />
          <span className="connection-card-copy">
            <strong>{definition.label}</strong>
            <span>{definition.description}</span>
          </span>
          <span className="connection-card-action">Connect</span>
        </button>)}
        {after}
      </div>
    );
  }

  if (presentation === "wizard") {
    return (
      <>
        <AccountConnectionGrid>
          {children}
          {connectors ? connectorDefinitions.map((definition) => {
            const view = connectorProviderView(connectors, definition);
            return <Fragment key={definition.provider}>
              <AccountConnectionCard
                action={view.unavailable ? "Unavailable" : providerConnectAction(definition.provider, view)}
                connected={view.connected}
                detail={view.detail}
                disabled={operation !== null || view.unavailable !== undefined}
                logo={<ConnectionLogo id={definition.provider} />}
                onClick={() => void (view.legacy
                  ? disconnect(definition.provider)
                  : connect(definition.provider, definition.capabilities))}
                title={definition.label}
              />
              {view.connections.map((connection) => <AccountConnectionCard
                action="Revoke"
                connected
                detail={connectorConnectionDetail(definition.provider, connection)}
                disabled={operation !== null}
                key={`${definition.provider}:${connection.id}`}
                logo={<ConnectionLogo id={definition.provider} />}
                onClick={() => void disconnect(definition.provider, connection)}
                title={connection.label}
              />)}
            </Fragment>;
          }) : null}
          {mcpError && !mcpConnections ? <AccountConnectionCard
            action="Retry"
            detail={mcpError}
            disabled={operation !== null}
            logo={<ConnectionLogo id="mcp" />}
            onClick={() => void loadMcpConnections()}
            title="MCP connections"
          /> : null}
          {mcpConnections ? <McpConnectionAddCard
            disabled={operation !== null}
            error={mcpError ?? undefined}
            onSubmit={createMcp}
          /> : null}
          {mcpConnections?.map((connection) => {
            return <McpConnectionCard
              action={mcpConnectionAction(connection.status)}
              actionDisabled={operation !== null}
              connection={connection}
              error={mcpConnectionError?.id === connection.id ? mcpConnectionError.message : undefined}
              key={connection.id}
              onAction={mcpConnectionCanAuthorize(connection.status)
                ? () => void connectMcp(connection)
                : connection.status === "connected" ? () => void disconnectMcp(connection) : undefined}
              presentation="account"
            />;
          })}
        </AccountConnectionGrid>
        {after}
        {result ? (
          <p className={`connector-result connector-result--${result.result}`} role="status">
            {connectorResultMessage(result)}
          </p>
        ) : null}
        {error ? (
          <div className="account-failure" role="alert">
            <p>{error}</p>
            {!connectors ? <button type="button" onClick={() => void load()}>Retry</button> : null}
          </div>
        ) : null}
      </>
    );
  }

  return (
    <div className="profile-connectors connection-grid">
      {children}
      {result ? (
        <p className={`connector-result connector-result--${result.result}`} role="status">
          {connectorResultMessage(result)}
        </p>
      ) : null}
      {error ? (
        <div className="account-failure" role="alert">
          <p>{error}</p>
          {!connectors ? <button type="button" onClick={() => void load()}>Retry</button> : null}
        </div>
      ) : null}
      {connectors ? connectorDefinitions.map((definition) => {
        const view = connectorProviderView(connectors, definition);
        return (<Fragment key={definition.provider}>
          <button
            className={`connection-card connector-row${view.connected ? " is-connected" : ""}${view.unavailable ? " is-unavailable" : ""}`}
            type="button"
            disabled={operation !== null || view.unavailable !== undefined}
            onClick={() => void (view.legacy
              ? disconnect(definition.provider)
              : connect(definition.provider, definition.capabilities))}
          >
            <ConnectionLogo id={definition.provider} />
            <span className="connection-card-copy">
              <strong>{definition.label}</strong>
              <span>{view.detail}</span>
            </span>
            <span className="connection-card-action">
              {view.unavailable ? "Unavailable" : providerConnectAction(definition.provider, view)}
            </span>
          </button>
          {view.connections.map((connection) => <button
            className="connection-card connector-row connector-account-row is-connected"
            disabled={operation !== null}
            key={`${definition.provider}:${connection.id}`}
            onClick={() => void disconnect(definition.provider, connection)}
            type="button"
          >
            <ConnectionLogo id={definition.provider} />
            <span className="connection-card-copy">
              <strong>{connection.label}</strong>
              <span>{connectorConnectionDetail(definition.provider, connection)}</span>
            </span>
            <span className="connection-card-action">Revoke</span>
          </button>)}
        </Fragment>);
      }) : null}
      {mcpConnections ? <McpConnectionAddCard
        disabled={operation !== null}
        error={mcpError ?? undefined}
        listItem={false}
        onSubmit={createMcp}
      /> : mcpError ? (
        <button
          className="connection-card connector-row mcp-connector-row is-unavailable"
          disabled={operation !== null}
          onClick={() => void loadMcpConnections()}
          type="button"
        >
          <ConnectionLogo id="mcp" />
          <span className="connection-card-copy">
            <strong>MCP connections</strong>
            <span>{mcpError}</span>
          </span>
          <span className="connection-card-action">Retry</span>
        </button>
      ) : null}
      {mcpConnections?.map((connection) => (
          <McpConnectionCard
            action={mcpConnectionAction(connection.status)}
            actionDisabled={operation !== null}
            connection={connection}
            error={mcpConnectionError?.id === connection.id ? mcpConnectionError.message : undefined}
            key={connection.id}
            listItem={false}
            onAction={mcpConnectionCanAuthorize(connection.status)
              ? () => void connectMcp(connection)
              : connection.status === "connected" ? () => void disconnectMcp(connection) : undefined}
            presentation="account"
          />
      ))}
      {after}
    </div>
  );
}

async function connectorRequest(path: string, init: RequestInit = {}): Promise<Response> {
  return fetch(path, {
    ...init,
    cache: "no-store",
    credentials: "same-origin",
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}

function decodeConnectorStatus(value: unknown): AccountConnectorStatuses {
  if (!isRecord(value) || !isRecord(value.connectors)) {
    throw new Error("Invalid connector response.");
  }
  const decoded = connectorStatusesFromWire(value.connectors);
  return Object.fromEntries(accountConnectorCapabilities.map((capability) => [
    capability,
    decoded[capability] ?? {
      connected: false,
      connections: [],
    },
  ])) as unknown as AccountConnectorStatuses;
}

function unavailableConnectorStatuses(message: string): AccountConnectorStatuses {
  return Object.fromEntries(accountConnectorCapabilities.map((capability) => [capability, {
    connected: false,
    connections: [],
    unavailable: message,
  }])) as unknown as AccountConnectorStatuses;
}

function decodeMcpConnections(value: unknown): readonly McpConnection[] {
  if (!isRecord(value) || !Array.isArray(value.mcp_connections)
    || value.mcp_connections.length > 64) {
    throw new Error("Invalid MCP connection response.");
  }
  const seen = new Set<string>();
  return value.mcp_connections.map((candidate): McpConnection => {
    const connection = decodeMcpConnection(candidate);
    if (seen.has(connection.id)) throw new Error("Invalid MCP connection response.");
    seen.add(connection.id);
    return connection;
  });
}

function mcpConnectionFromResponse(value: unknown, expectedId?: string): McpConnection {
  if (!isRecord(value)) throw new Error("Invalid MCP connection response.");
  const connection = decodeMcpConnection(value.mcp_connection);
  if (expectedId !== undefined && connection.id !== expectedId) {
    throw new Error("The account broker returned the wrong MCP connection.");
  }
  return connection;
}

function decodeMcpConnection(value: unknown): McpConnection {
  if (!isRecord(value)
    || typeof value.id !== "string" || !mcpConnectionId.test(value.id)
    || typeof value.name !== "string" || !mcpConnectionName.test(value.name)
    || value.name.trim().length === 0
    || typeof value.status !== "string"
    || !mcpConnectionStatuses.has(value.status as McpConnectionStatus)) {
    throw new Error("Invalid MCP connection response.");
  }
  return { id: value.id, name: value.name, status: value.status as McpConnectionStatus };
}

function authorizationUrlFromResponse(value: unknown): URL {
  if (!isRecord(value) || typeof value.authorization_url !== "string") {
    throw new Error("Invalid MCP authorization response.");
  }
  let url: URL;
  try { url = new URL(value.authorization_url); } catch {
    throw new Error("Invalid MCP authorization URL.");
  }
  if (url.protocol !== "https:" || url.username || url.password || url.hash) {
    throw new Error("Invalid MCP authorization URL.");
  }
  return url;
}

function replaceMcpConnection(
  connections: readonly McpConnection[],
  replacement: McpConnection,
): readonly McpConnection[] {
  return connections.map((connection) => connection.id === replacement.id ? replacement : connection);
}

function mcpConnectionCanAuthorize(status: McpConnectionStatus): boolean {
  return status === "authorization_required" || status === "reauthorization_required";
}

function mcpConnectionAction(status: McpConnectionStatus): string | undefined {
  if (status === "connected") return "Revoke";
  if (status === "authorization_required") return "Connect";
  if (status === "reauthorization_required") return "Reconnect";
  return undefined;
}

function connectorReturnTo(): string {
  const url = new URL(window.location.href);
  url.searchParams.delete("connector");
  url.searchParams.delete("connector_result");
  return `${url.pathname}${url.search}`;
}

function readConnectorResult(): { id: AccountConnectorProvider; result: "connected" | "cancelled" | "failed" } | null {
  const url = new URL(window.location.href);
  const id = url.searchParams.get("connector");
  const result = url.searchParams.get("connector_result");
  if (!connectorDefinitions.some((candidate) => candidate.provider === id)
    || (result !== "connected" && result !== "cancelled" && result !== "failed")) return null;
  if (window.opener && window.opener !== window) {
    window.opener.postMessage(connectorCompletion(id as AccountConnectorProvider, result), window.location.origin);
    window.close();
    return null;
  }
  url.searchParams.delete("connector");
  url.searchParams.delete("connector_result");
  window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
  return { id: id as AccountConnectorProvider, result };
}

function readMcpResult(): { id: string; result: "connected" | "cancelled" | "failed" } | null {
  const url = new URL(window.location.href);
  const id = url.searchParams.get("mcp_connection");
  const result = url.searchParams.get("mcp_result");
  if (!id || !mcpConnectionId.test(id)
    || (result !== "connected" && result !== "cancelled" && result !== "failed")) return null;
  if (window.opener && window.opener !== window) {
    window.opener.postMessage(connectorCompletion(mcpCompletionIdentifier(id), result), window.location.origin);
    window.close();
    return null;
  }
  url.searchParams.delete("mcp_connection");
  url.searchParams.delete("mcp_result");
  window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
  return { id, result };
}

function mcpCompletionIdentifier(id: string): string {
  return `mcp:${id}`;
}

function connectorResultMessage(result: NonNullable<ReturnType<typeof readConnectorResult>>): string {
  const label = connectorLabel(result.id);
  if (result.result === "connected") return `${label} connected.`;
  if (result.result === "cancelled") return `${label} authorization was cancelled.`;
  return `${label} couldn’t be connected. Try again.`;
}

function connectorLabel(id: AccountConnectorProvider): string {
  return connectorDefinitions.find((candidate) => candidate.provider === id)!.label;
}

type ConnectorDefinition = typeof connectorDefinitions[number];

function connectorProviderView(
  statuses: AccountConnectorStatuses,
  definition: ConnectorDefinition,
): Readonly<{
  connected: boolean;
  connections: readonly ConnectorConnection[];
  detail: string;
  legacy: boolean;
  unavailable?: string | undefined;
}> {
  const capabilityStatuses = definition.capabilities.map((capability) => statuses[capability]);
  const unavailable = capabilityStatuses.find((status) => status.unavailable)?.unavailable;
  if (unavailable) {
    return { connected: false, connections: [], detail: unavailable, legacy: false, unavailable };
  }
  const connections = connectorConnectionsForCapabilities(statuses, definition.capabilities);
  const connectedCapabilities = definition.capabilities.filter((capability) => statuses[capability].connected);
  const connected = connectedCapabilities.length > 0;
  const legacy = connected && connections.length === 0;
  const legacyLabel = capabilityStatuses.find((status) => status.label || status.account_id);
  const detail = legacy
    ? legacyLabel?.label ?? legacyLabel?.account_id ?? "Connected"
    : connections.length === 0
      ? definition.description
      : definition.provider === "google"
        ? `${connections.length} account${connections.length === 1 ? "" : "s"} · ${connectedCapabilities.map(connectorCapabilityLabel).join(", ")}`
        : definition.provider === "slack"
          ? `${connections.length} workspace identit${connections.length === 1 ? "y" : "ies"} connected`
          : `${connections.length} account${connections.length === 1 ? "" : "s"} connected`;
  return { connected, connections, detail, legacy };
}

function providerConnectAction(
  provider: AccountConnectorProvider,
  view: ReturnType<typeof connectorProviderView>,
): string {
  if (view.legacy) return "Disconnect";
  if (view.connections.length === 0) return "Connect";
  return provider === "slack" ? "Add workspace" : "Add account";
}

function connectorConnectionDetail(
  provider: AccountConnectorProvider,
  connection: ConnectorConnection,
): string {
  if (provider === "google") {
    const capabilities = googleConnectorCapabilities
      .filter((capability) => connection.capabilities.includes(capability))
      .map(connectorCapabilityLabel);
    return capabilities.length ? `Access: ${capabilities.join(", ")}` : "Google Workspace identity";
  }
  if (provider === "slack") return "Slack workspace and user identity";
  return `${connectorLabel(provider)} identity`;
}

function failureMessage(cause: unknown, fallback: string): string {
  return clientFailureMessage(cause, fallback);
}

function isAbortError(cause: unknown): boolean {
  return cause instanceof DOMException && cause.name === "AbortError";
}
