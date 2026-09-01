import { useEffect, useMemo, useRef, useState } from "react";

import {
  ConnectOnboarding,
  type ConnectOnboardingHost,
  type ConnectRequest,
} from "@nanocodex-connect/App";
import {
  deviceApiOrigin,
  deviceUserCode,
  settleDeviceAuthorization,
  type PendingDeviceAuthorization,
} from "./deviceVerification";
import {
  focusedMcpConnection,
  mcpConnectionsFromWire,
} from "@nanocodex-connect/connectPolicy.mjs";
import type { McpConnection } from "@nanocodex-connect/connectTypes";
import { ConnectHome } from "./ConnectHome";
import "@nanocodex-connect/styles.css";
import "./DeviceConnect.css";

export function DeviceConnect() {
  const deviceRequest = new URL(window.location.href).searchParams.has("user_code");
  if (!deviceRequest) return <ConnectHome />;
  return <DeviceAuthorization />;
}

function DeviceAuthorization() {
  const pending = useRef<PendingDeviceAuthorization | undefined>(undefined);
  const connectorName = useRef<string | undefined>(undefined);
  const settlement = useRef<Promise<void> | undefined>(undefined);
  const [request, setRequest] = useState<ConnectRequest>();

  useEffect(() => {
    const abort = new AbortController();
    void loadPendingDeviceRequest(new URL(window.location.href), abort.signal).then(
      (loaded) => {
        pending.current = loaded.pending;
        connectorName.current = focusedRequestName(
          loaded.pending.request,
          loaded.requestedMcpConnections,
          loaded.focusMcpConnection,
        );
        setRequest(Object.freeze({
          appId: "nanocodex-cli",
          confirmationCode: loaded.pending.userCode,
          focusMcpConnection: loaded.focusMcpConnection,
          id: `device:${String(loaded.pending.request.id)}:${loaded.pending.userCode}`,
          origin: "https://cli.nanocodex.xyz",
          requestedMcpConnections: loaded.requestedMcpConnections,
          returnedConnector: loaded.returnedConnector,
          returnedConnectorResult: loaded.returnedConnectorResult,
          returnedMcpConnection: loaded.returnedMcpConnection,
          returnedMcpResult: loaded.returnedMcpResult,
          rpc: loaded.pending.request,
          type: "walletConnect",
        }));
      },
      (error: unknown) => {
        if (abort.signal.aborted) return;
        setRequest(Object.freeze({
          id: "device-error",
          message: errorMessage(error),
          type: "deviceError",
        }));
      },
    );
    return () => abort.abort();
  }, []);

  const host = useMemo<ConnectOnboardingHost>(() => ({
    async reject() {
      if (settlement.current) return settlement.current;
      const current = pending.current;
      if (!current) throw new Error("The device authorization is unavailable.");
      let operation!: Promise<void>;
      operation = (async () => {
        await settleDeviceAuthorization(current, "deny");
        pending.current = undefined;
        setRequest(Object.freeze({
          connectorName: connectorName.current,
          id: "device-denied",
          status: "denied",
          type: "deviceComplete",
        }));
      })().finally(() => {
        if (settlement.current === operation && pending.current) settlement.current = undefined;
      });
      settlement.current = operation;
      return operation;
    },
    async respond(result: unknown) {
      if (settlement.current) return settlement.current;
      const current = pending.current;
      if (!current) throw new Error("The device authorization is unavailable.");
      let operation!: Promise<void>;
      operation = (async () => {
        await settleDeviceAuthorization(current, "approve", result);
        pending.current = undefined;
        if (!connectorName.current) {
          setRequest(Object.freeze({
            id: "device-approved",
            status: "approved",
            type: "deviceComplete",
          }));
        }
      })().finally(() => {
        if (settlement.current === operation && pending.current) settlement.current = undefined;
      });
      settlement.current = operation;
      return operation;
    },
  }), []);

  if (!request) return null;
  return (
    <div className="device-connect-route" data-testid="device-connect-route">
      <ConnectOnboarding host={host} presentation="wizard" request={request} />
    </div>
  );
}

