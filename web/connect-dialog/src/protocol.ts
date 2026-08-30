import type { Dialog } from "nanocodex/connect";
import { Wata, postMessage } from "wata/host";
import { mcpConnectionsFromWire, registeredApp } from "./connectPolicy.mjs";
import type { ConnectRequest, WalletRequest } from "./connectTypes";

export type Request = Exclude<ConnectRequest, { type: "deviceError" | "deviceComplete" }>;

type WalletEvent = Readonly<{
  request: WalletRpc;
  respond(result: unknown): Promise<unknown>;
  reject(error: Readonly<{ code: number; message: string }>): Promise<unknown>;
}>;

type WalletRpc = WalletRequest["rpc"] & Readonly<{ context?: unknown }>;

type WalletHostActions = Readonly<{
  logout(): Promise<void> | void;
}>;

const listeners = new Set<() => void>();
let snapshot: Request | undefined;
let walletEvent: WalletEvent | undefined;
let dialogParent: Readonly<{ id: string; origin: string; source: Window }> | undefined;
let executionCompletion: Readonly<{
  reject(error: Error): void;
  resolve(): void;
}> | undefined;
let started = false;

export const parentDialog = Object.freeze({
  getRequest() {
    return snapshot;
  },
  subscribe(listener: () => void) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  async respond(result: unknown) {
    if (walletEvent) {
      const event = walletEvent;
      settle();
      await event.respond(result);
      return;
    }
    const current = dialogParent;
    if (!current) throw new Error("The Nanocodex dialog has no pending request");
    settle();
    current.source.postMessage({ type: "nanocodex:response", id: current.id, result }, current.origin);
  },
  async reject(error?: unknown) {
    const message = errorMessage(error);
    if (walletEvent) {
      const event = walletEvent;
      settle();
      await event.reject({ code: 4001, message });
      return;
    }
    const current = dialogParent;
    if (!current) throw new Error("The Nanocodex dialog has no pending request");
    settle();
    current.source.postMessage({
      type: "nanocodex:response",
      id: current.id,
      error: { message },
    }, current.origin);
  },
  async approve() {
    const current = dialogParent;
    if (!current || snapshot?.type !== "webMcpApproval" || executionCompletion) {
      throw new Error("The Nanocodex dialog has no pending WebMCP approval");
    }
    return new Promise<void>((resolve, reject) => {
      executionCompletion = Object.freeze({ reject, resolve });
      current.source.postMessage({
        type: "nanocodex:approval",
        id: current.id,
      }, current.origin);
    });
  },
});

export function startWalletHost(actions: WalletHostActions) {
  if (started) return;
  started = true;
  const url = new URL(window.location.href);
  const origin = parseOrigin(singleParameter(url, "origin"));
  const appId = parseAppId(singleParameter(url, "app_id"));
  if (!origin || !appId) return;
  const nonConnectApp = (() => {
    try {
      return registeredApp(origin, appId, url.href, window.parent === window, false);
    } catch {
      return undefined;
    }
  })();
  const wata = Wata.create({ transports: [postMessage({ targetOrigin: origin })] });
  const session = wata.start();
  session.onRequest((event) => {
    if (event.request.method === "wallet_disconnect") {
      if (!nonConnectApp) {
        void event.reject({ code: -32601, message: "Nanocodex Connect only accepts connection requests from this popup application." });
        return;
      }
      if (snapshot) {
        void event.reject({ code: -32002, message: "Nanocodex Connect already has a pending request." });
        return;
      }
      void Promise.resolve()
        .then(() => actions.logout())
        .then(() => event.respond(undefined))
        .catch((error) => event.reject({ code: -32603, message: errorMessage(error) }));
      return;
    }
    const type = event.request.method === "wallet_connect"
      ? "walletConnect"
      : event.request.method === "wallet_revokeAccessKey"
        ? "walletRevokeAccessKey"
        : undefined;
    if (snapshot || !type) {
      void event.reject({ code: -32601, message: "Nanocodex Connect only accepts connection, logout, and access-key revocation requests here." });
      return;
    }
    try {
      const request = projectWalletRequest({
        appId,
        id: crypto.randomUUID(),
        origin,
        rpc: event.request,
        type,
      });
      walletEvent = event as unknown as WalletEvent;
      publish(request);
    } catch (error) {
      void event.reject({ code: -32600, message: errorMessage(error) });
    }
  });
  session.onNotification((event: Readonly<{ method: string }>) => {
    if (event.method === "cancel" && walletEvent) void parentDialog.reject();
  });
}