async function loadPendingDeviceRequest(url: URL, signal: AbortSignal): Promise<Readonly<{
  pending: PendingDeviceAuthorization;
  requestedMcpConnections: readonly McpConnection[];
  focusMcpConnection?: string | undefined;
  returnedConnector?: "github" | "gmail" | "gdrive" | "x" | "slack" | undefined;
  returnedConnectorResult?: "connected" | "cancelled" | "failed" | undefined;
  returnedMcpConnection?: string | undefined;
  returnedMcpResult?: "connected" | "cancelled" | "failed" | undefined;
}>> {
  const userCode = deviceUserCode(singleParameter(url, "user_code"));
  const apiOrigin = deviceApiOrigin(singleParameter(url, "api_origin"), url.origin);
  const response = await fetch(
    `${apiOrigin}/v1/device/verify?user_code=${encodeURIComponent(userCode)}`,
    { cache: "no-store", headers: { accept: "application/json" }, signal },
  );
  const body = await response.json().catch(() => undefined) as unknown;
  if (!response.ok || !isPendingDeviceResponse(body, userCode)) {
    throw new Error(deviceApiError(body));
  }
  const requestedMcpConnections = body.requested_mcp_connections === undefined
    ? mcpConnectionsFromWire([])
    : mcpConnectionsFromWire(body.requested_mcp_connections);
  const focusMcp = focusedMcpConnection(body.focus_mcp_connection, requestedMcpConnections);
  const focusConnector = focusedRequestConnector(body.request);
  const returnedConnector = optionalSingleParameter(url, "connector");
  const returnedConnectorResult = optionalSingleParameter(url, "connector_result");
  if (returnedConnector !== null
    && (returnedConnector !== focusConnector || returnedConnector === "chatgpt")) {
    throw new Error("The connector callback is invalid.");
  }
  if ((returnedConnector === null) !== (returnedConnectorResult === null)
    || (returnedConnectorResult !== null
      && !["connected", "cancelled", "failed"].includes(returnedConnectorResult))) {
    throw new Error("The connector callback result is invalid.");
  }
  const returned = optionalSingleParameter(url, "mcp_connection");
  const returnedMcp = returned === null
    ? undefined
    : focusedMcpConnection(returned, requestedMcpConnections);
  const returnedResult = optionalSingleParameter(url, "mcp_result");
  if ((returnedMcp === undefined) !== (returnedResult === null)
    || (returnedResult !== null && !["connected", "cancelled", "failed"].includes(returnedResult))
    || (returnedConnector !== null && returnedMcp !== undefined)) {
    throw new Error("The MCP callback result is invalid.");
  }
  return Object.freeze({
    pending: Object.freeze({ apiOrigin, request: body.request, userCode }),
    requestedMcpConnections,
    ...(focusMcp ? { focusMcpConnection: focusMcp } : {}),
    ...(returnedConnector ? {
      returnedConnector: returnedConnector as "github" | "gmail" | "gdrive" | "x" | "slack",
      returnedConnectorResult: returnedConnectorResult as "connected" | "cancelled" | "failed",
    } : {}),
    ...(returnedMcp ? { returnedMcpConnection: returnedMcp } : {}),
    ...(returnedResult ? {
      returnedMcpResult: returnedResult as "connected" | "cancelled" | "failed",
    } : {}),
  });
}

function focusedRequestConnector(
  request: PendingDeviceAuthorization["request"],
): "chatgpt" | "github" | "gmail" | "gdrive" | "x" | "slack" | undefined {
  const params = Array.isArray(request.params) ? request.params[0] : undefined;
  if (!isRecord(params) || !isRecord(params.capabilities) || !isRecord(params.capabilities.auth)) {
    return undefined;
  }
  const resources = params.capabilities.auth.resources;
  if (!Array.isArray(resources)) return undefined;
  const focused = resources.flatMap((resource) => typeof resource === "string"
    && resource.startsWith("urn:nanocodex:connector-focus:")
      ? [resource.slice("urn:nanocodex:connector-focus:".length)]
      : []);
  if (focused.length !== 1
    || !["chatgpt", "github", "gmail", "gdrive", "x", "slack"].includes(focused[0]!)) return undefined;
  return focused[0] as "chatgpt" | "github" | "gmail" | "gdrive" | "x" | "slack";
}

function focusedRequestName(
  request: PendingDeviceAuthorization["request"],
  mcpConnections: readonly McpConnection[],
  focusMcpConnection?: string,
): string | undefined {
  if (focusMcpConnection) {
    return mcpConnections.find(({ id }) => id === focusMcpConnection)?.name;
  }
  const params = Array.isArray(request.params) ? request.params[0] : undefined;
  if (!isRecord(params) || !isRecord(params.capabilities) || !isRecord(params.capabilities.auth)) {
    return undefined;
  }
  const resources = params.capabilities.auth.resources;
  if (!Array.isArray(resources)) return undefined;
  const focused = resources.flatMap((resource) => typeof resource === "string"
    && resource.startsWith("urn:nanocodex:connector-focus:")
      ? [resource.slice("urn:nanocodex:connector-focus:".length)]
      : []);
  if (focused.length !== 1) return undefined;
  if (focused[0] === "chatgpt") return "ChatGPT";
  if (focused[0] === "github") return "GitHub";
  if (focused[0] === "gmail") return "Gmail";
  if (focused[0] === "gdrive") return "Google Drive";
  if (focused[0] === "x") return "X";
  return undefined;
}

function isPendingDeviceResponse(value: unknown, userCode: string): value is Readonly<{
  app: Readonly<{ id: string; name: string; origin: string }>;
  focus_mcp_connection?: unknown;
  request: PendingDeviceAuthorization["request"];
  requested_mcp_connections?: unknown;
  user_code: string;
}> {
  if (!isRecord(value) || value.user_code !== userCode
    || !isRecord(value.app) || !isRecord(value.request)) return false;
  return value.app.id === "nanocodex-cli"
    && value.app.name === "Nanocodex CLI"
    && value.app.origin === "https://cli.nanocodex.xyz"
    && Object.keys(value.app).every((key) => key === "id" || key === "name" || key === "origin")
    && value.request.jsonrpc === "2.0"
    && (typeof value.request.id === "string" || typeof value.request.id === "number")
    && value.request.method === "wallet_connect";
}

function deviceApiError(value: unknown): string {
  if (isRecord(value)) {
    if (typeof value.error_description === "string") return value.error_description;
    if (typeof value.error === "string") return value.error;
  }
  return "The device authorization is unavailable or expired.";
}

function singleParameter(url: URL, name: string): string | null {
  const values = url.searchParams.getAll(name);
  return values.length === 1 ? values[0]! : null;
}

function optionalSingleParameter(url: URL, name: string): string | null {
  const values = url.searchParams.getAll(name);
  if (values.length > 1) throw new Error(`The ${name} callback parameter is invalid.`);
  return values[0] ?? null;
}

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return "The device authorization is unavailable or expired.";
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