window.addEventListener("message", (event: MessageEvent<unknown>) => {
  if (window.parent === window || event.source !== window.parent || event.origin === "null") return;
  if (isCompletionMessage(event.data)
    && dialogParent?.id === event.data.id
    && dialogParent.origin === event.origin) {
    const completion = executionCompletion;
    executionCompletion = undefined;
    if (!completion) return;
    if (event.data.ok) completion.resolve();
    else completion.reject(new Error(event.data.error?.message ?? "The action failed."));
    return;
  }
  if (snapshot !== undefined || !isDialogMessage(event.data, event.origin)) return;
  dialogParent = Object.freeze({ id: event.data.id, origin: event.origin, source: window.parent });
  publish(Object.freeze(event.data.request));
});

function publish(request: Request | undefined) {
  snapshot = request;
  for (const listener of [...listeners]) listener();
}

function settle() {
  walletEvent = undefined;
  dialogParent = undefined;
  executionCompletion = undefined;
  publish(undefined);
}

export function projectWalletRequest(value: Readonly<{
  appId: string;
  id: string;
  origin: string;
  rpc: WalletRpc;
  type: WalletRequest["type"];
}>): WalletRequest {
  const metadata = walletRequestMetadata(value.rpc.context);
  if (value.type !== "walletConnect" && Object.keys(metadata).length > 0) {
    throw new Error("Nanocodex Connect received unexpected MCP request metadata.");
  }
  return Object.freeze({
    appId: value.appId,
    id: value.id,
    origin: value.origin,
    rpc: Object.freeze({
      method: value.rpc.method,
      ...(value.rpc.params === undefined ? {} : { params: value.rpc.params }),
    }),
    type: value.type,
    ...metadata,
  });
}

function walletRequestMetadata(value: unknown): Readonly<{
  requestedMcpConnections?: WalletRequest["requestedMcpConnections"];
  focusMcpConnection?: string;
}> {
  if (value === undefined) return Object.freeze({});
  if (!isRecord(value)
    || Object.keys(value).some((key) => key !== "requestedMcpConnections" && key !== "focusMcpConnection")) {
    throw new Error("Nanocodex Connect received invalid MCP request metadata.");
  }
  if (value.requestedMcpConnections === undefined) {
    if (value.focusMcpConnection !== undefined) {
      throw new Error("The focused MCP connection is invalid.");
    }
    return Object.freeze({});
  }
  const connections = mcpConnectionsFromWire(value.requestedMcpConnections);
  if (connections.length > 16
    || connections.some(({ status }) => status !== "authorization_required")) {
    throw new Error("Nanocodex Connect received invalid MCP request metadata.");
  }
  const focus = value.focusMcpConnection;
  if (focus !== undefined
    && (typeof focus !== "string" || !connections.some(({ id }) => id === focus))) {
    throw new Error("The focused MCP connection is invalid.");
  }
  return Object.freeze({
    requestedMcpConnections: connections,
    ...(focus === undefined ? {} : { focusMcpConnection: focus }),
  });
}


function parseOrigin(value: string | null) {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    if (url.origin === value) return url.origin;
    return url.protocol === "chrome-extension:" && url.href === value && /^[a-p]{32}$/.test(url.hostname)
      ? value
      : undefined;
  } catch {
    return undefined;
  }
}

function parseAppId(value: string | null) {
  return value && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value) ? value : undefined;
}

function singleParameter(url: URL, name: string): string | null {
  const values = url.searchParams.getAll(name);
  return values.length === 1 ? values[0]! : null;
}

function isDialogMessage(value: unknown, origin: string): value is Readonly<{
  type: "nanocodex:request";
  id: string;
  request: Dialog.FundingRequest | Dialog.WebMcpApprovalRequest;
}> {
  if (!isRecord(value) || value.type !== "nanocodex:request" || typeof value.id !== "string") return false;
  if (!isRecord(value.request) || value.request.id !== value.id) return false;
  if (value.request.type === "machineUsdFund") return true;
  if (value.request.type !== "webMcpApproval"
    || !isRecord(value.request.app)
    || value.request.app.origin !== origin
    || typeof value.request.app.id !== "string"
    || typeof value.request.app.name !== "string"
    || !isRecord(value.request.action)
    || value.request.action.readOnly !== false
    || (value.request.action.kind !== "webmcp" && value.request.action.kind !== "semantic")
    || typeof value.request.action.name !== "string"
    || !value.request.action.name) return false;
  return true;
}

function isCompletionMessage(value: unknown): value is Readonly<{
  type: "nanocodex:completion";
  id: string;
  ok: boolean;
  error?: Readonly<{ message?: string }>;
}> {
  return isRecord(value)
    && value.type === "nanocodex:completion"
    && typeof value.id === "string"
    && typeof value.ok === "boolean";
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null;
}

function errorMessage(error: unknown) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error.length > 0) return error;
  return "The request was not approved.";
}
